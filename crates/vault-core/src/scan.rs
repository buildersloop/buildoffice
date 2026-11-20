//! 走査と vault_changes。
//!
//! - 見張り役は作らない。走査は、vault_changes を `rescan:true` で呼んだときだけ行う。
//!   前の走査からの間隔（下）が過ぎていない rescan は、走査せずに履歴だけを返す（同じ `Vault` の中で覚える）。
//! - 保管庫を歩いて比べるあいだは、ロックを持たない（フォルダが多いと、歩くのに何秒もかかるため）。
//!   履歴と索引に書くときだけロックを取る。歩き始めたときから履歴か索引が変わっていたら（歩くあいだにコアが
//!   書いた）、結果を捨てずに、歩き始めより後の履歴の行のパス（コアが書いたパス）だけを、ロックの中で今の状態に
//!   調べ直して合わせ、ロックの中で読み直した索引と比べる。歩くあいだの外の変更は、
//!   その回か次の走査で見つかる。
//! - 合わせ直せない（履歴の行が読めない・欠けている、調べ直すパスを開けない）ときは、その回を捨てる。続けて
//!   捨てるのは2回まで（`MAX_DISCARDED_SCANS`）。3回目は、合わせ直せないパスを今の索引の値のまま（unknown）に
//!   して「次の走査で調べ直す」印を付け、終える（行が欠けていた・索引がなくて印を付けられないなら、記録せずに
//!   未完了として終える）。
//! - ロックの中の仕事には上限がある：調べ直すパスの数（`MAX_LOCKED_RECHECKS`）と時間（`MAX_LOCKED_SCAN_TIME`。
//!   調べ直しと比べ直しの合計）。超えたら、ロックを外して外で合わせ直す（1回捨てた回に数える）。調べ直しでは
//!   試し直さない（使用中・拒否は、すぐ調べ直せないとする。途中のフォルダを開けなければ、その下もまとめて）。
//! - 走査の間隔は、最短の間隔（2 秒）と、前の走査（捨てたもの・エラーで終わったものも）にかかった時間の 10 倍
//!   （10 分まで）の長いほう。--read-only の MCP から繰り返し呼ばれても、根を持って歩くのは 1 割ほどまで。
//! - 書くときは、1つの `Txn` で行う：履歴のファイルを開いて seq を決め、見つけた変更を1回の sync で書き、
//!   書けたときだけ索引を1回で置き換える（履歴に書けなければ、索引を古いままにして、次の走査で同じ変更をもう一度見つける）。
//! - 再帰でなく、明示のスタックで歩く。どの深さでも `.vault`（大文字小文字を問わない）には入らない。
//!   リンク（シンボリックリンク・ジャンクション）は、リンクそのものを `link` の項目として扱い、中へはたどらない。
//! - 歩き方：根を開いて確かめ、フォルダはハンドル（HOLD_DIR）で持って
//!   そのハンドルで列挙する。下のフォルダは、持っている親を基準に、リンクをたどらずに開く（リンクに替わっていれば、
//!   その値で記録して中へは進まない）。祖先は、子を歩き終えるまで持つ（名前を変えられない。R6a）ので、下のフォルダ
//!   ごとには最終のパスを確かめない。下のフォルダが使用中・拒否で開けなければ、試し直さずに unknown にする
//! - 上限（64段・根を含めて260字）を超えるパスと、ツールで扱えない名前（UTF-8 にできない・文字列の規則に通らない）は、
//!   対象にしない（中へも進まない）。それぞれの数だけを返す。
//! - 索引と比べる。キーは実際の名前で、大文字小文字は索引と同じく、そのまま比べる：
//!   - 名前の変更：同じ回の「削除」と「作成」が、種類・大きさ・更新時刻で一致するもの
//!     （フォルダは、更新時刻が一致するか、中身（名前・種類・ファイルの大きさと更新時刻）が同じもの）
//!   - 削除：木のいちばん上だけを書く（中は含まれる）。作成・更新：1つずつ書く
//!   - フォルダの大きさと更新時刻の違いは、更新としない（中のファイルの作成・削除で変わるため）
//!   - 読めないフォルダの中は、索引の値を（名前の変更を当てたあとの名前で）引き継ぐ
//! - 索引が読めない（ない・壊れている）ときは、作り直して `index_rebuilt` だけを記録する（差分は出さない）。
//!   `index_rebuilt` は索引を置き換える回に毎回書く（直前の行が `index_rebuilt` でも書く。索引を置き換えたことを、
//!   同じときに歩いているほかの走査が履歴の行で知るため）。索引を書けない状態が続くと、走査の間隔ごとに1行増える。
//! - ハードリンク：外の名前から書き換えても、中のフォルダの一覧の値は古いまま（R22）。
//!   走査で開いたファイルは、同じハンドルでリンクの数も読み、2以上なら印（プロセスの中だけ。索引には入れない）に
//!   入れて、次の走査からは一覧の値が索引と同じでも開いて読み直す。まだ分からないリンクは、回り番（ファイルを
//!   `LINK_SWEEP_PARTS` に分け、走査ごとに1つの番のファイルを開く）で見つける。
//! - 完全な変更の検知ではない：大きさも更新時刻も変えずに中身だけを変えたものは見つからない。ハードリンクの別の
//!   名前から書き換えたものは、見つかるまでに何回かの走査がかかることがある。
//! - 返り値とエラーの文には、根の絶対パスを入れない。

#[cfg(feature = "test-support")]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Bound;
use std::rc::Rc;
use std::time::Instant;

use serde::Serialize;

use crate::actor::{Actor, ActorKind};
use crate::error::{ErrorCode, Result, VaultError};
use crate::fsutil::EntryKind;
use crate::guard::{
    self, At, ChildActual, ChildDir, ChildInfo, DirStep, Held, Listed, ManagedName, ParentDir,
    Pass, Root, parent_of,
};
use crate::index::{self, Index, IndexEntry};
use crate::journal::{self, JournalEntry, Op, Record};
use crate::ops::Txn;
use crate::relpath::{RESERVED_NAME, RelPath, is_valid_component};
use crate::vault::{
    MAX_DISCARDED_SCANS, MAX_LOCKED_RECHECKS, MAX_LOCKED_SCAN_TIME, ScanEnd, Vault, scan_now,
};

/// vault_changes の limit の既定値。
pub const DEFAULT_CHANGES_LIMIT: u64 = 200;
/// vault_changes の limit の上限。
pub const MAX_CHANGES_LIMIT: u64 = 10_000;
/// 名前の変更を探す回数の上限（名前を変えた木の中で、さらに名前を変えたものを見つけるために繰り返す）。
const MAX_RENAME_ROUNDS: usize = 8;
/// 走査で見つけた変更の actor の id（`{kind:"external", id:"fs"}`）。
const SCAN_ACTOR_ID: &str = "fs";

#[cfg(feature = "test-support")]
thread_local! {
    /// テストだけ：歩き終わったあと、ロックを取る前に1回だけ呼ぶ関数（このスレッドの次の走査で使う）。
    pub(crate) static AFTER_WALK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// vault_changes の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesResult {
    /// seq が since_seq より大きい履歴の行（古い順）。
    pub changes: Vec<JournalEntry>,
    /// 次に since_seq に渡す値（返した最後の seq。返すものがなければ since_seq のまま）。
    pub next_seq: u64,
    /// limit で切った（まだある）か。
    pub truncated: bool,
    /// この呼び出しで走査したか。rescan:false・前の走査からの間隔（2 秒と、前の走査にかかった時間の 10 倍
    /// （10 分まで）の長いほう）が過ぎていない・歩くあいだのコアの書き込みを合わせ直せずに結果を捨てた
    /// （続けて2回まで）・未完了で終えた（`scan_incomplete`）ときは false（そのときの外での変更は、次の走査で
    /// 見つかる）。
    pub scanned: bool,
    /// この呼び出しの走査が、未完了で終わったか（続けて捨てる上限の回で、履歴の行が欠けていた、または索引がなくて
    /// 合わせ直せなかったので、何も記録せず索引も変えずに終えた。scanned は false）。間隔が来ていない・捨てたときと
    /// 見分けるため。
    pub scan_incomplete: bool,
    /// 続けて未完了で終わった走査の回数（この呼び出しの分も。走査を終えたら 0 に戻る）。増え続けるなら、
    /// 履歴を書き換え続けている相手がいて、外での変更を記録できていない。
    pub incomplete_scans: u32,
    /// 走査で対象にしなかった項目のうち、上限（64段・260字）を超えるものの数（その中は数えない）。走査しなければ 0。
    pub skipped_over_limit: u64,
    /// 走査で対象にしなかった項目のうち、ツールで扱えない名前のものの数（その中は数えない）。走査しなければ 0。
    pub skipped_invalid_names: u64,
}

/// 保管庫の中の相対パス → 項目。
type Entries = BTreeMap<String, IndexEntry>;

/// 歩いた結果。
#[derive(Clone)]
struct Walked {
    entries: Entries,
    /// 調べられなかったフォルダ・項目（索引にある値を、そのまま使う）。
    unknown: Vec<String>,
    /// 上限を超えるので対象にしなかった項目の数。
    over_limit: u64,
    /// ツールで扱えない名前なので対象にしなかった項目の数。
    invalid_names: u64,
    /// この回に開いて読み、ほかの名前とハードリンクでつながっていたファイル。
    linked: BTreeSet<String>,
    /// この回に値を読めなかったファイルと、ロックの中で索引の値のままにしたパス（その下の前の印を残す）。
    not_read: Vec<String>,
    /// 前の印と回り番（一覧の値が索引と同じでも開き直すファイルの決め方）。
    sweep: Rc<LinkSweep>,
}

/// 走査で、一覧の値が索引と同じでも開いてリンクの数と値を読み直すファイルの決め方。
struct LinkSweep {
    /// 前の走査までに、ほかの名前とつながっていると分かったファイル（毎回開く）。
    prev: BTreeSet<String>,
    /// 回り番：ファイルを `parts` に分け、この回は `slot` 番のものを開く（0 なら回り番をしない）。
    parts: u32,
    slot: u32,
}

impl LinkSweep {
    fn wants(&self, rel: &str) -> bool {
        self.prev.contains(rel) || (self.parts > 0 && sweep_part(rel, self.parts) == self.slot)
    }
}

/// 回り番の番（std の DefaultHasher。名前を選べる者が同じ番に集めても、その番の走査が最初の走査と同じ重さに
/// なるだけ）。
fn sweep_part(rel: &str, parts: u32) -> u32 {
    let mut hasher = DefaultHasher::new();
    rel.hash(&mut hasher);
    u32::try_from(hasher.finish() % u64::from(parts)).unwrap_or(0)
}

/// この回の印（歩き終えた回なら、捨てた回・未完了の回も）：この回に開いて読み、つながっていたファイルと、前の印の
/// うち、この回に値を読めなかった所（開けないフォルダの下・開けないファイル・ロックの中で索引の値のままにした
/// パス）のもの。
fn link_marks(walked: &Walked) -> BTreeSet<String> {
    let mut marks = walked.linked.clone();
    let prev = &walked.sweep.prev;
    for held in walked.unknown.iter().chain(&walked.not_read) {
        if held.is_empty() {
            marks.extend(prev.iter().cloned());
            continue;
        }
        let prefix = format!("{held}/");
        for mark in prev.range::<str, _>((Bound::Included(held.as_str()), Bound::Unbounded)) {
            if !mark.starts_with(held.as_str()) {
                break;
            }
            if mark == held || mark.starts_with(&prefix) {
                marks.insert(mark.clone());
            }
        }
    }
    marks
}

/// 走査で対象にしなかった項目の数。
#[derive(Debug, Clone, Copy, Default)]
struct Skipped {
    over_limit: u64,
    invalid_names: u64,
}

