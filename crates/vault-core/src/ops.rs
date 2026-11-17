//! ツールの本体：vault_info・vault_list・vault_read・vault_write・vault_mkdir・vault_move。
//!
//! - 返り値とエラーの文には、保管庫の中の相対パスだけを入れる（根の絶対パスは入れない）。
//! - 書く処理は、ロックを1回だけ取り、そのあとはロックを持っている印（`&VaultLock`）を渡して進める。
//!   ロックを持ったまま、`lock()` を呼ぶ別の公開の関数を呼ばない（入れ子にすると 10 秒待って `LOCKED` になる）。
//! - 書く処理は、1回の呼び出しを1つの `Txn` で進める：最初に履歴のファイルを開いて seq を決め（書けることを確かめる）、
//!   起きたことを履歴と索引の変更としてため、最後に履歴を1回の sync で書き、索引を1回の読み書きで直す。
//!   変更のあとで履歴に書けなければ、エラーの文に「済んでいる」ことを入れる。索引だけが直せないときは成功にする。
//! - 読む処理はロックを取らない（書き込みは一時ファイルからの rename なので）。

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::base64;
use crate::error::{ErrorCode, Result, VaultError};
use crate::fsutil::{EntryKind, FileInfo, access_denied};
use crate::guard::{
    self, At, ChildDir, ChildInfo, DirStep, Held, LeafRead, LeafState, ListDir, Moved, Pass,
    Placing, Root, Target, TargetOpen, TargetUse, Trashed, parent_of,
};
use crate::index::{self, Index, IndexChange, IndexEntry};
use crate::journal::{JournalWriter, Op, Record};
use crate::relpath::{RESERVED_NAME, RelPath, is_valid_component};
use crate::root::is_not_found;
use crate::trash::{self, ToTrash, TrashReason};
use crate::vault::{Extent, Vault, VaultLock};

/// vault_write の中身の上限。
pub const MAX_WRITE_BYTES: usize = 16 * 1024 * 1024;
/// base64 の中身の、文字の数の上限（16 MiB を base64 にした長さ。これを超えたら、広げる前に `TOO_LARGE`）。
pub const MAX_WRITE_BASE64_CHARS: usize = 4 * MAX_WRITE_BYTES.div_ceil(3);
/// vault_read の max_bytes の上限。
pub const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;
/// vault_read の max_bytes の既定値。
pub const DEFAULT_READ_BYTES: u64 = 256 * 1024;
/// vault_list の limit の既定値。
pub const DEFAULT_LIST_LIMIT: u64 = 1000;
/// vault_list の limit の上限。
pub const MAX_LIST_LIMIT: u64 = 10_000;

/// 中身の書き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    /// UTF-8 の文字。
    #[default]
    Utf8,
    /// base64（RFC 4648 の標準の形）。
    Base64,
}

/// vault_info の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InfoResult {
    /// 保管庫のフォルダの名前だけ（絶対パスは出さない）。
    pub name: String,
    pub vault_id: String,
    pub format: u32,
    pub read_only: bool,
}

/// vault_list の1項目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListEntry {
    pub path: String,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: String,
    pub version: String,
}

/// vault_list の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListResult {
    pub entries: Vec<ListEntry>,
    pub truncated: bool,
    /// 上限（64段・根を含めて260字）を超えるので出さなかった項目の数。その中は数えない
    /// （そうした木は、上限の中にある祖先のフォルダを vault_delete すれば、まとめてゴミ箱へ移せる）。
    pub skipped_over_limit: u64,
    /// ツールで扱えない名前（UTF-8 にできない・文字列の規則に通らない：使えない文字・予約名・`~` と数字・
    /// 255 バイトを超えるなど）なので出さなかった項目の数。その中は数えない。読んだフォルダの中の数。
    pub skipped_invalid_names: u64,
}

/// vault_read の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadResult {
    pub path: String,
    pub content: String,
    pub encoding: Encoding,
    pub size: u64,
    pub version: String,
    pub truncated: bool,
}

/// vault_write の引数。
#[derive(Debug, Clone, Copy)]
pub struct WriteRequest<'a> {
    pub path: &'a str,
    pub content: &'a str,
    pub encoding: Encoding,
    pub overwrite: bool,
    pub create_dirs: bool,
    pub expected_version: Option<&'a str>,
}

/// vault_write の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteResult {
    pub path: String,
    pub size: u64,
    pub version: String,
    pub created: bool,
    pub previous_trash_id: Option<String>,
}

/// vault_mkdir の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MkdirResult {
    pub path: String,
    pub created: bool,
}

/// vault_move の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveResult {
    pub from: String,
    pub to: String,
}

/// 1回の書くツールの呼び出し（履歴と索引の変更をためて、最後に1回で書く）。
///
/// その操作の根（`root`）と `.vault`（`vault`）のハンドルを、終わるまで持つ（どちらも名前を変えられない）。
pub(crate) struct Txn<'a> {
    pub(crate) root: Root,
    pub(crate) vault: Pass,
    pub(crate) lock: &'a VaultLock,
    journal: JournalWriter<'a>,
    index: Vec<IndexChange>,
    /// 呼び出しの途中で読んだ索引（読んでいなければ None。読めなかったら Some(None)）。最後に当てるときに使い回す。
    loaded: Option<Option<Index>>,
}

impl<'a> Txn<'a> {
    /// 始める：根を開き直し、履歴のファイルを開いて seq を決める（変更の前に、履歴に書けることを確かめる）。
    pub(crate) fn begin(vault: &Vault, lock: &'a VaultLock) -> Result<Self> {
        let root = vault.op_root()?;
        let vault_handle = root.open_vault_dir()?;
        let journal = JournalWriter::open(&root, &vault_handle, lock)?;
        Ok(Self {
            root,
            vault: vault_handle,
            lock,
            journal,
            index: Vec::new(),
            loaded: None,
        })
    }

    /// 索引に `path` があるか（索引が読めなければ true。そのときは索引を直さないので、どちらでも同じ）。
    pub(crate) fn index_contains(&mut self, path: &str) -> bool {
        self.loaded
            .get_or_insert_with(|| index::load(&self.vault))
            .as_ref()
            .is_none_or(|index| index.entries().contains_key(path))
    }

    pub(crate) fn record(&mut self, actor: &Actor, op: Op, record: Record) -> Result<()> {
        self.journal.push(actor, op, record).map(|_| ())
    }

    pub(crate) fn index(&mut self, change: IndexChange) {
        self.index.push(change);
    }

