//! 操作の履歴（`.vault/journal/<最初のseq（20桁）>.jsonl`）。
//!
//! - 追記だけ。1行に1つの JSON（`{"seq","ts","actor":{"kind","id","session"},"op","path","to","trash_id","size"}`）。
//! - seq は、ロックを持った状態で、最後の履歴ファイルを末尾からさかのぼり、最後に読める行の seq に1を足したもの。
//! - ファイルが改行で終わっていなければ、先に改行を書いてから追記する（壊れた行を1行に閉じ込める）。
//! - 8 MiB を超えたら、次のファイルに分ける。最後のファイルがほかの名前とハードリンクでつながっていれば、そのファイル
//!   には書かず、次のファイルに切り替える（a3）。
//!
//! 書き方：ツールの呼び出しの最初に `JournalWriter::open` で履歴のファイルを開いて seq を決め（書けることを確かめる）、
//! 操作のあいだに `push` で行をためて、最後に `commit` で1回の書き込みと1回の sync で書く。
//!
//! 読み方（vault_changes）：`read_since` で、seq が since_seq より大きい行を古い順に読む（ロックは取らない）。

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};

use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::error::{ErrorCode, Result, VaultError};
use crate::fsutil::io_or_disk_full;
use crate::guard::{self, Held, ManagedName, Pass, Root};
use crate::relpath::RelPath;
use crate::time;
use crate::vault::VaultLock;

/// 履歴のフォルダ（`.vault/journal`）。
pub(crate) const JOURNAL_DIR: &str = "journal";
/// 1つの履歴ファイルの大きさの目安。これ以上なら次のファイルに分ける。
pub const MAX_JOURNAL_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// 書く直前に新しい履歴のファイルへ切り替えるとき、ふさがっている名前を飛ばして試す名前の数。
const MAX_SWITCH_NAMES: usize = 8;
/// 末尾からさかのぼるときに、最初に読む大きさ（足りなければ倍にする）。
const TAIL_WINDOW: u64 = 64 * 1024;

/// 操作の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Init,
    Mkdir,
    Write,
    /// 書き換えに失敗し、古い中身をゴミ箱からもとに戻せなかった（`trash_id` に、古い中身の居場所がある）。
    WriteFailed,
    Delete,
    Move,
    Restore,
    ExternalCreate,
    ExternalModify,
    ExternalDelete,
    ExternalRename,
    IndexRebuilt,
}

/// 履歴の1行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: u64,
    pub ts: String,
    pub actor: Actor,
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// 追記する中身（seq と ts は `push` が付ける）。
#[derive(Debug, Default)]
pub(crate) struct Record {
    pub path: Option<String>,
    pub to: Option<String>,
    pub trash_id: Option<String>,
    pub size: Option<u64>,
}

/// 履歴に書くもの。ロックを持っていること（`_lock`）が条件。
///
/// 履歴のフォルダ（`dir`）と履歴のファイルを commit まで持つ。フォルダは持っているあいだ名前を変えられず
/// ファイルは DELETE を共有しないので、同じユーザーのほかのプロセスも、書いているあいだ
/// 履歴のファイルの名前を変える・消すことができない。
///
/// 履歴の追記は、その場で中身を書く唯一の道。書くファイルがほかの名前とハードリンクでつながっていれば（開いたとき・
/// 書く直前の2回、同じハンドルで読む）、そのファイルには書かず、新しい履歴のファイルに切り替える（ハードリンクで残すバックアップの写しを書き換えない）。
pub(crate) struct JournalWriter<'a> {
    dir: Held,
    file: File,
    /// 書くファイルの名前の seq（そのファイルの最初の seq）。書く直前に切り替えるとき、名前がぶつからないように使う。
    file_seq: u64,
    /// open で新しく作った履歴のファイルの記録（init の片づけ）。
    created: Option<guard::Made>,
    next_seq: u64,
    needs_newline: bool,
    /// ためた行（commit で JSON にする。書く直前に切り替えて seq を付け直すことがあるため）。
    pending: Vec<JournalEntry>,
    _lock: &'a VaultLock,
}