/// 書く前に、歩き始めたときから変わっていないことを確かめる印（履歴の最後のファイルとその大きさ、最後に読める行の
/// seq、索引の大きさと更新時刻）。
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    journal: Option<(u64, u64)>,
    index: Option<(u64, i64)>,
    /// 履歴の最後に読める行の seq（履歴がなければ 0）。歩くあいだにコアが書いた行は、これより後の行。
    last_seq: u64,
}

/// 歩いた結果を索引と比べて用意した、書くもの。
struct Prepared {
    records: Vec<(Op, Record)>,
    /// 置き換える索引（書く形にしたもの）。変わらなければ None。
    encoded: Option<Vec<u8>>,
}

/// 合わせ直せなかったもの。
#[derive(Default)]
struct Unresolved {
    /// 調べ直せなかったパス（開けない・使用中など）。
    paths: BTreeSet<String>,
    /// 履歴の行が読めない・欠けている・パスのない行がある（ほかの走査の index_rebuilt など）：コアやほかの走査が
    /// どのパスを書いたか分からない。
    gap: bool,
}

impl Unresolved {
    fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.gap
    }
}

/// 履歴の行から集めた、書かれたパス（path・to）。
struct Touched {
    paths: BTreeSet<String>,
    /// 行が読めない・欠けている・パスのない行がある。
    gap: bool,
}

/// 調べ直す所（ロックの外か中か）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Within {
    /// ロックの外：フォルダなら、その下も歩き直す。
    Outside,
    /// ロックの中：下は歩き直さない（今の索引の値のまま、印を付ける）。
    Lock,
}

/// 1つのパスを調べ直した結果。
enum Recheck {
    Done,
    /// フォルダの下も歩き直した（実際の名前）。
    Walked(String),
    /// 調べ直せなかった（開けない・使用中、または歩いた結果の親の大文字小文字が古い）。
    Failed,
    /// 途中のフォルダ（このパス。履歴のパスの形）を開けなかった：その下のパスも調べ直せない。
    ParentFailed(String),
}

/// 調べ直した結果。
struct Rechecked {
    /// 調べ直せなかったパス。
    failed: BTreeSet<String>,
    /// 時間の上限に当たって、調べ直さなかったパス（ロックの中だけ）。
    rest: BTreeSet<String>,
}

/// 調べ直しで開いた途中のフォルダ（前のパスと親が同じなら、使い回す。パスは BTreeSet の順に調べる）。
#[derive(Default)]
struct Chain {
    names: Vec<String>,
    dirs: Vec<Pass>,
}

/// 走査の結果のうち、返り値に入れるもの。
#[derive(Default)]
struct ScanReport {
    /// 走査を終えた（記録した）なら、対象にしなかった項目の数。
    skipped: Option<Skipped>,
    /// 未完了で終えた。
    incomplete: bool,
}

/// 1回の走査の終わり方。
enum Outcome {
    /// 終えた（記録した）。印を付けたパス。
    Done(Skipped, BTreeSet<String>),
    /// 合わせ直せずに捨てた。続けて捨てた回数。
    Discarded(u32),
    /// 続けて捨てる上限の回で、履歴の行が欠けていた・索引がなくて印を付けられない：記録せず、索引も変えずに終えた
    /// （未完了。scanned:false）。
    Incomplete(BTreeSet<String>),
}

/// 大文字小文字をそろえた形（Windows では小文字。ほかは元のまま。索引の `remove_tree_any_case` と同じ比べ方）。
fn fold(key: &str) -> String {
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key.to_owned()
    }
}

/// キーを、大文字小文字をそろえた形で引けるようにしたもの（Windows で、大文字小文字だけが違うキーとその下を
/// 探すため。歩いた結果に使うときは、合わせるあいだ一緒に直す）。
struct CaseKeys(BTreeMap<String, BTreeSet<String>>);

impl CaseKeys {
    fn new(entries: &Entries) -> Self {
        let mut keys = Self(BTreeMap::new());
        for key in entries.keys() {
            keys.insert(key);
        }
        keys
    }

    fn insert(&mut self, key: &str) {
        self.0.entry(fold(key)).or_default().insert(key.to_owned());
    }

    /// `key` だけを除く（名前の変更の組を当てたとき）。
    fn remove_key(&mut self, key: &str) {
        let folded = fold(key);
        if let Some(keys) = self.0.get_mut(&folded) {
            keys.remove(key);
            if keys.is_empty() {
                self.0.remove(&folded);
            }
        }
    }

    /// `rel` と大文字小文字だけが違うキー（`rel` も）とその下の、そろえた形。
    fn tree(&self, rel: &str) -> Vec<String> {
        let folded = fold(rel);
        let prefix = format!("{folded}/");
        self.0
            .range::<str, _>((Bound::Included(folded.as_str()), Bound::Unbounded))
            .take_while(|(key, _)| key.starts_with(folded.as_str()))
            .filter(|(key, _)| **key == folded || key.starts_with(&prefix))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// `rel` と大文字小文字だけが違うキー（`rel` も）とその下を、`entries` から除く。
    fn remove_tree(&mut self, entries: &mut Entries, rel: &str) {
        for folded in self.tree(rel) {
            for key in self.0.remove(&folded).unwrap_or_default() {
                entries.remove(&key);
            }
        }
    }

    /// `rel` と大文字小文字だけが違うキー（`rel` も）とその下の、元のキー。
    fn keys_in_tree(&self, rel: &str) -> Vec<String> {
        self.tree(rel)
            .iter()
            .flat_map(|folded| self.0[folded].iter().cloned())
            .collect()
    }

    /// `entries` の `top` とその下のキーを足す（歩き直したあと）。
    fn add_tree(&mut self, entries: &Entries, top: &str) {
        let prefix = format!("{top}/");
        let keys: Vec<String> = entries
            .range::<str, _>((Bound::Included(top), Bound::Unbounded))
            .take_while(|(key, _)| key.starts_with(top))
            .filter(|(key, _)| *key == top || key.starts_with(&prefix))
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.insert(&key);
        }
    }

    /// `key` と大文字小文字だけが違う、別のキーがあるか。
    fn has_other_case(&self, key: &str) -> bool {
        self.0
            .get(&fold(key))
            .is_some_and(|keys| keys.iter().any(|k| k != key))
    }
}

/// 印を付けるパス（とその下。大文字小文字を問わない）を、歩いた結果から除き、今の索引の値で置き換える
/// （調べ直さずに、今の索引の値のまま次の走査に回す）。
fn mark_unknown(
    walked: &mut Walked,
    case: &mut CaseKeys,
    index: &Entries,
    paths: &BTreeSet<String>,
) {
    if paths.is_empty() {
        return;
    }
    // この回に読めなかったパス：その下の前のリンクの印を残す
    walked.not_read.extend(paths.iter().cloned());
    let index_case = CaseKeys::new(index);
    for path in paths {
        case.remove_tree(&mut walked.entries, path);
        for key in index_case.keys_in_tree(path) {
            if let Some(entry) = index.get(&key) {
                walked.entries.insert(key.clone(), *entry);
                case.insert(&key);
            }
        }
    }
}

impl Stamp {
    /// 根から開き直した `.vault` を基準に読む（履歴のファイルの一覧は、履歴のフォルダのハンドルで列挙する。
    /// 値は、ファイルを属性だけで開いて読む）。
    fn read(vault: &Vault) -> Result<Self> {
        let root = vault.op_root()?;
        let vault_dir = root.open_vault_dir()?;
        let (journal, last_seq) = match guard::open_place(
            &root,
            &vault_dir,
            &ManagedName::Journal,
            false,
            "履歴のフォルダが、ふつうのフォルダでない",
        )? {
            Some(mut dir) => match journal::journal_files(&mut dir)?.pop() {
                Some((first_seq, name)) => {
                    let size = guard::managed_info(dir.parent(), &name)?.map_or(0, |i| i.size);
                    let last_seq = journal::last_seq_of(&dir, &name, first_seq)?;
                    (Some((first_seq, size)), last_seq)
                }
                None => (None, 0),
            },
            None => (None, 0),
        };
        let index = guard::managed_info(vault_dir.parent(), &ManagedName::IndexJson)?
            .map(|i| (i.size, i.mtime_ns));
        Ok(Self {
            journal,
            index,
            last_seq,
        })
    }
}

/// 走査で持っているフォルダ。子（とその下）を歩き終えるまで、親を持ち続ける（祖先の名前を変えさせない）。
struct Node {
    dir: Held,
    depth: usize,
    _parent: Option<Rc<Node>>,
    /// テストだけ：持っていた時間を測る。
    #[cfg(feature = "test-support")]
    _hold: crate::hooks::HoldTimer,
}

impl Node {
    fn new(dir: Held, depth: usize, parent: Option<Rc<Node>>) -> Rc<Self> {
        Rc::new(Self {
            dir,
            depth,
            _parent: parent,
            #[cfg(feature = "test-support")]
            _hold: crate::hooks::HoldTimer::start(depth),
        })
    }
}

/// これから開いて歩く、下のフォルダ。
struct Pending {
    parent: Rc<Node>,
    name: OsString,
    rel: String,
}

impl Vault {
    /// vault_changes：`rescan` なら先に走査して、外での変更を履歴に入れる。そのあと、seq が `since_seq` より
    /// 大きい履歴の行を、古い順に `limit` 件まで返す。
    ///
    /// 読み取り専用で開いた保管庫でも、rescan は `.vault/` の索引と履歴に書く。
    pub fn changes(&self, since_seq: u64, limit: u64, rescan: bool) -> Result<ChangesResult> {
        if !(1..=MAX_CHANGES_LIMIT).contains(&limit) {
            return Err(VaultError::new(
                ErrorCode::InvalidArgument,
                format!("limit は 1 から {MAX_CHANGES_LIMIT} まで"),
            ));
        }
        let scan = if rescan {
            self.rescan()?
        } else {
            ScanReport::default()
        };
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let root = self.op_root()?;
        let vault = root.open_vault_dir()?;
        let (changes, truncated) = journal::read_since(&root, &vault, since_seq, limit)?;
        let next_seq = changes.last().map_or(since_seq, |entry| entry.seq);
        let skipped = scan.skipped.unwrap_or_default();
        Ok(ChangesResult {
            changes,
            next_seq,
            truncated,
            scanned: scan.skipped.is_some(),
            scan_incomplete: scan.incomplete,
            incomplete_scans: self.scan_incompletes(),
            skipped_over_limit: skipped.over_limit,
            skipped_invalid_names: skipped.invalid_names,
        })
    }

    /// 走査する。走査しなかった（前の走査から間隔が過ぎていない・合わせ直せずに結果を捨てた・未完了で終えた）
    /// ときは skipped が None。走査を始めたら、捨てた・未完了・エラーで終わったときも、間隔に数える
    fn rescan(&self) -> Result<ScanReport> {
        if !self.rescan_due() {
            return Ok(ScanReport::default());
        }
        let started = self.scan_started();
        let result = self.scan_once();
        test_hook!(Step::AfterScan);
        let (value, end) = match result {
            Ok(Outcome::Done(skipped, marks)) => (
                Ok(ScanReport {
                    skipped: Some(skipped),
                    incomplete: false,
                }),
                ScanEnd::Finished(marks),
            ),
            Ok(Outcome::Incomplete(marks)) => (
                Ok(ScanReport {
                    skipped: None,
                    incomplete: true,
                }),
                ScanEnd::Incomplete(marks),
            ),
            Ok(Outcome::Discarded(count)) => (Ok(ScanReport::default()), ScanEnd::Discarded(count)),
            Err(e) => (Err(e), ScanEnd::Failed),
        };
        self.scan_ended(started, end);
        value
    }