    /// 終える。起きたことを履歴に書き（1回の sync）、索引を直す（1回の読み書き。直せなくても成功のまま）。
    /// 成功の結果で履歴に書けなかったときは、`done` の文（変更は済んでいること）を添えたエラーにする。
    pub(crate) fn finish<T>(self, result: Result<T>, done: impl FnOnce(&T) -> String) -> Result<T> {
        let Self {
            root,
            vault,
            lock,
            journal,
            index,
            loaded,
            ..
        } = self;
        let committed = journal.commit();
        index::apply(&root, &vault, lock, loaded, index);
        match (result, committed) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(value), Err(e)) => {
                Err(e.with_note(&format!("{}。ただし、履歴には残せなかった", done(&value))))
            }
            (Err(e), _) => Err(e),
        }
    }

    /// 走査を終える：履歴に書き（1回の sync）、書けたときだけ、索引を `replacement`（書く形にしたもの）に
    /// 置き換える（1回の書き込み）。履歴に書けなかったときは、索引を古いままにして、次の走査で同じ変更を
    /// もう一度見つけられるようにする。
    pub(crate) fn finish_replacing_index(self, replacement: Option<&[u8]>) -> Result<()> {
        let Self {
            root,
            vault,
            lock,
            journal,
            index,
            ..
        } = self;
        debug_assert!(index.is_empty(), "走査は、索引の変更をためない");
        journal.commit()?;
        if let Some(replacement) = replacement {
            index::replace_encoded(&root, &vault, lock, replacement);
        }
        Ok(())
    }
}

impl Vault {
    /// vault_info：名前・vault_id・format・読み取り専用か。
    pub fn info(&self) -> InfoResult {
        InfoResult {
            name: self.name(),
            vault_id: self.vault_id().to_owned(),
            format: self.format(),
            read_only: self.is_read_only(),
        }
    }

    /// vault_list：フォルダの中の一覧。フォルダを先に、名前の順。
    ///
    /// `recursive` のときは、フォルダのすぐあとにその中身を並べる（深さ優先・繰り返しで書く）。
    /// どの深さでも `.vault` には入らず、リンクはたどらない（`kind:"link"` で、`symlink_metadata` の値だけを返す）。
    /// ほかのアプリで作られた、上限（64段・根を含めて260字）を超えるパスは出さず、`skipped_over_limit` に数だけを返す。
    /// ツールで扱えない名前（文字列の規則に通らない）も出さず、`skipped_invalid_names` に数だけを返す。
    pub fn list(&self, path: &str, recursive: bool, limit: u64) -> Result<ListResult> {
        if !(1..=MAX_LIST_LIMIT).contains(&limit) {
            return Err(invalid_argument(format!(
                "limit は 1 から {MAX_LIST_LIMIT} まで"
            )));
        }
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let rel = RelPath::parse(path)?;
        // 根を先に開き直して持つ（read と同じ順。根が差し替えられていれば、パスで門を通す前に NOT_A_VAULT。
        // 差し替えた先の名前の有無・種類がエラーのコードで分からない）
        let root = self.op_root()?;
        // 1つめの門（今のエラーのコードと文のため）
        let dir = self.resolve_rel(&rel)?;
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(not_a_directory(&rel)),
            Err(e) if is_not_found(&e) => return Err(not_found(&rel)),
            Err(e) => return Err(VaultError::io("フォルダを調べられない", &e)),
        }
        test_hook!(Step::AfterGate);

        // A：根から1段ずつ開き、一覧するフォルダは HOLD_DIR で持つ（最終のパスを確かめる）。途中のフォルダと根は、
        // 一覧を終えるまで持つ（名前を変えられない）
        let (_chain, held, _root) = if rel.is_root() {
            (Vec::new(), root.into_held(), None)
        } else {
            let (chain, held) = open_list_dir(&root, &rel)?;
            (chain, held, Some(root))
        };
        test_hook!(Step::BeforeList);