impl<'a> JournalWriter<'a> {
    /// 履歴のファイルを開き、次の seq を決める（操作の前に、履歴に書けることを確かめる）。
    ///
    /// 最後のファイルが 8 MiB 以上か、ほかの名前とハードリンクでつながっていれば、そのファイルには
    /// 書かず、次のファイルを作る。その名前が最後のファイルとぶつからないように、最後のファイルに読める行が1つも
    /// ないときは、seq をそのファイルの最初の seq より1つ進める。リンクの数を読めなければ IO_ERROR（書かない）。
    ///
    /// 履歴のフォルダは、`.vault` のハンドル（`vault`）を基準に、リンクをたどらずに開く（ないなら作って開き直す）。
    /// ファイルの名前の一覧は、持っている履歴のフォルダのハンドルで列挙し、選んだ名前もそのフォルダを基準に開く。
    pub fn open(root: &Root, vault: &Pass, lock: &'a VaultLock) -> Result<Self> {
        test_hook!(Step::Open(Place::Journal));
        let mut dir = guard::open_place(
            root,
            vault,
            &ManagedName::Journal,
            true,
            guard::MANAGEMENT_DIR_NOT_DIR,
        )?
        .ok_or_else(|| VaultError::new(ErrorCode::Internal, "履歴のフォルダがない"))?;
        let files = journal_files(&mut dir)?;
        let Some((first_seq, name)) = files.last() else {
            return Self::new_file(root, dir, 1, lock);
        };
        let mut file = guard::open_journal_file(&dir, name)?;
        // 開いたハンドルで、ほかの名前とつながっていないかを読む（書くのも同じハンドル）
        let linked = guard::journal_is_linked(&file)?;
        let len = file
            .metadata()
            .map_err(|e| VaultError::io("履歴を調べられない", &e))?
            .len();
        let last = last_readable(&mut file, len)?.unwrap_or(0);
        // 最後のファイルに読める行がなくても、ファイルの名前の seq より前に戻らない
        let next = next_after(last.max(first_seq.saturating_sub(1)))?;
        if linked || len >= MAX_JOURNAL_FILE_BYTES {
            drop(file);
            let seq = next.max(next_after(*first_seq)?);
            return Self::new_file(root, dir, seq, lock);
        }
        let needs_newline = len > 0 && !ends_with_newline(&mut file, len)?;
        Ok(Self {
            dir,
            file,
            file_seq: *first_seq,
            created: None,
            next_seq: next,
            needs_newline,
            pending: Vec::new(),
            _lock: lock,
        })
    }

    fn new_file(root: &Root, dir: Held, seq: u64, lock: &'a VaultLock) -> Result<Self> {
        let name = ManagedName::journal_file(seq);
        // 操作の前なので、名前がふさがっていれば、何も変えずにエラーで止める（今までと同じ）
        let Some((file, made)) = guard::create_journal_file(root, &dir, &name)? else {
            return Err(VaultError::new(
                ErrorCode::IoError,
                "履歴のファイルを作れない（同じ名前のものがすでにある）",
            ));
        };
        Ok(Self {
            dir,
            file,
            file_seq: seq,
            created: Some(made),
            next_seq: seq,
            needs_newline: false,
            pending: Vec::new(),
            _lock: lock,
        })
    }

    /// `open` で新しく作った履歴のファイルの記録を取り出す（init の片づけに使う）。
    pub(crate) fn take_created(&mut self) -> Option<guard::Made> {
        self.created.take()
    }

    /// 1行をためる。返り値は付けた seq（書く直前に新しいファイルへ切り替えるときは、付け直すことがある）。
    pub fn push(&mut self, actor: &Actor, op: Op, record: Record) -> Result<u64> {
        let seq = self.next_seq;
        let next = next_after(seq)?;
        let ts = time::format_unix_millis(time::now_unix_millis())
            .ok_or_else(|| VaultError::new(ErrorCode::Internal, "今の時刻を書けない"))?;
        self.pending.push(JournalEntry {
            seq,
            ts,
            actor: actor.clone(),
            op,
            path: record.path,
            to: record.to,
            trash_id: record.trash_id,
            size: record.size,
        });
        self.next_seq = next;
        Ok(seq)
    }