    /// 1回の走査。
    ///
    /// 歩くあいだのコアの書き込み（とほかの走査の記録）は、まずロックの外で合わせる（重い調べ直しと比べ直し。
    /// フォルダはその下も歩き直す）。ロックの中では、そのあとに増えた行と、外で合わせ直せなかったパスだけを
    /// 合わせ、下は歩き直さない（フォルダの下は今の索引の値のまま、印を付ける）。調べ直す数が
    /// `MAX_LOCKED_RECHECKS` を超えたら、ロックを外して外で合わせ直す（1回捨てた回に数える）。
    /// ロックの中の時間が `MAX_LOCKED_SCAN_TIME` を超えたとき、索引がなくて印を付けられないときも同じ。
    /// 合わせ直せない（履歴の行が欠けている・パスを開けない）なら捨てる。続けて捨てるのは
    /// `MAX_DISCARDED_SCANS` 回まで。次の回は、開けないパスと多すぎるパス（時間の上限で調べ直さなかったパス）に
    /// 印を付けて終える。行が欠けていた・索引がなくて印を付けられないなら、記録せず索引も変えずに、未完了として
    /// 終える。
    fn scan_once(&self) -> Result<Outcome> {
        // 1. ロックを持たずに歩く。印（Stamp）は、索引を読むより前に取る（索引を読んだあとにコアの書き込みが
        //    入っても、その行は印の seq より後なので、合わせ直す。逆の順だと、コアの書き込みを外での変更として
        //    二重に記録する）
        let before = Stamp::read(self)?;
        // 索引は、根から開き直した .vault のハンドルを基準に、リンクをたどらずに開く（歩く前に閉じる）
        let loaded = self.load_index()?;
        test_hook!(Step::AfterScanIndexLoaded);
        let mut walked = self.walk(loaded.as_ref().map(Index::entries))?;
        let skipped = Skipped {
            over_limit: walked.over_limit,
            invalid_names: walked.invalid_names,
        };

        #[cfg(feature = "test-support")]
        if let Some(hook) = AFTER_WALK.with(|hook| hook.borrow_mut().take()) {
            hook();
        }

        // 歩き終えた回は（捨てた回・未完了の回も）、リンクの印をこの回のものに置き換える。エラーで終わった回は
        // 前の印のまま
        let outcome = self.reconcile(before, loaded, &mut walked, skipped);
        if outcome.is_ok() {
            self.set_link_marks(link_marks(&walked));
        }
        outcome
    }

    /// 歩いた結果（`walked`）を、歩くあいだのコアの書き込みと合わせて書く（`scan_once` の 2〜4）。
    fn reconcile(
        &self,
        before: Stamp,
        loaded: Option<Index>,
        walked: &mut Walked,
        skipped: Skipped,
    ) -> Result<Outcome> {
        let before_seq = before.last_seq;
        let mut used = self.scan_discards();
        // walked は、この印の時点まで合わせてある。index_now は、その時点の索引
        let mut synced = before;
        let mut index_now = loaded;
        let mut case: Option<CaseKeys> = None;
        let mut pending = Unresolved::default();
        loop {
            // 2. ロックの外で、synced から今までの行を合わせ、今の索引と比べて用意する（行が欠けていたら、
            //    歩き始めから読み直す）
            let mid = Stamp::read(self)?;
            if mid != synced || !pending.is_empty() {
                let from = if pending.gap {
                    before_seq
                } else {
                    synced.last_seq
                };
                let current = self.load_index()?;
                let touched = self.collect_touched(from, mid.last_seq)?;
                let case = case.get_or_insert_with(|| CaseKeys::new(&walked.entries));
                let mut paths = std::mem::take(&mut pending.paths);
                paths.extend(touched.paths);
                pending.gap = touched.gap;
                pending.paths = if touched.gap {
                    paths
                } else {
                    let mut unused = BTreeSet::new();
                    self.recheck_all(
                        paths,
                        walked,
                        case,
                        current.as_ref().map(Index::entries),
                        Within::Outside,
                        &mut unused,
                        None,
                    )?
                    .failed
                };
                index_now = current;
                synced = mid;
            }
            let prepared = if pending.is_empty() {
                Some(self.prepare(index_now.as_ref(), walked.entries.clone(), &walked.unknown)?)
            } else {
                None
            };

            // 3. 書くときだけロックを取る。増えていなければ、ロックの外で用意したものをそのまま書く
            test_hook!(Step::BeforeScanLock);
            let lock = self.lock()?;
            // ロックの中の時間の上限（走査の時計。テストでは差し込み口で進められる）
            let deadline = scan_now() + MAX_LOCKED_SCAN_TIME;
            test_hook!(Step::BeforeReconcile);
            let now = Stamp::read(self)?;
            if now == synced
                && let Some(prepared) = prepared
            {
                self.write_scan(&lock, prepared)?;
                return Ok(Outcome::Done(skipped, BTreeSet::new()));
            }

            // 4. ロックの中：synced のあとに増えた行と、外で合わせ直せなかったパスだけ。下は歩き直さない。
            //    数か時間の上限を超えた・索引がなくて印を付けられないなら、ロックを外して、外で合わせ直す
            //    （1回捨てた回に数える。続けて捨てる上限に当たっていたら、外へは戻らない）
            let at_limit = used >= MAX_DISCARDED_SCANS;
            let from = if pending.gap {
                before_seq
            } else {
                synced.last_seq
            };
            let touched = self.collect_touched(from, now.last_seq)?;
            let case = case.get_or_insert_with(|| CaseKeys::new(&walked.entries));
            let mut paths = pending.paths.clone();
            paths.extend(touched.paths);
            let mut to_mark = BTreeSet::new();
            let mut failed = BTreeSet::new();
            if !touched.gap {
                if paths.len() > MAX_LOCKED_RECHECKS {
                    if !at_limit {
                        // 多すぎる：ロックを外して、ロックの外で合わせ直す
                        drop(lock);
                        used += 1;
                        continue;
                    }
                    // 上限の回：調べ直さずに、今の索引の値のまま印を付ける
                    to_mark = paths;
                } else {
                    let rechecked = self.recheck_all(
                        paths,
                        walked,
                        case,
                        None,
                        Within::Lock,
                        &mut to_mark,
                        Some(deadline),
                    )?;
                    failed = rechecked.failed;
                    if !rechecked.rest.is_empty() {
                        if !at_limit {
                            // 時間の上限：ロックを外して、ロックの外で合わせ直す
                            drop(lock);
                            used += 1;
                            continue;
                        }
                        // 上限の回：残りは調べ直さずに、今の索引の値のまま印を付ける
                        to_mark.extend(rechecked.rest);
                    }
                }
            }
            if touched.gap || !failed.is_empty() {
                if !at_limit {
                    return Ok(Outcome::Discarded(used + 1));
                }
                if touched.gap {
                    // どのパスを書いたか分からない：外での変更と取り違えないように、この回は何も記録せず、索引も
                    // 変えずに、未完了として終える（保管庫の全体に印を付ける。次の走査で比べ直す）
                    return Ok(Outcome::Incomplete(BTreeSet::from([String::new()])));
                }
                // 調べ直せなかったパスは、今の索引の値のまま印を付ける
                to_mark.extend(failed);
            }
            // 比べる索引は、ロックの中で読み直す（コアの書き込みが入ったもの）
            let current = self.load_index()?;
            match &current {
                Some(current) => mark_unknown(walked, case, current.entries(), &to_mark),
                None if !to_mark.is_empty() => {
                    // 索引がない：印を付けるパスの下を、索引の値で置き換えられない（そのままだと、作り直した索引に
                    // 入らず、次の走査で外での作成と記録する）。外で下まで歩き直す。上限の回は、未完了で終える
                    if !at_limit {
                        drop(lock);
                        used += 1;
                        continue;
                    }
                    return Ok(Outcome::Incomplete(BTreeSet::from([String::new()])));
                }
                None => {}
            }
            if at_limit {
                // この回で終える：歩いた値は写さずに渡す（リンクの印に使う所は walked に残る）
                let entries = std::mem::take(&mut walked.entries);
                let prepared = self.prepare(current.as_ref(), entries, &walked.unknown)?;
                self.write_scan(&lock, prepared)?;
                return Ok(Outcome::Done(skipped, to_mark));
            }
            let prepared =
                self.prepare(current.as_ref(), walked.entries.clone(), &walked.unknown)?;
            if scan_now() >= deadline {
                // 時間の上限（比べ直しまでの合計）：書かずにロックを外して、ロックの外で用意し直す
                drop(lock);
                used += 1;
                continue;
            }
            self.write_scan(&lock, prepared)?;
            return Ok(Outcome::Done(skipped, to_mark));
        }
    }

    /// 走査の結果を書く（ロックを持っていること）：履歴に書き（1回の sync）、書けたら索引を置き換える。
    fn write_scan(&self, lock: &crate::vault::VaultLock, prepared: Prepared) -> Result<()> {
        let mut txn = Txn::begin(self, lock)?;
        let actor = Actor::new(ActorKind::External, SCAN_ACTOR_ID);
        // index_rebuilt は、索引を置き換える回に毎回書く（直前が index_rebuilt でも書く。書かないと、同じときに歩いて
        // いるほかの走査が、索引の変わりを履歴の行で合わせられず、作り直した索引に入ったものを外での削除と記録する）。索引を書けない状態が続くと、走査の間隔ごとに1行増える
        for (op, record) in prepared.records {
            txn.record(&actor, op, record)?;
        }
        txn.finish_replacing_index(prepared.encoded.as_deref())
    }

    /// 索引を、根から開き直した .vault のハンドルを基準に、リンクをたどらずに読む（読めなければ None）。
    fn load_index(&self) -> Result<Option<Index>> {
        let root = self.op_root()?;
        let vault = root.open_vault_dir()?;
        index::check_file(&vault)?;
        Ok(index::load(&vault))
    }

    /// 歩いた結果を索引（`old`）と比べ、書く履歴の行と、置き換える索引を用意する。
    fn prepare(
        &self,
        old: Option<&Index>,
        entries: Entries,
        unknown: &[String],
    ) -> Result<Prepared> {
        let (records, replacement) = match old {
            None => {
                // 索引が読めない：作り直す。前の状態がわからないので、差分は出さない
                let rebuilt = Index::from_entries(entries);
                (vec![(Op::IndexRebuilt, Record::default())], Some(rebuilt))
            }
            Some(old) => {
                let old = old.entries();
                // 上限を超えるもの（保管庫のフォルダが深い場所へ移った場合など）は、削除とせずに索引から外す
                let comparable: Entries = old
                    .iter()
                    .filter(|(path, _)| self.fits(path))
                    .map(|(path, entry)| (path.clone(), *entry))
                    .collect();
                let mut new = entries;
                let records = diff(comparable, &mut new, unknown);
                // フォルダの更新時刻だけが変わったときも、索引を新しくする（次の名前の変更の判定に使う）
                let changed = new != *old;
                (records, changed.then(|| Index::from_entries(new)))
            }
        };
        // 書いても次に読めない大きさなら、何も書かずにエラー（index_rebuilt を繰り返さない）
        let encoded = replacement.as_ref().map(index::encode).transpose()?;
        Ok(Prepared { records, encoded })
    }

    /// 履歴の `from` より後、`to` までの行のパス（path・to）を集める。行が読めない・欠けている（seq が1つずつ
    /// 続かない）・パスのない行がある（ほかの走査の index_rebuilt など。索引がどう変わったか分からない）なら gap。
    fn collect_touched(&self, from: u64, to: u64) -> Result<Touched> {
        let mut touched = Touched {
            paths: BTreeSet::new(),
            gap: false,
        };
        if to <= from {
            return Ok(touched);
        }
        let root = self.op_root()?;
        let vault = root.open_vault_dir()?;
        let Ok((lines, _)) = journal::read_since(&root, &vault, from, usize::MAX) else {
            touched.gap = true;
            return Ok(touched);
        };
        let lines: Vec<JournalEntry> = lines.into_iter().take_while(|l| l.seq <= to).collect();
        if !lines
            .iter()
            .map(|line| line.seq)
            .eq(from.saturating_add(1)..=to)
        {
            touched.gap = true;
            return Ok(touched);
        }
        for line in lines {
            let paths: Vec<String> = [line.path, line.to]
                .into_iter()
                .flatten()
                .filter(|path| !path.is_empty())
                .collect();
            if paths.is_empty() {
                touched.gap = true;
                return Ok(touched);
            }
            touched.paths.extend(paths);
        }
        Ok(touched)
    }