        let mut entries = Vec::new();
        let mut truncated = false;
        let mut skipped_over_limit = 0u64;
        let mut skipped_invalid_names = 0u64;
        let mut held = held;
        let (first, invalid) = read_children(&mut held, &rel.to_string())?;
        skipped_invalid_names += invalid;
        // E：持っているフォルダのハンドルで列挙する。下のフォルダは、持っている親を基準に開く
        let mut stack: Vec<(Held, std::vec::IntoIter<ListEntry>)> = vec![(held, first.into_iter())];
        while let Some((dir, top)) = stack.last_mut() {
            let Some(entry) = top.next() else {
                stack.pop();
                continue;
            };
            if !self.fits(&entry.path) {
                skipped_over_limit += 1;
                continue;
            }
            if entries.len() >= limit {
                truncated = true;
                break;
            }
            if !(recursive && entry.kind == EntryKind::Dir) {
                entries.push(entry);
                continue;
            }
            test_hook!(Step::BeforeChildOpen);
            match dir.open_child_dir(OsStr::new(&entry.name), true) {
                ChildDir::Dir(mut sub) => {
                    let (children, invalid) = read_children(&mut sub, &entry.path)?;
                    skipped_invalid_names += invalid;
                    entries.push(entry);
                    stack.push((sub, children.into_iter()));
                }
                // リンクなどに替わっていた：その値に直して並べ、中へは進まない（例外）
                ChildDir::Other(info) => entries.push(ListEntry {
                    kind: info.kind,
                    size: info.size,
                    modified: info.modified(),
                    version: info.version(),
                    ..entry
                }),
                // 開く前に消えた：並べない
                ChildDir::Missing => {}
                ChildDir::Failed(e) => return Err(e),
            }
        }
        Ok(ListResult {
            entries,
            truncated,
            skipped_over_limit,
            skipped_invalid_names,
        })
    }

    /// vault_read：ファイルを読む。`offset` から最大 `max_bytes` バイト。
    ///
    /// 根から1段ずつ、持っている親のハンドルを基準に、リンクをたどらずに開く（途中のフォルダは通り抜けるだけの形）。
    /// 開いたハンドルで、種類と、最終のパスが読む対象の場所であることを確かめ、同じハンドルで読む。
    /// utf8 で、`max_bytes` の切れ目が文字の途中なら、完全な文字の分だけを返す（`truncated:true`）。
    /// `max_bytes` が1文字より小さくて1文字も返せないときは、1文字分（最大4バイト）だけを読んで返す
    /// （offset を進められるように。`max_bytes` が 0 のときは何も読まない）。
    pub fn read(
        &self,
        path: &str,
        encoding: Encoding,
        offset: u64,
        max_bytes: u64,
    ) -> Result<ReadResult> {
        if max_bytes > MAX_READ_BYTES {
            return Err(VaultError::new(
                ErrorCode::TooLarge,
                format!("max_bytes は {MAX_READ_BYTES} バイト（16 MiB）まで"),
            ));
        }
        let rel = RelPath::parse(path)?;
        if rel.is_root() {
            return Err(not_a_file(&rel));
        }
        let root = self.op_root()?;
        // 1つめの門（今のエラーのコードと文のため）
        let abs = self.resolve_rel(&rel)?;
        match fs::symlink_metadata(&abs) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(outside_link()),
            Ok(meta) if meta.is_dir() => return Err(not_a_file(&rel)),
            Ok(_) => {}
            Err(e) if is_not_found(&e) => return Err(not_found(&rel)),
            Err(e) => return Err(VaultError::io("ファイルを調べられない", &e)),
        }
        test_hook!(Step::AfterGate);
        let mut file = open_file_to_read(&root, &rel)?;
        let meta = file
            .metadata()
            .map_err(|e| VaultError::io("ファイルを調べられない", &e))?;
        if meta.file_type().is_symlink() {
            return Err(outside_link());
        }
        if !meta.is_file() {
            return Err(not_a_file(&rel));
        }
        let info = FileInfo::from_metadata(&meta);

        let mut buf = Vec::new();
        if offset < info.size {
            read_at(&mut file, offset, max_bytes, &mut buf)?;
        }
        let mut truncated = offset.saturating_add(buf.len() as u64) < info.size;
        let content = match encoding {
            Encoding::Base64 => base64::encode(&buf),
            Encoding::Utf8 => match String::from_utf8(buf) {
                Ok(text) => text,
                Err(e) => {
                    let err = e.utf8_error();
                    if !(truncated && err.error_len().is_none()) {
                        return Err(not_utf8(&rel));
                    }
                    if err.valid_up_to() > 0 {
                        // max_bytes の切れ目が文字の途中：完全な文字の分だけを返す
                        let mut bytes = e.into_bytes();
                        bytes.truncate(err.valid_up_to());
                        String::from_utf8(bytes).map_err(|_| not_utf8(&rel))?
                    } else {
                        // 1文字も返せない：1文字分（最大4バイト）だけを読む
                        let first = e.as_bytes()[0];
                        let need = utf8_char_len(first).ok_or_else(|| not_utf8(&rel))?;
                        let mut one = Vec::new();
                        read_at(&mut file, offset, need as u64, &mut one)?;
                        if one.len() < need {
                            return Err(not_utf8(&rel));
                        }
                        truncated = offset.saturating_add(need as u64) < info.size;
                        String::from_utf8(one).map_err(|_| not_utf8(&rel))?
                    }
                }
            },
        };
        Ok(ReadResult {
            path: rel.to_string(),
            content,
            encoding,
            size: info.size,
            version: info.version(),
            truncated,
        })
    }

    /// vault_write：ファイルを書く（ロックは最初から最後まで持つ）。
    ///
    /// 0. 1つめの門（パス）→ 空き容量（書いたあとに 1 GiB 残るか）→ 親のフォルダを根から1段ずつハンドルで開く・作る →
    ///    上書きなら、2つめの門（今あるものを開き直し、門の値と比べる。違えば `CONFLICT`）
    /// 1. `.vault/tmp` に一時ファイルを作り、最終のパスを確かめてから書いて `sync_all`（容量が足りなければ `DISK_FULL`）
    /// 2. 上書きなら、古い中身をゴミ箱へ（reason `overwrite`）
    /// 3. 置き換えない rename で置く（書く先に別のものができていたら `CONFLICT`。失敗したら、古い中身をゴミ箱から
    ///    もとに戻す。戻せなければ、エラーの文と履歴（`write_failed`）に、古い中身の居場所（previous_trash_id）を残す）
    /// 4. 履歴に追記する  5. 索引を直す
    ///
    /// 16 MiB の判定は、メモリに広げる前に行う（utf8 は長さで、base64 は入力の長さで）。
    pub fn write(&self, actor: &Actor, req: &WriteRequest<'_>) -> Result<WriteResult> {
        self.ensure_writable()?;
        let rel = RelPath::parse(req.path)?;
        if rel.is_root() {
            return Err(VaultError::new(
                ErrorCode::InvalidPath,
                "保管庫の根には書けない（ファイルのパスを渡す）",
            ));
        }
        let bytes: Cow<'_, [u8]> = match req.encoding {
            Encoding::Utf8 => {
                if req.content.len() > MAX_WRITE_BYTES {
                    return Err(too_large());
                }
                Cow::Borrowed(req.content.as_bytes())
            }
            Encoding::Base64 => {
                if req.content.len() > MAX_WRITE_BASE64_CHARS {
                    return Err(too_large());
                }
                let decoded = base64::decode(req.content)?;
                if decoded.len() > MAX_WRITE_BYTES {
                    return Err(too_large());
                }
                Cow::Owned(decoded)
            }
        };
        let lock = self.lock()?;
        let mut txn = Txn::begin(self, &lock)?;
        let result = self.write_in(&mut txn, actor, &rel, &bytes, req);
        txn.finish(result, |w| {
            let mut note = format!("ファイルの書き換えは済んでいる：{}", w.path);
            if let Some(id) = &w.previous_trash_id {
                note.push_str(&format!("（古い中身は previous_trash_id: {id}）"));
            }
            note
        })
    }

    /// vault_mkdir：フォルダを作る（途中のフォルダもなければ作る）。
    /// すでにフォルダがあれば `created:false`、ファイルなどがあれば `ALREADY_EXISTS`。
    pub fn mkdir(&self, actor: &Actor, path: &str) -> Result<MkdirResult> {
        self.ensure_writable()?;
        let rel = RelPath::parse(path)?;
        let lock = self.lock()?;
        let mut txn = Txn::begin(self, &lock)?;
        let result = self.mkdir_in(&mut txn, actor, &rel);
        txn.finish(result, |m| format!("フォルダは作れている：{}", m.path))
    }

    /// vault_move：移す（名前を変える）。移す先にすでに何かあれば `ALREADY_EXISTS`（上書きしない）。
    ///
    /// 移すもののハンドル（2つめの門）と移す先の親のハンドルで、置き換えない rename をする。
    /// 移す先にないことを決めるのは rename の結果（門の「ないことの確かめ」は、エラーの文のためだけ）。
    /// 読み取り専用の印が付いたファイルは移さない（`ACCESS_DENIED`）。
    pub fn move_path(&self, actor: &Actor, from: &str, to: &str) -> Result<MoveResult> {
        self.ensure_writable()?;
        let from_rel = RelPath::parse(from)?;
        let to_rel = RelPath::parse(to)?;
        if from_rel.is_root() || to_rel.is_root() {
            return Err(VaultError::new(
                ErrorCode::InvalidPath,
                "保管庫の根は移せない・根へは移せない",
            ));
        }
        if is_strict_descendant(&to_rel, &from_rel) {
            return Err(invalid_argument("フォルダを、自分の中へは移せない"));
        }
        let lock = self.lock()?;
        let mut txn = Txn::begin(self, &lock)?;
        let result = self.move_in(&mut txn, actor, &from_rel, &to_rel);
        txn.finish(result, |m| {
            format!("移動は済んでいる：{} → {}", m.from, m.to)
        })
    }

    pub(crate) fn ensure_writable(&self) -> Result<()> {
        if self.is_read_only() {
            return Err(VaultError::new(
                ErrorCode::ReadOnly,
                "読み取り専用で開いた保管庫には書けない",
            ));
        }
        Ok(())
    }

    fn write_in(
        &self,
        txn: &mut Txn<'_>,
        actor: &Actor,
        rel: &RelPath,
        bytes: &[u8],
        req: &WriteRequest<'_>,
    ) -> Result<WriteResult> {
        // 1つめの門（今のエラーのコードと文のため。決める値は2つめの門で取り直す）
        let target = self.resolve_rel(rel)?;
        let existing = match fs::symlink_metadata(&target) {
            Ok(meta) => Some(meta),
            Err(e) if is_not_found(&e) => None,
            Err(e) => return Err(VaultError::io("ファイルを調べられない", &e)),
        };
        let existing_info = existing.as_ref().map(FileInfo::from_metadata);
        if let (Some(meta), Some(info)) = (&existing, &existing_info) {
            match info.kind {
                EntryKind::Dir => return Err(not_a_file(rel)),
                EntryKind::Link => return Err(outside_link()),
                EntryKind::File => {}
            }
            if !req.overwrite {
                return Err(already_exists(rel));
            }
            reject_read_only_file(meta, rel)?;
        }
        if let Some(expected) = req.expected_version {
            let current = existing_info.as_ref().map(FileInfo::version);
            if current.as_deref() != Some(expected) {
                return Err(VaultError::new(
                    ErrorCode::Conflict,
                    format!(
                        "ファイルの版が expected_version と合わない（ほかで変わった）：{rel}（いまの版は {}）",
                        current.as_deref().unwrap_or("なし")
                    ),
                ));
            }
        }
        let existing_parents = self.gate_parents(rel, req.create_dirs)?;
        test_hook!(Step::AfterGate);

        // 空き容量（親のフォルダを作る前。足りなければ何も変えない）
        guard::check_free_space(&txn.root, bytes.len())?;

        // A・B：親のフォルダを1段ずつ開く・作る（作った親は、閉じて開き直したものを持つ）
        let chain = self.open_parents(txn, actor, rel, existing_parents)?;
        let name = last_name(rel)?;

        // 2つめの門（上書き）：今あるものを TARGET_FILE_W（書き込みの権利も求める）で開き直して、門の値と
        // 比べる（種類・リンク・読み取り専用・大きさ・時刻）。このハンドルのまま、ゴミ箱へ移す。書き込みか削除を
        // 拒否する設定（ACL）が付いていれば ACCESS_DENIED（門のあとで変わっていれば CONFLICT）
        let old = match &existing {
            Some(meta) => {
                let gate = FileInfo::from_metadata(meta);
                let read_only = meta.permissions().readonly();
                let target = match guard::open_target(
                    parent_of(&txn.root, &chain),
                    name,
                    EntryKind::File,
                    TargetUse::User,
                    "上書きするファイルを開けない",
                )? {
                    TargetOpen::Target(t) if t.info() == gate && t.read_only() == read_only => t,
                    TargetOpen::Target(_) | TargetOpen::Missing => {
                        return Err(changed_after_gate(rel));
                    }
                    TargetOpen::WriteDenied {
                        info,
                        read_only: now,
                    } => {
                        return Err(if info == gate && now == read_only {
                            write_denied(rel, now)
                        } else {
                            changed_after_gate(rel)
                        });
                    }
                };
                let actual = target.actual(&txn.root, rel)?;
                Some((target, actual))
            }
            None => None,
        };

        // 1. .vault/tmp に一時ファイルを作り（最終のパスを確かめてから）、中身を書いて sync
        let tmp = guard::create_tmp(&txn.root, &txn.vault)?.write_all(bytes)?;

        // 2. 上書きなら、古い中身をゴミ箱へ（2つめの門のハンドルを、置き換えない rename で移す。entry には実際の名前）
        let old_path = old.as_ref().map(|(_, actual)| actual.clone());
        let trashed = match old {
            Some((target, actual)) => {
                let size = target.info().size;
                let moved = trash::move_to_trash(
                    &txn.root,
                    &txn.vault,
                    txn.lock,
                    actor,
                    ToTrash {
                        target,
                        origin: guard::rename_parent(&txn.root, &chain),
                        name,
                        original_path: &actual,
                        kind: EntryKind::File,
                        size,
                        reason: TrashReason::Overwrite,
                    },
                );
                match moved {
                    Ok(trashed) => Some(trashed),
                    Err(e) => {
                        tmp.discard();
                        return Err(e);
                    }
                }
            }
            None => None,
        };

        // 3. 置き換えない rename で置く（RootDirectory は書く先の親）
        test_hook!(Step::BeforeRename(Rename::WriteFinal));
        let placed = match tmp.place(guard::rename_parent(&txn.root, &chain), name) {
            Placing::Placed(placed) => placed,
            Placing::Collision(tmp) => {
                tmp.discard();
                let e = VaultError::new(
                    ErrorCode::Conflict,
                    format!(
                        "書く先に、確かめたあとで別のものができた（ほかのアプリが作った）：{rel}。何も上書きしていない"
                    ),
                );
                return Err(self.put_old_back(txn, actor, trashed, &chain, rel, old_path, e));
            }
            Placing::Failed(tmp, e) => {
                tmp.discard();
                return Err(self.put_old_back(txn, actor, trashed, &chain, rel, old_path, e));
            }
        };
        // rename のあと、同じハンドルで最終のパスを読み直す（起きないはず：外なら元へ戻す）
        let rel_s = match placed.check_renamed(&txn.root, rel) {
            Ok(actual) => actual,
            Err(e) => {
                let e = placed.move_back(e, rel);
                return Err(self.put_old_back(txn, actor, trashed, &chain, rel, old_path, e));
            }
        };
        let previous_trash_id = trashed.map(|t| t.into_id().into_string());
        // 書いたハンドルを閉じてから、版（大きさと更新時刻）を読む
        drop(placed);
        let info = match guard::read_leaf_attrs(&txn.root, parent_of(&txn.root, &chain), name, rel)?
        {
            LeafState::File(info) => info,
            _ => {
                return Err(VaultError::new(
                    ErrorCode::IoError,
                    "書いたファイルを調べられない（書いたあとで、ほかのアプリが変えた）",
                ));
            }
        };

        // 4. 履歴  5. 索引（どちらも txn にためて、最後に書く）
        txn.record(
            actor,
            Op::Write,
            Record {
                path: Some(rel_s.clone()),
                trash_id: previous_trash_id.clone(),
                size: Some(info.size),
                ..Record::default()
            },
        )?;
        txn.index(IndexChange::Set {
            path: rel_s.clone(),
            entry: IndexEntry::from(info),
            drop_case_variants: existing.is_some(),
        });
        Ok(WriteResult {
            path: rel_s,
            size: info.size,
            version: info.version(),
            created: existing.is_none(),
            previous_trash_id,
        })
    }

    /// 書き込みが最後で失敗したとき：上書きなら、古い中身をゴミ箱からもとに戻す（take_back。置き換えない rename で、
    /// RootDirectory は書く先の親）。戻せなければ、居場所をエラーの文と履歴（write_failed）に残す。
    #[allow(clippy::too_many_arguments)]
    fn put_old_back(
        &self,
        txn: &mut Txn<'_>,
        actor: &Actor,
        trashed: Option<Trashed>,
        chain: &[Pass],
        rel: &RelPath,
        old_path: Option<String>,
        e: VaultError,
    ) -> VaultError {
        let Some(trashed) = trashed else {
            return e;
        };
        let id = trashed.id().as_str().to_owned();
        let name = rel.components().last().unwrap_or_default();
        let parent = guard::rename_parent(&txn.root, chain);
        if guard::take_back(&txn.root, trashed, parent, name, rel) {
            return e;
        }
        let old_path = old_path.unwrap_or_else(|| rel.to_string());
        let _ = txn.record(
            actor,
            Op::WriteFailed,
            Record {
                path: Some(old_path.clone()),
                trash_id: Some(id.clone()),
                ..Record::default()
            },
        );
        txn.index(IndexChange::RemoveTreeAnyCase(old_path));
        e.with_note(&format!(
            "古い中身はゴミ箱にある（previous_trash_id: {id}。vault_restore で戻せる）"
        ))
    }

    fn mkdir_in(&self, txn: &mut Txn<'_>, actor: &Actor, rel: &RelPath) -> Result<MkdirResult> {
        // 1つめの門
        let abs = self.resolve_rel(rel)?;
        match fs::symlink_metadata(&abs) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                test_hook!(Step::AfterGate);
                // すでにある：開き直して、最終のパスから実際の名前を返す（食い違えば CONFLICT）
                let path = self.open_existing_dir(txn, rel)?;
                return Ok(MkdirResult {
                    path,
                    created: false,
                });
            }
            Ok(_) => {
                return Err(VaultError::new(
                    ErrorCode::AlreadyExists,
                    format!("フォルダでないものがすでにある：{rel}"),
                ));
            }
            Err(e) if is_not_found(&e) => {}
            Err(e) => return Err(VaultError::io("パスを調べられない", &e)),
        }
        let existing_parents = self.gate_parents(rel, true)?;
        test_hook!(Step::AfterGate);
        let chain = self.open_parents(txn, actor, rel, existing_parents)?;
        let path = self.mkdir_one(txn, actor, &chain, rel, false)?.0;
        Ok(MkdirResult {
            path,
            created: true,
        })
    }

    /// すでにあるフォルダを、根から1段ずつ開き直し、最終のパスから実際の名前を返す。
    fn open_existing_dir(&self, txn: &Txn<'_>, rel: &RelPath) -> Result<String> {
        let mut chain: Vec<Pass> = Vec::new();
        for (i, part) in rel.components().enumerate() {
            let step = guard::step_dir(parent_of(&txn.root, &chain), part)?;
            match step {
                DirStep::Dir(p) => chain.push(p),
                DirStep::Link => return Err(outside_link()),
                DirStep::Missing | DirStep::NotDir => {
                    let sub = sub_rel(rel, i + 1)?;
                    return Err(changed_after_gate(&sub));
                }
            }
        }
        match chain.last() {
            Some(last) => guard::check_user_dir(&txn.root, last, rel, At::Target),
            None => Ok(String::new()),
        }
    }

    fn move_in(
        &self,
        txn: &mut Txn<'_>,
        actor: &Actor,
        from_rel: &RelPath,
        to_rel: &RelPath,
    ) -> Result<MoveResult> {
        // 1つめの門（今のエラーのコードと文のため。決める値は2つめの門で取り直し、移す先にないことは rename の結果で決める）
        let from_abs = self.resolve_rel(from_rel)?;
        let to_abs = self.resolve_rel(to_rel)?;
        let from_meta = match fs::symlink_metadata(&from_abs) {
            Ok(meta) => meta,
            Err(e) if is_not_found(&e) => return Err(not_found(from_rel)),
            Err(e) => return Err(VaultError::io("移すものを調べられない", &e)),
        };
        let info = FileInfo::from_metadata(&from_meta);
        if info.kind == EntryKind::File {
            reject_read_only_file(&from_meta, from_rel)?;
        }
        if exists(&to_abs)? {
            return Err(already_exists(to_rel));
        }
        let to_parent = to_abs
            .parent()
            .ok_or_else(|| VaultError::new(ErrorCode::Internal, "移す先の親がない"))?;
        match fs::symlink_metadata(to_parent) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(VaultError::new(
                    ErrorCode::NotADirectory,
                    format!("移す先の親がフォルダでない：{to_rel}"),
                ));
            }
            Err(e) if is_not_found(&e) => {
                return Err(VaultError::new(
                    ErrorCode::NotFound,
                    format!("移す先のフォルダがない：{to_rel}（先に vault_mkdir で作る）"),
                ));
            }
            Err(e) => return Err(VaultError::io("移す先を調べられない", &e)),
        }
        test_hook!(Step::AfterGate);

        // A：移す元の親と移す先の親を、根から1段ずつ開いて持つ。2つめの門：移すものを TARGET_* で開き、門の値と比べる
        let from_chain = self.open_parents(txn, actor, from_rel, usize::MAX)?;
        let mut target = second_gate(
            &txn.root,
            &from_chain,
            from_rel,
            &from_meta,
            "移すものを開けない",
        )?;
        // 記録は、実際の名前（大文字小文字）にそろえる：移すものは rename の前、移した先は rename のあとのハンドルから
        let from_s = target.actual(&txn.root, from_rel)?;
        // フォルダなら、中のいちばん深いもの・いちばん長いものが、移す先で上限に収まるかを、何も変える前に確かめる
        // （移すもののハンドルで歩く）
        if info.kind == EntryKind::Dir {
            self.check_extent(
                to_rel,
                subtree_extent(&mut target)?,
                "移す先では、中のいちばん深いもの・長いもの",
            )?;
        }
        let to_chain = self.open_parents(txn, actor, to_rel, usize::MAX)?;
        let to_name = last_name(to_rel)?;

        // C：置き換えない rename（RootDirectory は移す先の親）。移す先にものができていたら ALREADY_EXISTS
        test_hook!(Step::BeforeRename(Rename::Move));
        match target.rename_into(
            guard::rename_parent(&txn.root, &to_chain),
            to_name,
            "移せない",
        )? {
            Moved::Done => {}
            Moved::Collision => return Err(already_exists(to_rel)),
        }
        // rename のあと、同じハンドルで最終のパスを読み直す（起きないはず：外なら、置き換えずに元の場所へ戻す）
        let to_s = match target.check_renamed(&txn.root, to_rel) {
            Ok(actual) => actual,
            Err(e) => {
                let back = target.move_back(
                    guard::rename_parent(&txn.root, &from_chain),
                    last_name(from_rel)?,
                );
                return Err(if back {
                    e
                } else {
                    e.with_note(&format!("元の場所へ戻せなかった（移した先：{to_rel}）"))
                });
            }
        };
        // まだ索引にない（走査の前に外で作られた）ときに入れる値は、移したもののハンドルで読む・歩く
        let not_indexed = if txn.index_contains(&from_s) {
            None
        } else {
            Some(self.index_tree(&mut target, &to_s))
        };
        drop(target);
        txn.record(
            actor,
            Op::Move,
            Record {
                path: Some(from_s.clone()),
                to: Some(to_s.clone()),
                size: (info.kind == EntryKind::File).then_some(info.size),
                ..Record::default()
            },
        )?;
        match not_indexed {
            None => txn.index(IndexChange::RenameTree {
                from: from_s.clone(),
                to: to_s.clone(),
            }),
            Some(changes) => {
                // まだ索引にない（走査の前に外で作られた）：行き先を、実際の値で入れる（あとの走査で作成と出さない）
                txn.index(IndexChange::RemoveTreeAnyCase(to_s.clone()));
                for change in changes {
                    txn.index(change);
                }
            }
        }
        Ok(MoveResult {
            from: from_s,
            to: to_s,
        })
    }

    /// 1つめの門：`rel` の親のフォルダを、根に近いほうからパスで確かめる（今のエラーのコードと文のため）。
    /// 返り値は、根から続けてある親の数（それより下は、ない）。ないものがあって `create` でなければ NOT_FOUND。
    pub(crate) fn gate_parents(&self, rel: &RelPath, create: bool) -> Result<usize> {
        let parts: Vec<&str> = rel.components().collect();
        let Some((_, parents)) = parts.split_last() else {
            return Ok(0);
        };
        let mut abs = self.root().to_path_buf();
        let mut rel_path = String::new();
        for (i, part) in parents.iter().enumerate() {
            abs.push(part);
            if !rel_path.is_empty() {
                rel_path.push('/');
            }
            rel_path.push_str(part);
            match fs::symlink_metadata(&abs) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(outside_link()),
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(VaultError::new(
                        ErrorCode::NotADirectory,
                        format!("途中がフォルダでない：{rel_path}"),
                    ));
                }
                Err(e) if is_not_found(&e) => {
                    if !create {
                        return Err(VaultError::new(
                            ErrorCode::NotFound,
                            format!("親のフォルダがない：{rel_path}（create_dirs:true で作れる）"),
                        ));
                    }
                    return Ok(i);
                }
                Err(e) => return Err(VaultError::io("パスを調べられない", &e)),
            }
        }
        Ok(parents.len())
    }

    /// A・B：`rel` の親のフォルダを、根から1段ずつ、持っている親のハンドルを基準に開く。門で「あった」もの
    /// （`existing` 個）は開き、それより下は作る（履歴に mkdir をため、閉じて開き直したものを持つ）。
    /// 返り値は、根からの親の並び（いちばん下が `rel` の親）。
    pub(crate) fn open_parents(
        &self,
        txn: &mut Txn<'_>,
        actor: &Actor,
        rel: &RelPath,
        existing: usize,
    ) -> Result<Vec<Pass>> {
        let count = rel.components().count().saturating_sub(1);
        let mut chain: Vec<Pass> = Vec::new();
        for i in 0..count {
            let sub = sub_rel(rel, i + 1)?;
            let name = last_name(&sub)?;
            let next = if i < existing {
                match guard::step_dir(parent_of(&txn.root, &chain), name)? {
                    DirStep::Dir(p) => p,
                    DirStep::Link => return Err(outside_link()),
                    // 門と A のあいだに消えた（1-5）
                    DirStep::Missing => return Err(not_found(&sub)),
                    DirStep::NotDir => {
                        return Err(VaultError::new(
                            ErrorCode::NotADirectory,
                            format!("途中がフォルダでない：{sub}"),
                        ));
                    }
                }
            } else {
                self.mkdir_one(txn, actor, &chain, &sub, true)?
                    .1
                    .ok_or_else(|| VaultError::new(ErrorCode::Internal, "作った親を開き直せない"))?
            };
            chain.push(next);
        }
        Ok(chain)
    }

    /// フォルダを1つ作り（B）、履歴（mkdir）と索引の変更を txn にためる。記録は、作ったハンドルの最終のパスの
    /// 実際の名前にそろえ、その相対パスを返す。`reopen` なら、閉じてから開き直したものも返す（このあと親として使う）。
    fn mkdir_one(
        &self,
        txn: &mut Txn<'_>,
        actor: &Actor,
        chain: &[Pass],
        rel: &RelPath,
        reopen: bool,
    ) -> Result<(String, Option<Pass>)> {
        let name = last_name(rel)?;
        test_hook!(Step::BeforeCreate);
        let created = guard::create_user_dir(&txn.root, parent_of(&txn.root, chain), name, rel)?;
        let actual = created.actual().to_owned();
        let info = created.info();
        txn.record(
            actor,
            Op::Mkdir,
            Record {
                path: Some(actual.clone()),
                ..Record::default()
            },
        )?;
        txn.index(IndexChange::Set {
            path: actual.clone(),
            entry: IndexEntry::from(info),
            drop_case_variants: false,
        });
        if !reopen {
            created.close();
            return Ok((actual, None));
        }
        let pass = created.reopen(&txn.root, parent_of(&txn.root, chain), rel)?;
        Ok((actual, Some(pass)))
    }
}