    /// ためた行を、1回の書き込みと1回の sync で書く。ためた行がなければ何もしない。
    ///
    /// 書く直前に、同じハンドルでリンクの数を読み直す（開いたあとでハードリンクを足されたとき）。つながっていれば、
    /// そのファイルには書かず、ためた行の最初の seq の名前で新しい履歴のファイルを作って書く。その名前が今のファイルの
    /// 名前とぶつかる（今のファイルに読める行がなかった）ときは、open の切り替えと同じく、今のファイルの最初の seq より
    /// 1つ進めた seq から付け直す。操作は済んでいるので、作る名前がすでにふさがっていれば（ほかのプロセスが先に置いた）、
    /// seq を1つずつ進めて `MAX_SWITCH_NAMES` 個の名前まで作り直す（上書きはしない。行の seq も付け直す）。
    /// 新しく作ったファイルの記録は `on_created` に渡す（init の片づけ）。リンクの数を読めなければ IO_ERROR（書かない）。
    pub fn commit(mut self, root: &Root, on_created: impl FnOnce(guard::Made)) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if guard::journal_is_linked(&self.file)? {
            let mut start = self.pending[0].seq.max(next_after(self.file_seq)?);
            let mut created = None;
            for _ in 0..MAX_SWITCH_NAMES {
                let name = ManagedName::journal_file(start);
                if let Some(made) = guard::create_journal_file(root, &self.dir, &name)? {
                    created = Some(made);
                    break;
                }
                start = next_after(start)?;
            }
            let Some((file, made)) = created else {
                return Err(VaultError::new(
                    ErrorCode::IoError,
                    "履歴のファイルを作れない（切り替える先の名前が、どれもすでにある）",
                ));
            };
            let mut seq = start;
            for entry in &mut self.pending {
                entry.seq = seq;
                seq = next_after(seq)?;
            }
            on_created(made);
            self.file = file;
            self.needs_newline = false;
        }
        let mut bytes = Vec::new();
        if self.needs_newline {
            bytes.push(b'\n');
        }
        for entry in &self.pending {
            serde_json::to_writer(&mut bytes, entry)
                .map_err(|_| VaultError::new(ErrorCode::Internal, "履歴の行を作れない"))?;
            bytes.push(b'\n');
        }
        self.file
            .write_all(&bytes)
            .and_then(|()| self.file.sync_data())
            .map_err(|e| io_or_disk_full("履歴に書けない", &e))
    }
}

/// 履歴のファイルの一覧（`<20桁の最初の seq>.jsonl` の形のものだけ。最初の seq の順）。持っている履歴のフォルダの
/// ハンドルで列挙する（E）。
pub(crate) fn journal_files(dir: &mut Held) -> Result<Vec<(u64, ManagedName)>> {
    let mut files = Vec::new();
    for entry in dir.entries("履歴のフォルダを読めない")? {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        let Some(digits) = name.strip_suffix(".jsonl") else {
            continue;
        };
        if digits.len() == 20
            && digits.bytes().all(|b| b.is_ascii_digit())
            && let Ok(first) = digits.parse::<u64>()
        {
            files.push((first, ManagedName::JournalFile(name.to_owned())));
        }
    }
    files.sort_by_key(|(first, _)| *first);
    Ok(files)
}

/// 行の頭（seq と op）だけを読む形。op は、知らない名前でも読めるように文字のまま（値は使わない。op が文字でない
/// 行を読める行としないための形）。
#[derive(Deserialize)]
struct LineHead {
    seq: u64,
    #[serde(default, rename = "op")]
    _op: Option<String>,
}

/// `seq` の次の seq。あふれるなら `NOT_A_VAULT`（履歴が壊れているか、書き換えられている）。
fn next_after(seq: u64) -> Result<u64> {
    seq.checked_add(1).ok_or_else(|| {
        VaultError::new(
            ErrorCode::NotAVault,
            "履歴の seq が上限に達している（履歴が壊れているか、書き換えられている）",
        )
    })
}