    /// `paths` を今の状態に調べ直して、歩いた結果に当てる。調べ直せなかったパスと、時間の上限
    /// （`deadline`）に当たって調べ直さなかったパスを返す。親を先に調べ直し（BTreeSet の順）、下まで歩き直した
    /// フォルダの下のパスは飛ばす。途中のフォルダを開けなかったら、その下のパスは開かずに調べ直せないとする
    /// （どちらも、そろえた形の集まりを祖先ごとに引く）。ロックの中では、フォルダの下を歩き直さず、`marks` に
    /// 入れる（今の索引の値のまま、印を付ける）。
    #[allow(clippy::too_many_arguments)]
    fn recheck_all(
        &self,
        paths: BTreeSet<String>,
        walked: &mut Walked,
        case: &mut CaseKeys,
        known: Option<&Entries>,
        within: Within,
        marks: &mut BTreeSet<String>,
        deadline: Option<Instant>,
    ) -> Result<Rechecked> {
        let root = self.op_root()?;
        let mut chain = Chain::default();
        let mut walked_dirs: BTreeSet<String> = BTreeSet::new();
        let mut failed_dirs: BTreeSet<String> = BTreeSet::new();
        let mut result = Rechecked {
            failed: BTreeSet::new(),
            rest: BTreeSet::new(),
        };
        let mut paths = paths.into_iter();
        while let Some(path) = paths.next() {
            if deadline.is_some_and(|deadline| scan_now() >= deadline) {
                result.rest.insert(path);
                result.rest.extend(paths.by_ref());
                break;
            }
            let folded = fold(&path);
            let in_tree = |dirs: &BTreeSet<String>| {
                dirs.contains(&folded) || ancestors(&folded).any(|a| dirs.contains(a))
            };
            if in_tree(&walked_dirs) {
                continue;
            }
            if in_tree(&failed_dirs) {
                result.failed.insert(path);
                continue;
            }
            match self.recheck(&root, &mut chain, &path, walked, case, known, within, marks)? {
                Recheck::Done => {}
                Recheck::Walked(actual) => {
                    walked_dirs.insert(fold(&actual));
                }
                Recheck::Failed => {
                    result.failed.insert(path);
                }
                Recheck::ParentFailed(dir) => {
                    failed_dirs.insert(fold(&dir));
                    result.failed.insert(path);
                }
            }
        }
        Ok(result)
    }

    /// 歩いた結果のうち、`rel`（と大文字小文字だけが違うキー。その下も）を、今の状態で置き換える。根から1段ずつ、
    /// 持っている親を基準にリンクをたどらずに、試し直さずに開き（前のパスと同じ親までは `chain` を使い回す）、
    /// 開いたハンドルの最終のパスから実際の名前を取って、そのキーで入れる（実際の名前が `rel` と違えば、`rel` は
    /// 「ない」）。ない・リンクに替わっていたも、調べ直せた。
    #[allow(clippy::too_many_arguments)]
    fn recheck(
        &self,
        root: &Root,
        chain: &mut Chain,
        rel: &str,
        walked: &mut Walked,
        case: &mut CaseKeys,
        known: Option<&Entries>,
        within: Within,
        marks: &mut BTreeSet<String>,
    ) -> Result<Recheck> {
        case.remove_tree(&mut walked.entries, rel);
        let folded = fold(rel);
        let prefix = format!("{folded}/");
        walked.unknown.retain(|p| {
            let p = fold(p);
            p != folded && !p.starts_with(&prefix)
        });
        // 履歴の path は read_since で確かめてある。上限を超えるものは、歩くときと同じく対象にしない
        let Ok(rel_path) = RelPath::parse(rel) else {
            return Ok(Recheck::Done);
        };
        if !self.fits(rel) {
            return Ok(Recheck::Done);
        }
        let parts: Vec<&str> = rel_path.components().collect();
        let Some((last, parents)) = parts.split_last() else {
            return Ok(Recheck::Done);
        };
        // 前のパスと同じ親までは、開いた並びを使い回す
        let same = chain
            .names
            .iter()
            .zip(parents)
            .take_while(|(opened, part)| opened == *part)
            .count();
        chain.names.truncate(same);
        chain.dirs.truncate(same);
        for (i, part) in parents.iter().enumerate().skip(same) {
            // 試し直さない（使用中・拒否は、すぐ調べ直せないとする。ロックを長く持たない）
            match guard::step_dir_once(parent_of(root, &chain.dirs), part) {
                Ok(DirStep::Dir(pass)) => {
                    chain.names.push((*part).to_owned());
                    chain.dirs.push(pass);
                }
                // 親がない・フォルダでない・リンク：rel は（保管庫の中には）ない
                Ok(DirStep::Missing | DirStep::NotDir | DirStep::Link) => return Ok(Recheck::Done),
                Err(_) => return Ok(Recheck::ParentFailed(parts[..=i].join("/"))),
            }
        }
        let parent = parent_of(root, &chain.dirs);
        let (info, actual, linked) = match guard::child_actual(root, parent, last, &rel_path) {
            ChildActual::Found(info, actual, linked) => (info, actual, linked),
            ChildActual::Missing => return Ok(Recheck::Done),
            ChildActual::Unreadable => return Ok(Recheck::Failed),
        };
        // 実際の名前も、履歴のパスと同じ規則で確かめる（文字列の規則・同じ数の要素・上限の中）。通らなければ
        // 入れない（保管庫の外・.vault・扱えない名前を索引に入れない）
        let valid = RelPath::parse(&actual)
            .is_ok_and(|actual| actual.components().count() == parts.len())
            && self.fits(&actual);
        if !valid {
            return Ok(Recheck::Failed);
        }
        // 歩いた結果の親が、実際と別の大文字小文字になっている（歩いたあとで外のアプリが親の名前を変えた）：
        // 歩いた結果のほうが古いので、ここでは合わせない（次の走査で、親の名前の変更として比べる）
        if ancestors(&actual).any(|a| case.has_other_case(a)) {
            return Ok(Recheck::Failed);
        }
        walked
            .entries
            .insert(actual.clone(), IndexEntry::from(info));
        case.insert(&actual);
        if linked {
            walked.linked.insert(actual.clone());
        }
        if info.kind != EntryKind::Dir {
            return Ok(Recheck::Done);
        }
        if within == Within::Lock {
            // ロックの中では下を歩き直さない：今の索引の値のまま、次の走査で調べ直す印を付ける
            marks.insert(actual);
            return Ok(Recheck::Done);
        }
        let name = std::ffi::OsStr::new(last);
        let mut dir = match guard::open_child_dir(parent, name, false) {
            // 開いたフォルダが、実際の名前の場所にあることを確かめる（調べたあとで差し替えられていない）
            ChildDir::Dir(dir) => match guard::check_held_dir(root, &dir, &rel_path, At::Target) {
                Ok(opened) if opened == actual => dir,
                _ => return Ok(Recheck::Failed),
            },
            ChildDir::Other(info) => {
                walked
                    .entries
                    .insert(actual.clone(), IndexEntry::from(info));
                return Ok(Recheck::Done);
            }
            ChildDir::Missing => {
                case.remove_tree(&mut walked.entries, &actual);
                return Ok(Recheck::Done);
            }
            ChildDir::Failed(_) => return Ok(Recheck::Failed),
        };
        let Ok(children) = dir.entries("フォルダを読めない") else {
            return Ok(Recheck::Failed);
        };
        // 対象にしなかった項目の数は、歩いた回で数えた（二重に数えない）
        let counts = (walked.over_limit, walked.invalid_names);
        let node = Node::new(dir, parts.len(), None);
        self.walk_from(node, &actual, children, known, walked);
        (walked.over_limit, walked.invalid_names) = counts;
        case.add_tree(&walked.entries, &actual);
        Ok(Recheck::Walked(actual))
    }

    /// 保管庫を歩いて、今の状態を集める（繰り返しで書く）。
    ///
    /// 根を開いて確かめ（操作の根）、フォルダはハンドルで持ってそのハンドルで列挙する。下のフォルダは、持っている
    /// 親を基準に開く（リンクはたどらない）。項目の値は、まず列挙の値で索引（`known`）と比べ、違うもの・索引に
    /// ないものだけ、項目を属性だけで開いて確かめる（リンクはたどらない）。
    /// 1万ファイル（リリースビルド）で、すべてを開くと約 0.35 秒、この形で約 0.04 秒。
    fn walk(&self, known: Option<&Entries>) -> Result<Walked> {
        let (prev, parts, slot) = self.link_sweep_start();
        let mut walked = Walked {
            entries: BTreeMap::new(),
            unknown: Vec::new(),
            over_limit: 0,
            invalid_names: 0,
            linked: BTreeSet::new(),
            not_read: Vec::new(),
            sweep: Rc::new(LinkSweep { prev, parts, slot }),
        };
        let mut root = self.op_root()?.into_held();
        let children = root.entries("保管庫のフォルダを読めない")?;
        self.walk_from(Node::new(root, 0, None), "", children, known, &mut walked);
        Ok(walked)
    }

    /// 持っているフォルダ（`start`。中を列挙した `children`）から下を歩いて、`walked` に集める。
    fn walk_from(
        &self,
        start: Rc<Node>,
        start_rel: &str,
        children: Vec<Listed>,
        known: Option<&Entries>,
        walked: &mut Walked,
    ) {
        let mut stack: Vec<Pending> = Vec::new();
        self.visit_children(&start, start_rel, children, known, walked, &mut stack);
        drop(start);
        while let Some(Pending { parent, name, rel }) = stack.pop() {
            test_hook!(Step::BeforeChildOpen);
            // 試し直さない（使用中・拒否は unknown）
            let mut dir = match parent.dir.open_child_dir(&name, false) {
                ChildDir::Dir(dir) => dir,
                ChildDir::Other(info) => {
                    // リンクなどに差し替えられた：今の値にして、中へは進まない
                    walked.entries.insert(rel, IndexEntry::from(info));
                    continue;
                }
                ChildDir::Missing => {
                    // 開く前に消えた
                    walked.entries.remove(&rel);
                    continue;
                }
                ChildDir::Failed(_) => {
                    // 開けない（使用中・権限など）：中は、索引の値をそのまま使う（削除と取り違えない）
                    walked.unknown.push(rel);
                    continue;
                }
            };
            let Ok(children) = dir.entries("フォルダを読めない") else {
                walked.unknown.push(rel);
                continue;
            };
            test_hook!(Step::AfterChildList);
            let node = Node::new(dir, parent.depth + 1, Some(parent));
            test_hook!(Step::WalkHolding);
            self.visit_children(&node, &rel, children, known, walked, &mut stack);
        }
    }