/// 移すフォルダの中を、そのハンドル（TARGET_DIR）で歩き（E。リンクはたどらず、再帰でなく繰り返しで）、
/// いちばん深いものの深さと、いちばん長い相対パスの長さ（UTF-16）を返す。
pub(crate) fn subtree_extent(target: &mut Target) -> Result<Extent> {
    let mut extent = Extent::default();
    target.walk(true, &mut |found| {
        let name_len = found.name.to_string_lossy().encode_utf16().count();
        let len = if found.parent_rel.is_empty() {
            name_len
        } else {
            found.parent_rel.encode_utf16().count() + 1 + name_len
        };
        extent.depth = extent.depth.max(found.depth);
        extent.len_utf16 = extent.len_utf16.max(len);
        Ok(true)
    })?;
    Ok(extent)
}

/// 読み取り専用の印が付いたファイルは、上書きも移動もしない（利用者が守るために付けた印を尊重する）。
pub(crate) fn reject_read_only_file(meta: &Metadata, rel: &RelPath) -> Result<()> {
    if meta.permissions().readonly() {
        return Err(access_denied(&format!(
            "読み取り専用の印が付いたファイルは、上書きも移動もしない：{rel}（印を外してから使う）"
        )));
    }
    Ok(())
}

/// 一覧の1項目と、その絶対パス（中へ進むときに使う。返り値には入れない）。
/// 持っているフォルダの中を、そのハンドルで列挙し（E）、フォルダを先に、名前の順に並べる。`.vault`（大文字小文字を
/// 問わない）は除く。ツールで扱えない名前（UTF-8 にできない・文字列の規則に通らない）も除き、その数を2つ目の値で返す。
/// 値（種類・大きさ・版）は、項目を属性だけで開いて読む（LEAF_ATTRS。vault_read と同じ値になる）。
/// 開けなければ列挙の値を使い、消えていれば飛ばす。
fn read_children(dir: &mut Held, rel_dir: &str) -> Result<(Vec<ListEntry>, u64)> {
    let mut children = Vec::new();
    let mut invalid = 0u64;
    for listed in dir.entries("フォルダを読めない")? {
        if listed.name.eq_ignore_ascii_case(RESERVED_NAME) {
            continue;
        }
        let Some(name) = listed
            .name
            .to_str()
            .filter(|name| is_valid_component(name))
            .map(str::to_owned)
        else {
            invalid += 1;
            continue;
        };
        let info = match dir.child_info(&listed.name) {
            ChildInfo::Info(info) => info,
            ChildInfo::Missing => continue, // 読むあいだに消えた
            ChildInfo::Unreadable => listed.info,
        };
        let path = if rel_dir.is_empty() {
            name.clone()
        } else {
            format!("{rel_dir}/{name}")
        };
        children.push(ListEntry {
            path,
            name,
            kind: info.kind,
            size: info.size,
            modified: info.modified(),
            version: info.version(),
        });
    }
    children.sort_by(|a, b| {
        let a_key = (a.kind != EntryKind::Dir, &a.name);
        let b_key = (b.kind != EntryKind::Dir, &b.name);
        a_key.cmp(&b_key)
    });
    Ok((children, invalid))
}