/// 履歴のファイル（持っている履歴のフォルダの `name`）の、最後に読める行の seq。読める行がなければ、そのファイルの
/// 最初の seq の1つ前（`JournalWriter::open` が次の seq を決めるのと同じ決め方）。走査の印（Stamp）に使う。
pub(crate) fn last_seq_of(dir: &Held, name: &ManagedName, first_seq: u64) -> Result<u64> {
    let mut file = guard::open_journal_for_reading(dir, name)?;
    let len = file
        .metadata()
        .map_err(|e| VaultError::io("履歴を調べられない", &e))?
        .len();
    Ok(match last_readable(&mut file, len)? {
        Some(seq) => seq.max(first_seq.saturating_sub(1)),
        None => first_seq.saturating_sub(1),
    })
}

/// 末尾からさかのぼって、最後に読める行の seq を探す。読める行がなければ None。
fn last_readable(file: &mut File, len: u64) -> Result<Option<u64>> {
    let mut window = TAIL_WINDOW;
    loop {
        let start = len.saturating_sub(window);
        let mut buf = Vec::new();
        file.seek(SeekFrom::Start(start))
            .and_then(|_| Read::by_ref(file).take(len - start).read_to_end(&mut buf))
            .map_err(|e| VaultError::io("履歴を読めない", &e))?;
        // 途中から読んだときは、最初の行が欠けているかもしれないので使わない
        let body: &[u8] = if start == 0 {
            &buf
        } else {
            match buf.iter().position(|&b| b == b'\n') {
                Some(i) => &buf[i + 1..],
                None => &[],
            }
        };
        for line in body.rsplit(|&b| b == b'\n') {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Ok(LineHead { seq, .. }) = serde_json::from_slice(line) {
                return Ok(Some(seq));
            }
        }
        if start == 0 {
            return Ok(None);
        }
        window = window.saturating_mul(2);
    }
}

/// seq が `since_seq` より大きい行を、古い順に `limit` 件まで読む（vault_changes。ロックは取らない）。
/// 2つ目の値は、limit で切ったか（まだあるか）。読めない行（書いている途中・壊れた行）と、
/// path・to が文字列の規則（`RelPath`）に通らない行（外で書き換えられた絶対パス・`..` など）は飛ばす。
///
/// ファイルの名前は、そのファイルの最初の seq なので、次のファイルの最初の seq が `since_seq + 1` 以下の
/// ファイルは読まない（その中の行は、どれも since_seq 以下）。
///
/// 履歴のフォルダは `.vault` のハンドルを基準に、ファイルはそのフォルダのハンドルを基準に、リンクをたどらずに開く。
/// ファイルの名前の一覧は、持っている履歴のフォルダのハンドルで列挙する。
pub(crate) fn read_since(
    root: &Root,
    vault: &Pass,
    since_seq: u64,
    limit: usize,
) -> Result<(Vec<JournalEntry>, bool)> {
    test_hook!(Step::Open(Place::Journal));
    let Some(mut dir) = guard::open_place(
        root,
        vault,
        &ManagedName::Journal,
        false,
        "履歴のフォルダが、ふつうのフォルダでない",
    )?
    else {
        return Ok((Vec::new(), false));
    };
    let files = journal_files(&mut dir)?;
    let skip = files
        .iter()
        .skip(1)
        .take_while(|(first, _)| *first <= since_seq.saturating_add(1))
        .count();
    let mut entries = Vec::new();
    let mut line = Vec::new();
    for (_, name) in &files[skip..] {
        let mut reader = BufReader::new(guard::open_journal_for_reading(&dir, name)?);
        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .map_err(|e| VaultError::io("履歴を読めない", &e))?;
            if read == 0 {
                break;
            }
            match line_seq(&line) {
                Some(seq) if seq > since_seq => {}
                _ => continue,
            }
            let Ok(entry) = serde_json::from_slice::<JournalEntry>(&line) else {
                continue;
            };
            if !is_vault_path(entry.path.as_deref()) || !is_vault_path(entry.to.as_deref()) {
                continue;
            }
            if entries.len() >= limit {
                return Ok((entries, true));
            }
            entries.push(entry);
        }
    }
    Ok((entries, false))
}