    /// 持っているフォルダ（`node`）の中の項目を、索引と比べて集める。フォルダは、あとで開いて歩くように積む。
    fn visit_children(
        &self,
        node: &Rc<Node>,
        rel_dir: &str,
        children: Vec<Listed>,
        known: Option<&Entries>,
        walked: &mut Walked,
        stack: &mut Vec<Pending>,
    ) {
        for child in children {
            if child.name.eq_ignore_ascii_case(RESERVED_NAME) {
                continue;
            }
            let Some(name) = child.name.to_str().filter(|name| is_valid_component(name)) else {
                walked.invalid_names += 1;
                continue;
            };
            let rel = if rel_dir.is_empty() {
                name.to_owned()
            } else {
                format!("{rel_dir}/{name}")
            };
            if !self.fits(&rel) {
                walked.over_limit += 1;
                continue;
            }
            let listed = IndexEntry::from(child.info);
            let unchanged = known
                .and_then(|known| known.get(&rel))
                .is_some_and(|before| listed == *before);
            // ハードリンクのファイルは、外の名前から書き換えても一覧の値が古いまま（R22）。つながっていると
            // 分かったものと、回り番に当たったものは、一覧の値が索引と同じでも開いて読み直す
            let reread = child.info.kind == EntryKind::File && walked.sweep.wants(&rel);
            let entry = if unchanged && !reread {
                listed
            } else {
                // 確かめる（リンクはたどらない）。開けないとき（権限など）は、列挙の値を使う。ファイルなら、同じ
                // ハンドルでリンクの数も読む
                match node.dir.scan_child_info(&child.name) {
                    (ChildInfo::Info(info), linked) => {
                        if linked {
                            walked.linked.insert(rel.clone());
                        }
                        IndexEntry::from(info)
                    }
                    (ChildInfo::Missing, _) => continue, // 調べる前に消えた
                    (ChildInfo::Unreadable, _) => {
                        walked.not_read.push(rel.clone());
                        listed
                    }
                }
            };
            if entry.kind == EntryKind::Dir {
                stack.push(Pending {
                    parent: Rc::clone(node),
                    name: child.name,
                    rel: rel.clone(),
                });
            }
            walked.entries.insert(rel, entry);
        }
    }
}

/// 調べられなかったフォルダ・項目は、索引にある値（そのものと中）をそのまま使う。
fn keep_unknown(old: &Entries, new: &mut Entries, unknown: &[String]) {
    for path in unknown {
        let prefix = format!("{path}/");
        for (key, entry) in old.range::<str, _>((Bound::Included(path.as_str()), Bound::Unbounded))
        {
            if !key.starts_with(path.as_str()) {
                break;
            }
            if key == path || key.starts_with(&prefix) {
                new.entry(key.clone()).or_insert(*entry);
            }
        }
    }
}

/// 索引（`old`）と、歩いた結果（`new`）を比べて、履歴に書く変更を返す。
///
/// 順番：名前の変更（深い組の直前には、その先の親のフォルダの作成）→ 削除 → 作成 → 更新。削除・作成・更新の
/// パスは、名前の変更を当てたあとの名前（前から順に当てていけば、今の状態になる）。
/// 調べられなかったもの（`unknown`）の中は、名前の変更を当てたあとの索引の値を `new` に引き継ぐ
/// （読めないフォルダの祖先の名前が変わっても、中を削除と取り違えない）。`new` は、索引に書く今の状態になる。
fn diff(old: Entries, new: &mut Entries, unknown: &[String]) -> Vec<(Op, Record)> {
    diff_tracked(old, new, unknown, &mut Vec::new())
}

/// `diff` の本体。深い段で組にした名前の変更（from, to）を `deep` に足す（テストの確かめに使う）。
fn diff_tracked(
    old: Entries,
    new: &mut Entries,
    unknown: &[String],
    deep: &mut Vec<(String, String)>,
) -> Vec<(Op, Record)> {
    let mut renamer = Renamer {
        old: Index::from_entries(old),
        case: None,
        out: Vec::new(),
    };
    keep_unknown(renamer.old.entries(), new, unknown);

    // 1. 名前の変更。上の段（いちばん上の削除と作成の組）を先に試し、組がなければ深い段
    //    （いちばん上でない削除・作成も、名前が同じで値が同じもの）。合わせて MAX_RENAME_ROUNDS 回まで。どちらの
    //    段も、組を1つ決めるたびに作業用の索引へ当てる（先がふさがっていないことを、その時点の索引で確かめる）
    for _ in 0..MAX_RENAME_ROUNDS {
        let applied = match renamer.top_round(new) {
            0 => renamer.deep_round(new, deep),
            n => n,
        };
        if applied == 0 {
            break;
        }
        // 名前を変えた先の、読めないフォルダの中を引き継ぐ
        keep_unknown(renamer.old.entries(), new, unknown);
    }
    let Renamer { old, mut out, .. } = renamer;
    let new: &Entries = new;

    // 2. 削除（木のいちばん上だけ）
    for path in tops(&missing_in(old.entries(), new)) {
        let size = file_size(&old.entries()[&path]);
        out.push((
            Op::ExternalDelete,
            Record {
                path: Some(path),
                size,
                ..Record::default()
            },
        ));
    }

    // 3. 作成（種類が変わったものは、上の削除のあとに作成）と、4. 更新（ファイルとリンクの、大きさか更新時刻）
    let mut modified = Vec::new();
    for (path, entry) in new {
        let record = Record {
            path: Some(path.clone()),
            size: file_size(entry),
            ..Record::default()
        };
        match old.entries().get(path) {
            Some(before) if before.kind == entry.kind => {
                if entry.kind != EntryKind::Dir
                    && (before.size != entry.size || before.mtime_ns != entry.mtime_ns)
                {
                    modified.push((Op::ExternalModify, record));
                }
            }
            _ => out.push((Op::ExternalCreate, record)),
        }
    }
    out.extend(modified);
    out
}

/// `a` にあって、`b` にない（または種類が違う）キー。
fn missing_in(a: &Entries, b: &Entries) -> BTreeSet<String> {
    a.iter()
        .filter(|(path, entry)| b.get(*path).is_none_or(|other| other.kind != entry.kind))
        .map(|(path, _)| path.clone())
        .collect()
}

/// 集まりの中で、祖先が集まりに入っていないもの（木のいちばん上）。名前の順。
fn tops(set: &BTreeSet<String>) -> Vec<String> {
    set.iter()
        .filter(|path| !ancestors(path).any(|a| set.contains(a)))
        .cloned()
        .collect()
}

/// `a/b/c` → `a`、`a/b`。
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(i, _)| &path[..i])
}

/// 中身の1項目（相対の名前・種類・大きさ・更新時刻。フォルダの大きさと更新時刻は 0）。
type ContentItem = (String, u8, u64, i64);

/// 上の段の候補（いちばん上の作成）。値（種類・大きさ・更新時刻）ごとと、フォルダの中身ごと（同じ中身に同じ番号）に、
/// 使ったもの・組にできなかったものを除ける順序付きの集まりで持つ（同じ値の候補が多くても、候補の数の2乗に
/// ならない）。
struct TopCandidates<'a> {
    by_attrs: Candidates<'a, (u8, u64, i64)>,
    by_contents: Candidates<'a, usize>,
    content_ids: HashMap<Vec<ContentItem>, usize>,
    added_contents: HashMap<&'a str, usize>,
}

impl<'a> TopCandidates<'a> {
    fn new(new: &Entries, added: &'a [String]) -> Self {
        let mut c = Self {
            by_attrs: Candidates::new(),
            by_contents: Candidates::new(),
            content_ids: HashMap::new(),
            added_contents: HashMap::new(),
        };
        for path in added {
            let entry = &new[path];
            c.by_attrs.insert(attrs(entry), path);
            if entry.kind == EntryKind::Dir {
                let items = contents(new, path);
                if !items.is_empty() {
                    let next = c.content_ids.len();
                    let id = *c.content_ids.entry(items).or_insert(next);
                    c.by_contents.insert(id, path);
                    c.added_contents.insert(path, id);
                }
            }
        }
        c
    }

    /// `from`（値 `entry`、フォルダなら中身 `items`）の、近さ `closeness` の候補のうち、パスの順でいちばん小さいもの。
    fn pick(
        &self,
        entry: &IndexEntry,
        items: Option<&[ContentItem]>,
        from: &str,
        closeness: u8,
    ) -> Option<&'a str> {
        let by_attrs = self.by_attrs.pick(&attrs(entry), from, closeness);
        let by_contents = items
            .filter(|items| !items.is_empty())
            .and_then(|items| self.content_ids.get(items))
            .and_then(|id| self.by_contents.pick(id, from, closeness));
        by_attrs.into_iter().chain(by_contents).min()
    }

    /// 使った・組にできなかった候補を、この回のあいだ除く。
    fn remove(&mut self, new: &Entries, to: &str) {
        self.by_attrs.remove(&attrs(&new[to]), to);
        if let Some(id) = self.added_contents.get(to) {
            self.by_contents.remove(id, to);
        }
    }
}

/// 深い段の候補を分ける値：種類・大きさ・更新時刻（フォルダは更新時刻だけ）と、名前（`fold` でそろえた形）。
type DeepKey = (u8, u64, i64, String);

fn deep_key(path: &str, entry: &IndexEntry) -> DeepKey {
    let size = if entry.kind == EntryKind::Dir {
        0
    } else {
        entry.size
    };
    (
        kind_code(entry.kind),
        size,
        entry.mtime_ns,
        fold(split_name(path).1),
    )
}

/// 名前の変更の組を、作業用の索引（`old`）へ1つずつ当てながら決めるもの。
///
/// 組にしない条件（`from` → `to`）：(a) 先がふさがっている（`to` が今の `old` にある。Windows では大文字小文字
/// だけが違うキーがあり、それが `from` でない場合も）、(b) 深い段だけ：名前が同じでない、(c) 深い段だけ：`to` の
/// 祖先が `old` に別の種類である・大文字小文字だけ違う形である、(d) `from` が今の `old` にもうない（同じ回の前の組で
/// 祖先ごと動いた）、(e) 値が違う（上の段は今と同じ。深い段は種類・大きさ・更新時刻、フォルダは更新時刻）。
struct Renamer {
    old: Index,
    /// `old` のキーを大文字小文字をそろえて引く形（削除と作成の両方があるときだけ作り、組を当てるたびに直す）。
    case: Option<CaseKeys>,
    out: Vec<(Op, Record)>,
}

impl Renamer {
    fn case(&mut self) -> &mut CaseKeys {
        self.case
            .get_or_insert_with(|| CaseKeys::new(self.old.entries()))
    }

    /// (a) `to` がふさがっている。
    fn occupied(&mut self, from: &str, to: &str) -> bool {
        if self.old.entries().contains_key(to) {
            return true;
        }
        let folded = fold(to);
        self.case()
            .0
            .get(&folded)
            .is_some_and(|keys| keys.iter().any(|key| key != from))
    }