/// 一覧するフォルダ（根でない `rel`）を開く：根から1段ずつ、途中は通り抜けるだけの形（Pass）で、最後は HOLD_DIR で
/// 開き（A）、最終のパスが `rel` の場所であることを確かめる。途中が消えた・フォルダでないなら NOT_FOUND・
/// NOT_A_DIRECTORY、リンクなら OUTSIDE_VAULT（今の門と同じ）。
fn open_list_dir(root: &Root, rel: &RelPath) -> Result<(Vec<Pass>, Held)> {
    let parts: Vec<&str> = rel.components().collect();
    let (last, parents) = parts
        .split_last()
        .ok_or_else(|| VaultError::new(ErrorCode::Internal, "パスに名前がない"))?;
    let mut chain: Vec<Pass> = Vec::new();
    for (i, part) in parents.iter().enumerate() {
        let sub = sub_rel(rel, i + 1)?;
        match guard::step_dir(parent_of(root, &chain), part)? {
            DirStep::Dir(p) => chain.push(p),
            DirStep::Link => return Err(outside_link()),
            DirStep::Missing => return Err(not_found(&sub)),
            DirStep::NotDir => return Err(not_a_directory(&sub)),
        }
    }
    let held = match guard::open_list_dir(parent_of(root, &chain), last)? {
        ListDir::Dir(held) => held,
        ListDir::Link => return Err(outside_link()),
        ListDir::Missing => return Err(not_found(rel)),
        ListDir::NotDir => return Err(not_a_directory(rel)),
    };
    guard::check_held_dir(root, &held, rel, At::Target)?;
    Ok((chain, held))
}