/// 履歴の path・to が、保管庫の中の相対パスとして正しいか（ないものは正しいとする）。
fn is_vault_path(path: Option<&str>) -> bool {
    path.is_none_or(|path| RelPath::parse(path).is_ok())
}

/// 行の seq。書いた形（`{"seq":<数字>,`）なら、そこだけを読む。ほかの形なら、JSON として読む。
fn line_seq(line: &[u8]) -> Option<u64> {
    if let Some(rest) = line.strip_prefix(b"{\"seq\":") {
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0
            && matches!(rest.get(digits), Some(b',' | b'}'))
            && let Some(seq) = std::str::from_utf8(&rest[..digits])
                .ok()
                .and_then(|text| text.parse().ok())
        {
            return Some(seq);
        }
    }
    serde_json::from_slice::<LineHead>(line)
        .ok()
        .map(|head| head.seq)
}

fn ends_with_newline(file: &mut File, len: u64) -> Result<bool> {
    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(len - 1))
        .and_then(|_| file.read_exact(&mut last))
        .map_err(|e| VaultError::io("履歴を読めない", &e))?;
    Ok(last[0] == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::Actor;

    #[test]
    fn entries_serialize_like_the_adr() {
        let entry = JournalEntry {
            seq: 7,
            ts: "2026-11-16T00:00:00.000Z".to_owned(),
            actor: Actor::user("app"),
            op: Op::Write,
            path: Some("a/b.md".to_owned()),
            to: None,
            trash_id: Some("1-2-3".to_owned()),
            size: Some(5),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["seq"], 7);
        assert_eq!(json["op"], "write");
        assert_eq!(json["actor"]["kind"], "user");
        assert_eq!(json["path"], "a/b.md");
        assert_eq!(json["trash_id"], "1-2-3");
        assert!(json.get("to").is_none());
        let back: JournalEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn op_names_match_the_adr() {
        let names = [
            (Op::Init, "init"),
            (Op::Mkdir, "mkdir"),
            (Op::Write, "write"),
            (Op::WriteFailed, "write_failed"),
            (Op::Delete, "delete"),
            (Op::Move, "move"),
            (Op::Restore, "restore"),
            (Op::ExternalCreate, "external_create"),
            (Op::ExternalModify, "external_modify"),
            (Op::ExternalDelete, "external_delete"),
            (Op::ExternalRename, "external_rename"),
            (Op::IndexRebuilt, "index_rebuilt"),
        ];
        for (op, name) in names {
            assert_eq!(serde_json::to_value(op).unwrap(), name);
        }
    }

    #[test]
    fn seq_overflow_is_not_a_vault() {
        assert_eq!(next_after(1).unwrap(), 2);
        assert_eq!(
            next_after(u64::MAX).unwrap_err().code(),
            ErrorCode::NotAVault
        );
    }

    #[test]
    fn journal_paths_must_be_vault_paths() {
        assert!(is_vault_path(None));
        assert!(is_vault_path(Some("a/b.txt")));
        for bad in [
            r"C:\Windows",
            "C:/Windows",
            "../x",
            "a/../b",
            r"\\server\share",
            "/etc/passwd",
        ] {
            assert!(!is_vault_path(Some(bad)), "{bad}");
        }
    }

    #[test]
    fn line_seq_reads_the_written_form_and_other_forms() {
        assert_eq!(line_seq(br#"{"seq":42,"ts":"x"}"#), Some(42));
        assert_eq!(line_seq(br#"{"ts":"x","seq":7}"#), Some(7));
        assert_eq!(line_seq(b"{\"seq\":12"), None, "書いている途中の行");
        assert_eq!(line_seq(b"garbage"), None);
    }

    #[test]
    fn rotation_size_matches_the_adr() {
        assert_eq!(MAX_JOURNAL_FILE_BYTES, 8 * 1024 * 1024);
    }
}