    /// (c) `to` の祖先が `old` に別の種類である、または（Windows）大文字小文字だけ違う形である。
    fn ancestors_blocked(&mut self, to: &str) -> bool {
        for ancestor in ancestors(to) {
            match self.old.entries().get(ancestor) {
                Some(entry) if entry.kind != EntryKind::Dir => return true,
                Some(_) => {}
                None => {
                    let folded = fold(ancestor);
                    if self.case().0.contains_key(&folded) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// 名前の変更を1つ当てて書く。
    fn rename(&mut self, from: &str, to: &str) {
        let size = file_size(&self.old.entries()[from]);
        if let Some(case) = self.case.as_mut() {
            let prefix = format!("{from}/");
            let moved: Vec<String> = self
                .old
                .entries()
                .range::<str, _>((Bound::Included(from), Bound::Unbounded))
                .take_while(|(key, _)| key.starts_with(from))
                .filter(|(key, _)| *key == from || key.starts_with(&prefix))
                .map(|(key, _)| key.clone())
                .collect();
            for key in moved {
                case.remove_key(&key);
                case.insert(&format!("{to}{}", &key[from.len()..]));
            }
        }
        self.old.rename_tree(from, to);
        self.out.push((
            Op::ExternalRename,
            Record {
                path: Some(from.to_owned()),
                to: Some(to.to_owned()),
                size,
                ..Record::default()
            },
        ));
    }

    /// 深い組の先の親のフォルダを、組の前に作成として書く（`new` の値で `old` に入れる）。
    fn create(&mut self, path: &str, entry: IndexEntry) {
        self.old.set(path, entry);
        if let Some(case) = self.case.as_mut() {
            case.insert(path);
        }
        self.out.push((
            Op::ExternalCreate,
            Record {
                path: Some(path.to_owned()),
                size: file_size(&entry),
                ..Record::default()
            },
        ));
    }

    /// 上の段：いちばん上の削除と作成の組（今までと同じ候補・近さ・順）。名前が同じ組（近さ 0）をすべての削除に
    /// ついて先に決め、そのあと近さ 1（同じフォルダの中）、2（ほか）。組にできない候補（(a)）は、この回のあいだ
    /// 全体から除く（次の回で引き直す）。当てた数を返す。
    fn top_round(&mut self, new: &Entries) -> usize {
        let gone = tops(&missing_in(self.old.entries(), new));
        let added = tops(&missing_in(new, self.old.entries()));
        if gone.is_empty() || added.is_empty() {
            return 0;
        }
        let mut candidates = TopCandidates::new(new, &added);
        let mut done = vec![false; gone.len()];
        let mut applied = 0;
        for closeness in 0..=2u8 {
            for (i, from) in gone.iter().enumerate() {
                if done[i] {
                    continue;
                }
                // (d) 同じ回の前の組で、祖先ごと動いた
                let Some(entry) = self.old.entries().get(from).copied() else {
                    done[i] = true;
                    continue;
                };
                let items =
                    (entry.kind == EntryKind::Dir).then(|| contents(self.old.entries(), from));
                while let Some(to) = candidates.pick(&entry, items.as_deref(), from, closeness) {
                    candidates.remove(new, to);
                    if self.occupied(from, to) {
                        continue;
                    }
                    self.rename(from, to);
                    done[i] = true;
                    applied += 1;
                    break;
                }
            }
        }
        applied
    }

    /// 深い段：いちばん上でない削除・作成も使い、名前が同じ（`fold` で比べる）で、種類・大きさ・更新時刻が同じもの
    /// （フォルダは更新時刻が同じもの）を組にする。削除は（深さ, パス）の順に見る（フォルダの組をその中のファイルの
    /// 組より先に決める）。組を決めたら、先の祖先のうち `old` にないものを上から作成として
    /// 書いてから、名前の変更を書く。当てた数を返す。
    fn deep_round(&mut self, new: &Entries, deep: &mut Vec<(String, String)>) -> usize {
        let mut gone: Vec<String> = missing_in(self.old.entries(), new).into_iter().collect();
        let added = missing_in(new, self.old.entries());
        if gone.is_empty() || added.is_empty() {
            return 0;
        }
        gone.sort_by(|a, b| {
            a.matches('/')
                .count()
                .cmp(&b.matches('/').count())
                .then_with(|| a.cmp(b))
        });
        let mut candidates: HashMap<DeepKey, BTreeSet<String>> = HashMap::new();
        for to in added {
            candidates
                .entry(deep_key(&to, &new[&to]))
                .or_default()
                .insert(to);
        }
        let mut applied = 0;
        for from in &gone {
            // (d) 同じ回の前の組で、祖先ごと動いた
            let Some(entry) = self.old.entries().get(from).copied() else {
                continue;
            };
            // (b)・(e)：名前が同じで値が同じものだけが候補
            let key = deep_key(from, &entry);
            while let Some(to) = candidates.get_mut(&key).and_then(BTreeSet::pop_first) {
                // 自分の下へは移せない・(a)・(c)。組にできない候補は、この回のあいだ除いたまま
                if to.starts_with(&format!("{from}/"))
                    || self.occupied(from, &to)
                    || self.ancestors_blocked(&to)
                {
                    continue;
                }
                let missing: Vec<String> = ancestors(&to)
                    .filter(|a| !self.old.entries().contains_key(*a))
                    .map(str::to_owned)
                    .collect();
                if missing.iter().any(|a| !new.contains_key(a)) {
                    continue; // 起きないはず（歩いた結果には、親のフォルダがある）
                }
                for ancestor in missing {
                    let made = new[&ancestor];
                    if let Some(set) = candidates.get_mut(&deep_key(&ancestor, &made)) {
                        set.remove(&ancestor);
                    }
                    self.create(&ancestor, made);
                }
                self.rename(from, &to);
                deep.push((from.clone(), to));
                applied += 1;
                break;
            }
        }
        applied
    }
}

/// 名前の変更の候補（使ったものを除ける、順序付きの集まり）。`K` は、候補を分ける値（種類・大きさ・更新時刻、
/// またはフォルダの中身の番号）。近さ（0：名前が同じ（大文字小文字を問わない）、1：同じフォルダの中、2：ほか）
/// ごとに引けるようにして、近いほうから、パスの順でいちばん小さいものを選ぶ（1つの削除につき、引くのは3回まで）。
struct Candidates<'a, K> {
    all: HashMap<K, BTreeSet<&'a str>>,
    by_name: HashMap<(K, String), BTreeSet<&'a str>>,
    by_parent: HashMap<(K, String), BTreeSet<&'a str>>,
}

/// パスを、親と名前に分ける。
fn split_name(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

impl<'a, K: Hash + Eq + Clone> Candidates<'a, K> {
    fn new() -> Self {
        Self {
            all: HashMap::new(),
            by_name: HashMap::new(),
            by_parent: HashMap::new(),
        }
    }

    fn insert(&mut self, key: K, path: &'a str) {
        let (parent, name) = split_name(path);
        self.by_name
            .entry((key.clone(), name.to_lowercase()))
            .or_default()
            .insert(path);
        self.by_parent
            .entry((key.clone(), parent.to_owned()))
            .or_default()
            .insert(path);
        self.all.entry(key).or_default().insert(path);
    }

    /// 使った候補を除く。
    fn remove(&mut self, key: &K, path: &str) {
        let (parent, name) = split_name(path);
        if let Some(set) = self.by_name.get_mut(&(key.clone(), name.to_lowercase())) {
            set.remove(path);
        }
        if let Some(set) = self.by_parent.get_mut(&(key.clone(), parent.to_owned())) {
            set.remove(path);
        }
        if let Some(set) = self.all.get_mut(key) {
            set.remove(path);
        }
    }

    /// `from` から近さ `closeness`（0：名前が同じ、1：同じフォルダの中、2：ほか）で引いた候補のうち、パスの順で
    /// いちばん小さいもの。
    fn pick(&self, key: &K, from: &str, closeness: u8) -> Option<&'a str> {
        let (parent, name) = split_name(from);
        let first = |set: Option<&BTreeSet<&'a str>>| set.and_then(|set| set.first().copied());
        match closeness {
            0 => first(self.by_name.get(&(key.clone(), name.to_lowercase()))),
            1 => first(self.by_parent.get(&(key.clone(), parent.to_owned()))),
            _ => first(self.all.get(key)),
        }
    }
}

/// 種類・大きさ・更新時刻。
fn attrs(entry: &IndexEntry) -> (u8, u64, i64) {
    (kind_code(entry.kind), entry.size, entry.mtime_ns)
}

fn kind_code(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::File => 0,
        EntryKind::Dir => 1,
        EntryKind::Link => 2,
    }
}

/// フォルダの中身（名前の順）。
fn contents(entries: &Entries, dir: &str) -> Vec<ContentItem> {
    let prefix = format!("{dir}/");
    entries
        .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
        .take_while(|(path, _)| path.starts_with(&prefix))
        .map(|(path, entry)| {
            let (size, mtime_ns) = if entry.kind == EntryKind::Dir {
                (0, 0)
            } else {
                (entry.size, entry.mtime_ns)
            };
            (
                path[prefix.len()..].to_owned(),
                kind_code(entry.kind),
                size,
                mtime_ns,
            )
        })
        .collect()
}

/// 履歴に書く大きさ（ファイルとリンクだけ。フォルダは書かない）。
fn file_size(entry: &IndexEntry) -> Option<u64> {
    (entry.kind != EntryKind::Dir).then_some(entry.size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(size: u64, mtime_ns: i64) -> IndexEntry {
        IndexEntry {
            kind: EntryKind::File,
            size,
            mtime_ns,
        }
    }

    fn dir(mtime_ns: i64) -> IndexEntry {
        IndexEntry {
            kind: EntryKind::Dir,
            size: 0,
            mtime_ns,
        }
    }

    fn entries(items: &[(&str, IndexEntry)]) -> Entries {
        items.iter().map(|(p, e)| ((*p).to_owned(), *e)).collect()
    }

    /// (op, path, to) の並び。
    fn ops(changes: &[(Op, Record)]) -> Vec<(Op, &str, Option<&str>)> {
        changes
            .iter()
            .map(|(op, r)| (*op, r.path.as_deref().unwrap(), r.to.as_deref()))
            .collect()
    }

    #[test]
    fn creates_modifies_and_deletes() {
        let old = entries(&[
            ("a.txt", file(1, 10)),
            ("d", dir(5)),
            ("d/x", file(2, 20)),
            ("gone", dir(6)),
            ("gone/y", file(3, 30)),
            ("same.txt", file(4, 40)),
        ]);
        let mut new = entries(&[
            ("a.txt", file(9, 11)),
            ("d", dir(99)), // フォルダの更新時刻だけの違いは、更新としない
            ("d/x", file(2, 20)),
            ("d/z", file(5, 50)),
            ("same.txt", file(4, 40)),
        ]);
        let changes = diff(old, &mut new, &[]);
        assert_eq!(
            ops(&changes),
            [
                (Op::ExternalDelete, "gone", None), // 中の gone/y は書かない
                (Op::ExternalCreate, "d/z", None),
                (Op::ExternalModify, "a.txt", None),
            ]
        );
        assert_eq!(changes[2].1.size, Some(9));
        assert_eq!(changes[0].1.size, None, "フォルダの大きさは書かない");
    }

    #[test]
    fn renames_of_files_and_folders() {
        let old = entries(&[
            ("a.txt", file(1, 10)),
            ("d", dir(5)),
            ("d/x", file(2, 20)),
            ("d/sub", dir(6)),
            ("d/sub/y", file(3, 30)),
        ]);
        // ファイルの名前を変え、フォルダも名前を変えた（フォルダの更新時刻は変わったが、中身は同じ）
        let mut new = entries(&[
            ("b.txt", file(1, 10)),
            ("e", dir(7)),
            ("e/x", file(2, 20)),
            ("e/sub", dir(6)),
            ("e/sub/y", file(3, 30)),
        ]);
        assert_eq!(
            ops(&diff(old, &mut new, &[])),
            [
                (Op::ExternalRename, "a.txt", Some("b.txt")),
                (Op::ExternalRename, "d", Some("e")), // 中の x・sub・y は書かない
            ]
        );
    }

    /// 名前を変えた木の中で、さらに名前を変えたものと、中の更新。
    #[test]
    fn changes_inside_a_renamed_folder() {
        let old = entries(&[
            ("d", dir(5)),
            ("d/x", file(2, 20)),
            ("d/y", file(3, 30)),
            ("d/z", file(4, 40)),
        ]);
        let mut new = entries(&[
            ("e", dir(5)),
            ("e/x2", file(2, 20)),
            ("e/y", file(8, 31)),
            ("e/w", file(6, 60)),
        ]);
        assert_eq!(
            ops(&diff(old, &mut new, &[])),
            [
                (Op::ExternalRename, "d", Some("e")),
                (Op::ExternalRename, "e/x", Some("e/x2")),
                (Op::ExternalDelete, "e/z", None),
                (Op::ExternalCreate, "e/w", None),
                (Op::ExternalModify, "e/y", None),
            ]
        );
    }

    /// 候補がいくつかあれば、名前が同じもの（大文字小文字を問わない）を選ぶ。種類が変われば削除と作成。
    #[test]
    fn rename_candidates_and_kind_changes() {
        let old = entries(&[
            ("k", file(1, 1)),
            ("p", dir(1)),
            ("p/a.txt", file(1, 10)),
            ("q", dir(1)),
        ]);
        let mut new = entries(&[
            ("k", dir(1)),
            ("p", dir(1)),
            ("p/other.txt", file(1, 10)),
            ("q", dir(1)),
            ("q/A.TXT", file(1, 10)),
        ]);
        let changes = diff(old, &mut new, &[]);
        assert_eq!(
            ops(&changes),
            [
                (Op::ExternalRename, "p/a.txt", Some("q/A.TXT")),
                (Op::ExternalDelete, "k", None),
                (Op::ExternalCreate, "k", None),
                (Op::ExternalCreate, "p/other.txt", None),
            ]
        );
    }

    /// 読めないフォルダの祖先の名前が変わっても、中は、名前を変えたあとの名前で引き継ぐ。
    #[test]
    fn unknown_folders_follow_renamed_ancestors() {
        let old = entries(&[
            ("a", dir(5)),
            ("a/locked", dir(6)),
            ("a/locked/x.txt", file(1, 10)),
            ("a/y.txt", file(2, 20)),
        ]);
        // a → b。b/locked は読めない（中が見えない）
        let mut new = entries(&[
            ("b", dir(5)),
            ("b/locked", dir(6)),
            ("b/y.txt", file(2, 20)),
        ]);
        let changes = diff(old, &mut new, &["b/locked".to_owned()]);
        assert_eq!(ops(&changes), [(Op::ExternalRename, "a", Some("b"))]);
        assert_eq!(
            new["b/locked/x.txt"],
            file(1, 10),
            "中は、名前を変えたあとの名前で残る"
        );

        // 読めないフォルダそのものの名前が変わったとき
        let old = entries(&[("locked", dir(6)), ("locked/x.txt", file(1, 10))]);
        let mut new = entries(&[("locked2", dir(6))]);
        let changes = diff(old, &mut new, &["locked2".to_owned()]);
        assert_eq!(
            ops(&changes),
            [(Op::ExternalRename, "locked", Some("locked2"))]
        );
        assert!(new.contains_key("locked2/x.txt"));
    }

    #[test]
    fn unknown_folders_keep_their_old_entries() {
        let old = entries(&[("d", dir(1)), ("d/x", file(1, 1)), ("dx", file(2, 2))]);
        let mut new = entries(&[("d", dir(2))]);
        keep_unknown(&old, &mut new, &["d".to_owned()]);
        assert_eq!(new.len(), 2);
        assert_eq!(new["d"], dir(2), "そのものは、歩いて読めた値のまま");
        assert_eq!(new["d/x"], file(1, 1));
        assert!(
            !new.contains_key("dx"),
            "名前の頭が同じだけのものは入れない"
        );
    }

    #[test]
    fn tops_are_the_highest_items() {
        let set: BTreeSet<String> = ["a", "a/b", "a/b/c", "a.txt", "b/c", "b/c/d"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(tops(&set), ["a", "a.txt", "b/c"]);
    }

    // -----------------------------------------------------------------------
    // 名前の変更の組を深い位置にも
    // -----------------------------------------------------------------------

    /// D6：先が別の種類でふさがっている（old {x ファイル, y フォルダ}、new {y ファイル（x と同じ値）}）→ 名前の
    /// 変更にしない（前から当てると、先の y がまだある）：[delete x, delete y, create y]。
    #[test]
    fn d6_a_rename_onto_a_folder_of_another_kind_is_not_made() {
        let old = entries(&[("x", file(1, 10)), ("y", dir(5))]);
        let mut new = entries(&[("y", file(1, 10))]);
        assert_eq!(
            ops(&diff(old, &mut new, &[])),
            [
                (Op::ExternalDelete, "x", None),
                (Op::ExternalDelete, "y", None),
                (Op::ExternalCreate, "y", None),
            ]
        );
    }

    /// D8：同じ回の組がぶつかる（d を新しい n の下へ移し、e/y.txt を消す。e/y.txt と d/y.txt は同じ名前・同じ値）
    /// → 前の組で d の下ごと動き（(d)）、n/d/y.txt はふさがっている（(a)）ので、e/y.txt は削除。
    #[test]
    fn d8_pairs_in_the_same_round_do_not_collide() {
        let old = entries(&[
            ("d", dir(5)),
            ("d/y.txt", file(1, 10)),
            ("e", dir(6)),
            ("e/y.txt", file(1, 10)),
        ]);
        let mut new = entries(&[
            ("e", dir(7)),
            ("n", dir(8)),
            ("n/d", dir(5)),
            ("n/d/y.txt", file(1, 10)),
        ]);
        assert_eq!(
            ops(&diff(old, &mut new, &[])),
            [
                (Op::ExternalCreate, "n", None),
                (Op::ExternalRename, "d", Some("n/d")),
                (Op::ExternalDelete, "e/y.txt", None),
            ]
        );
    }

    /// D9（Windows）：old {a.txt, c.txt}、new {C.txt}（同じ値）→ 名前が同じ組（近さ 0）を先に決める：
    /// [rename c.txt → C.txt, delete a.txt]（a.txt → C.txt は、c.txt がまだあるので前から当てられない）。
    #[cfg(windows)]
    #[test]
    fn d9_a_case_only_rename_wins_over_a_nearby_candidate() {
        let old = entries(&[("a.txt", file(1, 10)), ("c.txt", file(1, 10))]);
        let mut new = entries(&[("C.txt", file(1, 10))]);
        assert_eq!(
            ops(&diff(old, &mut new, &[])),
            [
                (Op::ExternalRename, "c.txt", Some("C.txt")),
                (Op::ExternalDelete, "a.txt", None),
            ]
        );
    }

    /// D9b（Windows）：フォルダの大文字小文字だけの名前の変更で、更新時刻は変わったが
    /// 中身が同じ（上の段の中身の組。深い段は更新時刻が同じものしか組にしないので、助けられない）。同じフォルダに
    /// 同じ中身の別のフォルダ a がある → 名前が同じ組（近さ 0）をすべての削除について先に決めるので
    /// [rename c → C, delete a]（a を先に見て C を「ふさがっている」と除くと、c も組にできない）。
    #[cfg(windows)]
    #[test]
    fn d9b_a_case_only_folder_rename_by_contents_wins() {
        let old = entries(&[
            ("a", dir(1)),
            ("a/f", file(1, 10)),
            ("c", dir(2)),
            ("c/f", file(1, 10)),
        ]);
        let mut new = entries(&[("C", dir(9)), ("C/f", file(1, 10))]);
        assert_eq!(
            ops(&diff(old, &mut new, &[])),
            [
                (Op::ExternalRename, "c", Some("C")),
                (Op::ExternalDelete, "a", None),
            ]
        );
    }

    /// 悪い形：old {a1..an, b1..bn}（同じ値）、new {B1..Bn}。上の段は名前が同じ組を
    /// 先に決めるので bK → BK、aK は削除。候補を外す数は合わせて n まで（候補の数の2乗にならない）。時間も記録する。
    #[test]
    fn many_blocked_candidates_stay_fast() {
        let n = 3000;
        let mut old = Entries::new();
        let mut new = Entries::new();
        for k in 0..n {
            old.insert(format!("a{k:04}"), file(1, 10));
            old.insert(format!("b{k:04}"), file(1, 10));
            new.insert(format!("B{k:04}"), file(1, 10));
        }
        let started = std::time::Instant::now();
        let changes = diff(old, &mut new, &[]);
        let took = started.elapsed();
        println!("悪い形（n={n}）の比べ直し：{took:?}");
        let renames = changes
            .iter()
            .filter(|(op, r)| {
                *op == Op::ExternalRename
                    && r.path.as_deref().map(|p| p.replacen('b', "B", 1)) == r.to
            })
            .count();
        assert_eq!(renames, n, "bK → BK");
        let deletes = changes
            .iter()
            .filter(|(op, _)| *op == Op::ExternalDelete)
            .count();
        assert_eq!(deletes, n, "aK は削除");
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
    }

    // RU：乱数の木と操作で、diff の結果を確かめる（I1〜I7）。外の FS に触れない。
    mod random {
        use super::*;

        /// 小さな乱数（xorshift。テストのためだけ）。
        struct Rng(u64);

        impl Rng {
            fn next(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                x
            }

            fn below(&mut self, n: usize) -> usize {
                usize::try_from(self.next() % (n as u64)).unwrap()
            }
        }

        const NAMES: [&str; 6] = ["a", "b", "A", "x.txt", "X.txt", "y.txt"];

        fn join(parent: &str, name: &str) -> String {
            if parent.is_empty() {
                name.to_owned()
            } else {
                format!("{parent}/{name}")
            }
        }

        fn parent(key: &str) -> &str {
            key.rsplit_once('/').map_or("", |(p, _)| p)
        }

        fn depth(key: &str) -> usize {
            key.matches('/').count()
        }

        /// `key` と（この OS の比べ方で）同じ名前のキーがあるか（`except` を除く）。
        fn taken(e: &Entries, key: &str, except: Option<&str>) -> bool {
            let folded = fold(key);
            e.keys()
                .any(|k| fold(k) == folded && Some(k.as_str()) != except)
        }

        fn subtree(e: &Entries, key: &str) -> Vec<String> {
            let prefix = format!("{key}/");
            e.keys()
                .filter(|k| *k == key || k.starts_with(&prefix))
                .cloned()
                .collect()
        }

        fn dirs(e: &Entries) -> Vec<String> {
            e.iter()
                .filter(|(_, v)| v.kind == EntryKind::Dir)
                .map(|(k, _)| k.clone())
                .collect()
        }

        /// 外のファイルシステムをまねた木。中身を足し引きしたフォルダの更新時刻を進める。
        struct Sim {
            e: Entries,
            clock: i64,
        }

        impl Sim {
            fn touch(&mut self, key: &str) {
                let p = parent(key);
                if !p.is_empty() {
                    self.clock += 1;
                    if let Some(d) = self.e.get_mut(p) {
                        d.mtime_ns = self.clock;
                    }
                }
            }

            fn put(&mut self, key: &str, entry: IndexEntry) {
                self.e.insert(key.to_owned(), entry);
                self.touch(key);
            }

            fn remove(&mut self, key: &str) {
                for k in subtree(&self.e, key) {
                    self.e.remove(&k);
                }
                self.touch(key);
            }

            /// 名前の変更（先の親がフォルダで、先が空いているときだけ）。値はそのまま。
            fn rename(&mut self, from: &str, to: &str) -> bool {
                let ok = self.e.contains_key(from)
                    && !to.starts_with(&format!("{from}/"))
                    && !taken(&self.e, to, Some(from))
                    && (parent(to).is_empty()
                        || self
                            .e
                            .get(parent(to))
                            .is_some_and(|d| d.kind == EntryKind::Dir));
                if !ok {
                    return false;
                }
                let moved: Vec<(String, IndexEntry)> = subtree(&self.e, from)
                    .into_iter()
                    .map(|k| {
                        let v = self.e.remove(&k).unwrap();
                        (format!("{to}{}", &k[from.len()..]), v)
                    })
                    .collect();
                self.e.extend(moved);
                self.touch(from);
                self.touch(to);
                true
            }

            fn value(&mut self, rng: &mut Rng) -> IndexEntry {
                file(rng.next() % 3, i64::try_from(rng.next() % 3).unwrap())
            }

            fn new_dir(&mut self) -> IndexEntry {
                self.clock += 1;
                dir(self.clock)
            }

            fn random_place(&self, rng: &mut Rng) -> String {
                let mut places = vec![String::new()];
                places.extend(dirs(&self.e).into_iter().filter(|d| depth(d) < 3));
                places[rng.below(places.len())].clone()
            }

            fn random_item(&self, rng: &mut Rng) -> Option<String> {
                let keys: Vec<&String> = self.e.keys().collect();
                (!keys.is_empty()).then(|| keys[rng.below(keys.len())].clone())
            }

            fn create(&mut self, rng: &mut Rng) {
                let at = join(&self.random_place(rng), NAMES[rng.below(NAMES.len())]);
                if taken(&self.e, &at, None) || depth(&at) > 3 {
                    return;
                }
                let entry = if rng.below(3) == 0 {
                    self.new_dir()
                } else {
                    self.value(rng)
                };
                self.put(&at, entry);
            }

            /// 乱数の操作を1つ。
            fn op(&mut self, rng: &mut Rng, counter: &mut u32) {
                *counter += 1;
                let Some(item) = self.random_item(rng) else {
                    self.create(rng);
                    return;
                };
                match rng.below(9) {
                    // 同じフォルダでの名前の変更（大文字小文字だけの変更も）
                    0 => {
                        let to = join(parent(&item), NAMES[rng.below(NAMES.len())]);
                        self.rename(&item, &to);
                    }
                    // すでにあるフォルダへの移動
                    1 => {
                        let name = item.rsplit('/').next().unwrap().to_owned();
                        let to = join(&self.random_place(rng), &name);
                        self.rename(&item, &to);
                    }
                    // 新しいフォルダを作って移す
                    2 => {
                        let place = self.random_place(rng);
                        let nd = join(&place, &format!("n{counter}"));
                        if place == item || place.starts_with(&format!("{item}/")) || depth(&nd) > 3
                        {
                            return;
                        }
                        let d = self.new_dir();
                        self.put(&nd, d);
                        let name = item.rsplit('/').next().unwrap().to_owned();
                        self.rename(&item, &join(&nd, &name));
                    }
                    // フォルダから出して元を消す
                    3 => {
                        let ds = dirs(&self.e);
                        if ds.is_empty() {
                            return;
                        }
                        let d = ds[rng.below(ds.len())].clone();
                        let children: Vec<String> =
                            self.e.keys().filter(|k| parent(k) == d).cloned().collect();
                        for c in children {
                            let name = c.rsplit('/').next().unwrap().to_owned();
                            self.rename(&c, &join(parent(&d), &name));
                        }
                        self.remove(&d);
                    }
                    // 木ごと消す
                    4 => self.remove(&item),
                    // 作る
                    5 | 6 => self.create(rng),
                    // 値を変える
                    7 => {
                        if self.e[&item].kind != EntryKind::Dir {
                            let v = file(3 + rng.next() % 3, 9);
                            self.e.insert(item.clone(), v);
                        }
                    }
                    // 種類を変える
                    _ => {
                        let was_dir = self.e[&item].kind == EntryKind::Dir;
                        self.remove(&item);
                        let entry = if was_dir {
                            self.value(rng)
                        } else {
                            self.new_dir()
                        };
                        self.put(&item, entry);
                    }
                }
            }
        }

        fn random_tree(rng: &mut Rng) -> Sim {
            let mut sim = Sim {
                e: Entries::new(),
                clock: 100,
            };
            let n = 5 + rng.below(36);
            for _ in 0..n {
                sim.create(rng);
            }
            sim
        }

        /// `old` に `changes` を前から当てる（Index の rename_tree を使わず、当てられることも確かめる：rename は
        /// 元があり・先がなく（この OS の比べ方で、元を除く）・先の親がフォルダで・元の下でない、create は親が
        /// フォルダで同じキーがない、delete・modify はある）。値は、create・modify は `new` から、rename は元の値。
        fn apply(
            old: &Entries,
            changes: &[(Op, Record)],
            new: &Entries,
        ) -> std::result::Result<Entries, String> {
            let mut e = old.clone();
            let parent_ok = |e: &Entries, key: &str| {
                parent(key).is_empty()
                    || e.get(parent(key)).is_some_and(|d| d.kind == EntryKind::Dir)
            };
            for (op, r) in changes {
                let path = r.path.clone().unwrap_or_default();
                match op {
                    Op::ExternalRename => {
                        let to = r.to.clone().ok_or("rename に to がない")?;
                        if !e.contains_key(&path) {
                            return Err(format!("I1：rename の元がない {path}"));
                        }
                        if taken(&e, &to, Some(path.as_str())) {
                            return Err(format!("I1：rename の先がふさがっている {path} → {to}"));
                        }
                        if !parent_ok(&e, &to) || to.starts_with(&format!("{path}/")) {
                            return Err(format!(
                                "I1：rename の先の親がフォルダでない {path} → {to}"
                            ));
                        }
                        // I3：組の値が同じ（種類はいつも。ファイルとリンクは大きさと更新時刻も）
                        let (a, b) = (e[&path], new[&to]);
                        if a.kind != b.kind
                            || (a.kind != EntryKind::Dir
                                && (a.size, a.mtime_ns) != (b.size, b.mtime_ns))
                        {
                            return Err(format!("I3：組の値が違う {path}={a:?} → {to}={b:?}"));
                        }
                        let moved: Vec<(String, IndexEntry)> = subtree(&e, &path)
                            .into_iter()
                            .map(|k| {
                                let v = e.remove(&k).unwrap();
                                (format!("{to}{}", &k[path.len()..]), v)
                            })
                            .collect();
                        e.extend(moved);
                    }
                    Op::ExternalCreate => {
                        if taken(&e, &path, None) || !parent_ok(&e, &path) {
                            return Err(format!("I1：create できない {path}"));
                        }
                        e.insert(path.clone(), new[&path]);
                    }
                    Op::ExternalDelete => {
                        if !e.contains_key(&path) {
                            return Err(format!("I1：delete するものがない {path}"));
                        }
                        for k in subtree(&e, &path) {
                            e.remove(&k);
                        }
                    }
                    Op::ExternalModify => {
                        if !e.contains_key(&path) {
                            return Err(format!("I1：modify するものがない {path}"));
                        }
                        e.insert(path.clone(), new[&path]);
                    }
                    _ => return Err(format!("思わない op {op:?}")),
                }
            }
            Ok(e)
        }

        /// I2：当てた結果が new と同じ（キーと種類。ファイルとリンクは値も。フォルダの更新時刻は比べない）。
        fn same_tree(a: &Entries, b: &Entries) -> bool {
            a.len() == b.len()
                && a.iter().zip(b).all(|((ka, va), (kb, vb))| {
                    ka == kb
                        && va.kind == vb.kind
                        && (va.kind == EntryKind::Dir
                            || (va.size, va.mtime_ns) == (vb.size, vb.mtime_ns))
                })
        }

        /// 1つの形（unknown を含む）で diff を流し、I1〜I6 を確かめる。
        fn check(
            old: &Entries,
            new: &Entries,
            unknown: &[String],
        ) -> std::result::Result<(), String> {
            let mut after = new.clone();
            let mut deep = Vec::new();
            let changes = diff_tracked(old.clone(), &mut after, unknown, &mut deep);
            let applied = apply(old, &changes, &after)?;
            if !same_tree(&applied, &after) {
                return Err(format!(
                    "I2：当てた結果が new と違う\nold {old:?}\nnew {after:?}\n当てた {applied:?}\n変更 {:?}",
                    ops(&changes)
                ));
            }
            // I4：深い組は名前が同じ
            for (from, to) in &deep {
                let name = |p: &str| fold(p.rsplit('/').next().unwrap());
                if name(from) != name(to) {
                    return Err(format!("I4：深い組の名前が違う {from} → {to}"));
                }
            }
            // I5：2回流して同じ結果
            let mut again = new.clone();
            if format!("{:?}", diff(old.clone(), &mut again, unknown)) != format!("{changes:?}")
                || again != after
            {
                return Err("I5：2回流して結果が違う".to_owned());
            }
            // I6：同じパスの create は1回まで
            let mut created = BTreeSet::new();
            for (op, r) in &changes {
                if *op == Op::ExternalCreate && !created.insert(r.path.clone()) {
                    return Err(format!("I6：同じパスを2回作った {:?}", r.path));
                }
            }
            Ok(())
        }

        fn run(seed: u64) -> std::result::Result<(), String> {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let sim = random_tree(&mut rng);
            let old = sim.e.clone();
            let mut sim = sim;
            let mut counter = 0;
            for _ in 0..5 + rng.below(11) {
                sim.op(&mut rng, &mut counter);
            }
            let new = sim.e.clone();
            check(&old, &new, &[]).map_err(|e| format!("unknown なし：{e}"))?;
            // ランダムなフォルダを unknown に（中は歩けなかった：new から中を除く）
            let ds = dirs(&new);
            if !ds.is_empty() {
                let mut partial = new.clone();
                let mut unknown = Vec::new();
                for _ in 0..1 + rng.below(2) {
                    let u = ds[rng.below(ds.len())].clone();
                    for k in subtree(&partial, &u).into_iter().filter(|k| *k != u) {
                        partial.remove(&k);
                    }
                    unknown.push(u);
                }
                check(&old, &partial, &unknown).map_err(|e| format!("unknown {unknown:?}：{e}"))?;
            }
            Ok(())
        }

        fn seeds() -> Vec<u64> {
            let var = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
            match (var("VAULT_RANDOM_SEED_FROM"), var("VAULT_RANDOM_SEED_N")) {
                (Some(from), Some(n)) => (from..from + n).collect(),
                _ => (1..=200).collect(),
            }
        }

        /// RU：既定の種 200 個（`VAULT_RANDOM_SEED_FROM`・`VAULT_RANDOM_SEED_N` で広げられる）。
        #[test]
        fn ru_random_trees_and_operations_keep_the_invariants() {
            let failures: Vec<String> = seeds()
                .into_iter()
                .filter_map(|seed| run(seed).err().map(|e| format!("種 {seed}：{e}")))
                .collect();
            assert!(
                failures.is_empty(),
                "{} 個の種で落ちた：\n{}",
                failures.len(),
                failures
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }

        /// I7（効き目の確かめ）：名前と値の組が木に1つしかないファイルを、「新しいフォルダを
        /// 作って移す」「フォルダから出して元を消す」と、名前の変更で出る。
        #[test]
        fn i7_unique_files_moved_by_folder_operations_are_renames() {
            for seed in 1..=100u64 {
                let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
                let mut sim = random_tree(&mut rng);
                // 値をすべて違うものに（名前と値の組を1つにする）
                let keys: Vec<String> = sim.e.keys().cloned().collect();
                for (i, k) in keys.iter().enumerate() {
                    if sim.e[k].kind != EntryKind::Dir {
                        sim.e.insert(k.clone(), file(1000 + i as u64, 7));
                    }
                }
                let files: Vec<String> = sim
                    .e
                    .iter()
                    .filter(|(_, v)| v.kind != EntryKind::Dir)
                    .map(|(k, _)| k.clone())
                    .collect();
                if files.is_empty() {
                    continue;
                }
                let old = sim.e.clone();
                let f = files[rng.below(files.len())].clone();
                let name = f.rsplit('/').next().unwrap().to_owned();
                let expected = if rng.below(2) == 0 || parent(&f).is_empty() {
                    // 新しいフォルダを作って移す
                    let nd = join(parent(&f), "new-folder");
                    if taken(&sim.e, &nd, None) {
                        continue;
                    }
                    let d = sim.new_dir();
                    sim.put(&nd, d);
                    let to = join(&nd, &name);
                    assert!(sim.rename(&f, &to));
                    to
                } else {
                    // フォルダから出して元を消す
                    let d = parent(&f).to_owned();
                    let to = join(parent(&d), &name);
                    if !sim.rename(&f, &to) {
                        continue;
                    }
                    sim.remove(&d);
                    to
                };
                let mut new = sim.e.clone();
                let changes = diff(old, &mut new, &[]);
                assert!(
                    changes.iter().any(|(op, r)| *op == Op::ExternalRename
                        && r.path.as_deref() == Some(f.as_str())
                        && r.to.as_deref() == Some(expected.as_str())),
                    "種 {seed}：{f} → {expected} が名前の変更で出ない：{:?}",
                    ops(&changes)
                );
            }
        }
    }
}