/// 読むためにファイルを開く：根から1段ずつ、持っている親を基準に、リンクをたどらずに開き（A）、
/// 最終のパスが読む対象の場所であることを確かめる。途中が消えた・フォルダでないなら NOT_FOUND、リンクなら
/// OUTSIDE_VAULT（今の門と同じ）。
fn open_file_to_read(root: &Root, rel: &RelPath) -> Result<File> {
    let parts: Vec<&str> = rel.components().collect();
    let Some((leaf, parents)) = parts.split_last() else {
        return Err(not_a_file(rel));
    };
    let mut chain: Vec<Pass> = Vec::new();
    for part in parents {
        match guard::step_dir(parent_of(root, &chain), part)? {
            DirStep::Dir(p) => chain.push(p),
            DirStep::Link => return Err(outside_link()),
            DirStep::Missing | DirStep::NotDir => return Err(not_found(rel)),
        }
    }
    let file = match guard::open_leaf_read(parent_of(root, &chain), leaf)? {
        LeafRead::File(f) => f,
        LeafRead::Dir => return Err(not_a_file(rel)),
        LeafRead::Link => return Err(outside_link()),
        LeafRead::Missing => return Err(not_found(rel)),
    };
    guard::check_user_target(root, &file, rel, At::Target)?;
    Ok(file)
}

