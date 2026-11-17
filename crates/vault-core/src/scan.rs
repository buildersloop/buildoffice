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
//! - 完全な変更の検知ではない：大きさも更新時刻も変えずに中身だけを変えたもの、ハードリンクの別の名前から
//!   書き換えたものは、見つからないことがある。
//! - 返り値とエラーの文には、根の絶対パスを入れない。

#[cfg(feature = "test-support")]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::hash::Hash;
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
        let before_seq = before.last_seq;
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
                        &mut walked,
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
                Some(self.prepare(index_now.as_ref(), walked.clone())?)
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
                        &mut walked,
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
                Some(current) => mark_unknown(&mut walked, case, current.entries(), &to_mark),
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
                let prepared = self.prepare(current.as_ref(), walked)?;
                self.write_scan(&lock, prepared)?;
                return Ok(Outcome::Done(skipped, to_mark));
            }
            let prepared = self.prepare(current.as_ref(), walked.clone())?;
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
    fn prepare(&self, old: Option<&Index>, walked: Walked) -> Result<Prepared> {
        let (records, replacement) = match old {
            None => {
                // 索引が読めない：作り直す。前の状態がわからないので、差分は出さない
                let rebuilt = Index::from_entries(walked.entries);
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
                let mut new = walked.entries;
                let records = diff(comparable, &mut new, &walked.unknown);
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
        let (info, actual) = match guard::child_actual(root, parent, last, &rel_path) {
            ChildActual::Found(info, actual) => (info, actual),
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
        let mut walked = Walked {
            entries: BTreeMap::new(),
            unknown: Vec::new(),
            over_limit: 0,
            invalid_names: 0,
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
            let entry = if unchanged {
                listed
            } else {
                // 確かめる（リンクはたどらない）。開けないとき（権限など）は、列挙の値を使う
                match node.dir.child_info(&child.name) {
                    ChildInfo::Info(info) => IndexEntry::from(info),
                    ChildInfo::Missing => continue, // 調べる前に消えた
                    ChildInfo::Unreadable => listed,
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
/// 順番：名前の変更 → 削除 → 作成 → 更新。削除・作成・更新のパスは、名前の変更を当てたあとの名前
/// （前から順に当てていけば、今の状態になる）。
/// 調べられなかったもの（`unknown`）の中は、名前の変更を当てたあとの索引の値を `new` に引き継ぐ
/// （読めないフォルダの祖先の名前が変わっても、中を削除と取り違えない）。`new` は、索引に書く今の状態になる。
fn diff(old: Entries, new: &mut Entries, unknown: &[String]) -> Vec<(Op, Record)> {
    let mut old = Index::from_entries(old);
    let mut out = Vec::new();
    keep_unknown(old.entries(), new, unknown);

    // 1. 名前の変更（いちばん上の「削除」と「作成」の組）
    for _ in 0..MAX_RENAME_ROUNDS {
        let gone = tops(&missing_in(old.entries(), new));
        let added = tops(&missing_in(new, old.entries()));
        let pairs = pair_renames(old.entries(), new, &gone, &added);
        if pairs.is_empty() {
            break;
        }
        for (from, to) in pairs {
            let size = file_size(&old.entries()[&from]);
            old.rename_tree(&from, &to);
            out.push((
                Op::ExternalRename,
                Record {
                    path: Some(from),
                    to: Some(to),
                    size,
                    ..Record::default()
                },
            ));
        }
        // 名前を変えた先の、読めないフォルダの中を引き継ぐ
        keep_unknown(old.entries(), new, unknown);
    }
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

/// 名前の変更の組を探す。いちばん上の削除 `gone` ごとに、種類・大きさ・更新時刻が一致する
/// （フォルダなら、中身が同じでもよい）いちばん上の作成 `added` を1つ選ぶ。
/// 候補がいくつかあれば、名前が同じもの（大文字小文字を問わない）→ 同じフォルダの中のもの → 名前の順。
fn pair_renames(
    old: &Entries,
    new: &Entries,
    gone: &[String],
    added: &[String],
) -> Vec<(String, String)> {
    if gone.is_empty() || added.is_empty() {
        return Vec::new();
    }
    // 候補は、値（種類・大きさ・更新時刻）ごとと、フォルダの中身ごと（同じ中身に同じ番号）に、使ったものを除ける
    // 順序付きの集まりで持つ（同じ値の候補が多くても、候補の数の2乗にならない）
    let mut by_attrs: Candidates<'_, (u8, u64, i64)> = Candidates::new();
    let mut by_contents: Candidates<'_, usize> = Candidates::new();
    let mut content_ids: HashMap<Vec<ContentItem>, usize> = HashMap::new();
    let mut added_contents: HashMap<&str, usize> = HashMap::new();
    for path in added {
        let entry = &new[path];
        by_attrs.insert(attrs(entry), path);
        if entry.kind == EntryKind::Dir {
            let items = contents(new, path);
            if !items.is_empty() {
                let next = content_ids.len();
                let id = *content_ids.entry(items).or_insert(next);
                by_contents.insert(id, path);
                added_contents.insert(path, id);
            }
        }
    }

    let mut pairs = Vec::new();
    for from in gone {
        let entry = &old[from];
        let mut best = by_attrs.best(&attrs(entry), from);
        if entry.kind == EntryKind::Dir {
            let items = contents(old, from);
            if let Some(id) = content_ids.get(&items).filter(|_| !items.is_empty()) {
                best = best.into_iter().chain(by_contents.best(id, from)).min();
            }
        }
        if let Some((_, to)) = best {
            by_attrs.remove(&attrs(&new[to]), to);
            if let Some(id) = added_contents.get(to) {
                by_contents.remove(id, to);
            }
            pairs.push((from.clone(), to.to_owned()));
        }
    }
    pairs
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

    /// `from` にいちばん近い候補（近さ, パス）。
    fn best(&self, key: &K, from: &str) -> Option<(u8, &'a str)> {
        let (parent, name) = split_name(from);
        let first = |set: Option<&BTreeSet<&'a str>>| set.and_then(|set| set.first().copied());
        if let Some(to) = first(self.by_name.get(&(key.clone(), name.to_lowercase()))) {
            return Some((0, to));
        }
        if let Some(to) = first(self.by_parent.get(&(key.clone(), parent.to_owned()))) {
            return Some((1, to));
        }
        first(self.all.get(key)).map(|to| (2, to))
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
}