/// 2つめの門（削除・移動・restore の replace）：今あるものを、親を基準に TARGET_FILE_W（ファイル。書き込みの権利も
/// 求める）・TARGET_DIR（フォルダ）・TARGET_FILE（リンクそのもの）で開き直し、門の値と比べる（種類・リンク・
/// 読み取り専用・大きさ。時刻は比べない＝ゴミ箱に入るので戻せる。フォルダとリンクは種類だけ）。違う・消えていれば CONFLICT。書き込みか削除を拒否する設定（ACL）が付いたファイルで、値が門と同じなら
/// ACCESS_DENIED。
pub(crate) fn second_gate(
    root: &Root,
    chain: &[Pass],
    rel: &RelPath,
    gate: &Metadata,
    what: &str,
) -> Result<Target> {
    let g = FileInfo::from_metadata(gate);
    let gate_read_only = gate.permissions().readonly();
    let same = |now: FileInfo, read_only: bool| {
        now.kind == g.kind
            && (g.kind != EntryKind::File || (now.size == g.size && read_only == gate_read_only))
    };
    match guard::open_target(
        parent_of(root, chain),
        last_name(rel)?,
        g.kind,
        TargetUse::User,
        what,
    )? {
        TargetOpen::Target(target) if same(target.info(), target.read_only()) => Ok(target),
        TargetOpen::Target(_) | TargetOpen::Missing => Err(changed_after_gate(rel)),
        TargetOpen::WriteDenied { info, read_only } => Err(if same(info, read_only) {
            write_denied(rel, read_only)
        } else {
            changed_after_gate(rel)
        }),
    }
}

/// 2つめの門で、書き込みか削除を拒否された（値は門と同じ）。読み取り専用の印なら今の文、そうでなければ ACL の文
fn write_denied(rel: &RelPath, read_only: bool) -> VaultError {
    if read_only {
        return access_denied(&format!(
            "読み取り専用の印が付いたファイルは、上書きも移動もしない：{rel}（印を外してから使う）"
        ));
    }
    access_denied(&format!(
        "書き込みか削除を拒否する設定（ACL）が付いたファイルは、上書き・移動・削除・置き換えをしない：{rel}（設定を外してから使う）"
    ))
}

/// `rel` の最後の要素。
pub(crate) fn last_name(rel: &RelPath) -> Result<&str> {
    rel.components()
        .last()
        .ok_or_else(|| VaultError::new(ErrorCode::Internal, "パスに名前がない"))
}

/// `rel` の先頭から `n` 個の要素の相対パス。
fn sub_rel(rel: &RelPath, n: usize) -> Result<RelPath> {
    let joined = rel.components().take(n).collect::<Vec<_>>().join("/");
    RelPath::parse(&joined)
}

/// 門と2つめの門で値が食い違った（確かめたあとで、ほかのアプリが変えた）。
fn changed_after_gate(rel: &RelPath) -> VaultError {
    VaultError::new(
        ErrorCode::Conflict,
        format!("確かめたあとで、ほかのアプリが変えた：{rel}。もう一度試す"),
    )
}

/// `offset` から最大 `max` バイトを `buf` に読む。
fn read_at(file: &mut File, offset: u64, max: u64, buf: &mut Vec<u8>) -> Result<()> {
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| Read::by_ref(file).take(max).read_to_end(buf))
        .map(|_| ())
        .map_err(|e| VaultError::io("ファイルを読めない", &e))
}

/// UTF-8 の1文字のバイト数（先頭のバイトから）。先頭になれないバイトなら None。
fn utf8_char_len(first: u8) -> Option<usize> {
    match first {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

/// `child` が `parent` の下（同じものは除く）か。Windows では ASCII の大文字小文字を区別しない。
fn is_strict_descendant(child: &RelPath, parent: &RelPath) -> bool {
    let c: Vec<&str> = child.components().collect();
    let p: Vec<&str> = parent.components().collect();
    c.len() > p.len()
        && c.iter().zip(&p).all(|(a, b)| {
            if cfg!(windows) {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        })
}

pub(crate) fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if is_not_found(&e) => Ok(false),
        Err(e) => Err(VaultError::io("パスを調べられない", &e)),
    }
}

pub(crate) fn not_found(rel: &RelPath) -> VaultError {
    VaultError::new(ErrorCode::NotFound, format!("ない：{rel}"))
}

fn not_a_file(rel: &RelPath) -> VaultError {
    VaultError::new(ErrorCode::NotAFile, format!("ファイルでない：{rel}"))
}

fn not_a_directory(rel: &RelPath) -> VaultError {
    VaultError::new(ErrorCode::NotADirectory, format!("フォルダでない：{rel}"))
}

fn not_utf8(rel: &RelPath) -> VaultError {
    VaultError::new(
        ErrorCode::NotUtf8,
        format!("UTF-8 の文字として読めない：{rel}（encoding:\"base64\" で読める）"),
    )
}

pub(crate) fn already_exists(rel: &RelPath) -> VaultError {
    VaultError::new(
        ErrorCode::AlreadyExists,
        format!("すでにある：{rel}（上書きしない）"),
    )
}

fn too_large() -> VaultError {
    VaultError::new(
        ErrorCode::TooLarge,
        format!("中身は {MAX_WRITE_BYTES} バイト（16 MiB）まで"),
    )
}

pub(crate) fn outside_link() -> VaultError {
    VaultError::new(
        ErrorCode::OutsideVault,
        "リンク（シンボリックリンク・ジャンクション）はたどれない",
    )
}

pub(crate) fn invalid_argument(message: impl Into<String>) -> VaultError {
    VaultError::new(ErrorCode::InvalidArgument, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_match_the_adr() {
        assert_eq!(MAX_WRITE_BYTES, 16 * 1024 * 1024);
        assert_eq!(MAX_WRITE_BASE64_CHARS, 22_369_624);
        assert_eq!(MAX_READ_BYTES, 16 * 1024 * 1024);
        assert_eq!(DEFAULT_READ_BYTES, 256 * 1024);
        assert_eq!(DEFAULT_LIST_LIMIT, 1000);
        assert_eq!(MAX_LIST_LIMIT, 10_000);
    }

    #[test]
    fn strict_descendants() {
        let p = |s| RelPath::parse(s).unwrap();
        assert!(is_strict_descendant(&p("a/b"), &p("a")));
        assert!(is_strict_descendant(&p("a/b/c"), &p("a")));
        assert!(!is_strict_descendant(&p("a"), &p("a")));
        assert!(!is_strict_descendant(&p("ab/c"), &p("a")));
        assert!(!is_strict_descendant(&p("b"), &p("a")));
        assert_eq!(
            is_strict_descendant(&p("A/b"), &p("a")),
            cfg!(windows),
            "Windows では大文字小文字を区別しない"
        );
    }

    #[test]
    fn utf8_char_lengths() {
        assert_eq!(utf8_char_len(b'a'), Some(1));
        assert_eq!(utf8_char_len("é".as_bytes()[0]), Some(2));
        assert_eq!(utf8_char_len("あ".as_bytes()[0]), Some(3));
        assert_eq!(utf8_char_len("𝄞".as_bytes()[0]), Some(4));
        for b in [0x80, 0xBF, 0xC0, 0xC1, 0xF5, 0xFF] {
            assert_eq!(utf8_char_len(b), None, "{b:#x}");
        }
    }

    #[test]
    fn encodings_serialize_like_the_adr() {
        assert_eq!(serde_json::to_value(Encoding::Utf8).unwrap(), "utf8");
        assert_eq!(serde_json::to_value(Encoding::Base64).unwrap(), "base64");
    }
}
