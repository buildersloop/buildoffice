//! 保管庫を作る（init）・開く（open）・パスを解決する（resolve）・ロックを取る（lock）。

use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::error::{ErrorCode, ErrorDetail, Result, VaultError};
use crate::fsutil::TMP_DIR;
use crate::guard::{
    self, InitScope, ManagedFile, ManagedName, ManagedUse, ParentDir, Pass, Replaceable, Root,
};
use crate::index::{self, Index};
use crate::journal::{JOURNAL_DIR, JournalWriter, Op, Record};
use crate::relpath::{RESERVED_NAME, RelPath};
use crate::root::{RootEnv, check_input_path, check_no_ancestor_vault, check_root, is_not_found};
use crate::time;
use crate::trash::TRASH_DIR;

/// `vault.json` の形式の版。
pub const FORMAT: u32 = 1;
/// ロックを取るのを待つ時間の既定値。
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
/// ロックを試す間隔。
pub const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// 相対パスの要素の数の上限。
pub const MAX_PATH_DEPTH: usize = 64;
/// 保管庫のフォルダ（`\\?\` を除いた形）＋区切り＋相対パスの長さの上限（UTF-16 で、この数未満）。
pub const MAX_FULL_PATH_UTF16: usize = 260;

pub(crate) const LOCK_FILE: &str = "lock";
const SUB_DIRS: [&str; 3] = [TRASH_DIR, JOURNAL_DIR, TMP_DIR];
const MAX_VAULT_JSON_BYTES: u64 = 64 * 1024;

/// フォルダの中身の広がり：いちばん深いものの深さ（そのフォルダからの要素の数）と、
/// いちばん長い相対パス（そのフォルダから）の長さ（UTF-16）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Extent {
    pub depth: usize,
    pub len_utf16: usize,
}

/// `.vault/vault.json` の中身。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultMeta {
    format: u32,
    vault_id: String,
    created_at: String,
}

/// 開いた保管庫。根は開いたときに1回だけ決め（入力のパスをリンクをたどって開いた、そのハンドルの最終のパスと
/// ID）、以後はその値を使う。根のハンドルは持たない（操作のたびに開き直し、最終のパスと ID が同じことを確かめる）。
#[derive(Debug)]
pub struct Vault {
    root: PathBuf,
    /// 開いたときの根の ID（ID を受け付けないファイルシステムでは None）。
    root_id: guard::RootId,
    /// 根の長さ（`\\?\` を除いた形の UTF-16 の数）。パスの長さの判定に使う。
    root_len_utf16: usize,
    read_only: bool,
    meta: VaultMeta,
    lock_timeout: Duration,
    /// 走査の状態（このプロセスの中、この `Vault` の中だけで覚える）。
    scan_state: Mutex<ScanState>,
    /// 走査の最短の間隔（前の走査の時間に合わせて伸ばす前の値）。
    min_rescan_interval: Duration,
    /// 前の走査にかかった時間に掛ける倍数（走査の間隔を伸ばす）。
    rescan_interval_factor: u32,
}

/// 走査の状態。
#[derive(Debug, Default)]
struct ScanState {
    /// 前の走査（捨てたもの・エラーで終わったものも）が終わった時刻と、かかった時間。
    last: Option<(Instant, Duration)>,
    /// 合わせ直せずに、続けて捨てた回数（未完了で終えても 0 に戻さない）。
    discards: u32,
    /// 続けて未完了で終えた回数（走査を終えたら 0 に戻す）。
    incompletes: u32,
    /// 合わせ直せずに「次の走査で調べ直す」印を付けたパス（`""` は保管庫の全体）。
    rechecks: BTreeSet<String>,
}

/// 走査の終わり方（間隔と、続けて捨てた回数を覚えるため）。
pub(crate) enum ScanEnd {
    /// 終えた（記録した）。合わせ直せずに印を付けたパス（ないなら空）。
    Finished(BTreeSet<String>),
    /// 合わせ直せずに、その回の結果を捨てた。続けて捨てた回数（ロックの外へ戻って合わせ直した回も数える）。
    Discarded(u32),
    /// 未完了で終えた（記録せず、索引も変えなかった）。印を付けたパス。続けて捨てた回数は 0 に戻さない
    /// （.vault に書ける相手が「2回捨てて1回未完了」を繰り返して、記録を止め続けられないように）。
    Incomplete(BTreeSet<String>),
    /// エラーで終わった。
    Failed,
}

/// 走査の時計の今（テストのビルドでは、差し込み口で進められる）。
pub(crate) fn scan_now() -> Instant {
    #[cfg(feature = "test-support")]
    return crate::hooks::scan_now();
    #[cfg(not(feature = "test-support"))]
    Instant::now()
}

/// 走査の最短の間隔の既定値。
pub const MIN_RESCAN_INTERVAL: Duration = Duration::from_secs(2);
/// 走査の間隔は、最短の間隔と、前の走査にかかった時間のこの倍数の長いほう（走査に使う時間を、
/// だいたい 1 割までに抑える。アプリの自動の走査の間隔と同じ倍数）。
pub const RESCAN_INTERVAL_FACTOR: u32 = 10;
/// 前の走査の時間に合わせて伸ばすのは、この長さまで（10 分）。
pub const MAX_RESCAN_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// 合わせ直せないとき、続けて捨てるのはこの回数まで（次の回は、合わせ直せないパスに印を付けて終える。N=3）。
pub const MAX_DISCARDED_SCANS: u32 = 2;
/// ロックの中で調べ直すパスの数の上限（超えたら、ロックを外して、ロックの外で合わせ直す）。
pub const MAX_LOCKED_RECHECKS: usize = 256;
/// ロックの中で、調べ直しと比べ直しに使う時間の上限（超えたら、ロックを外して、ロックの外で合わせ直す）。
pub const MAX_LOCKED_SCAN_TIME: Duration = Duration::from_secs(1);

/// 書き込みのロック。この値を持っているあいだだけ効き、Drop で外れる。
#[must_use = "ロックは、この値を持っているあいだだけ効く"]
#[derive(Debug)]
pub struct VaultLock {
    file: File,
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        // 外せなくても、ハンドルを閉じれば OS が外す
        let _ = self.file.unlock();
    }
}

impl Vault {
    /// 保管庫を作る（アプリと `vault-mcp init` だけが使う）。
    ///
    /// 入力は `.`・`..` を含まない絶対パス。根の判定（`check_root` と入れ子の判定）を、
    /// フォルダを作るより前に通す。まだないパスなら作り、あるなら空のフォルダ（隠しファイルも数える）だけを受け付ける。
    /// 途中で失敗したら、この呼び出しで作ったもの（`.vault` とその中身、作った保管庫のフォルダ）だけを片づける。
    /// 履歴に `init` を記録し、空の索引を書く。`actor` は、アプリなら `{user, app}`、
    /// `vault-mcp init` なら `{user, cli}`。
    pub fn init(path: &Path, env: &RootEnv, actor: &Actor) -> Result<()> {
        check_input_path(path)?;
        // 入力のパスを深いほうから開き、いちばん深いある祖先を持つ（何も作らない）
        let mut scope = InitScope::begin(path)?;
        check_root(scope.target(), env)?;
        check_no_ancestor_vault(scope.target())?;
        match init_at(&mut scope, env, actor) {
            Ok(()) => Ok(()),
            // この呼び出しで作ったもの（場所と ID が記録と一致するもの）だけを片づける
            Err(e) => Err(scope.undo(e)),
        }
    }

    /// 保管庫を開く。
    ///
    /// 入力は `.`・`..` を含まない絶対パス。根の判定（`check_root` と入れ子の判定）を init と同じく通す。
    /// `.vault` とその下の `trash`・`journal`・`tmp` がリンクなら `NOT_A_VAULT`、
    /// `.vault/index.json` がふつうのファイルでない（フォルダ・リンク）なら `NOT_A_VAULT`、
    /// `.vault/vault.json` がない・読めない・知らない形式なら `NOT_A_VAULT`。
    /// 書ける状態で開いたときは、`.vault/tmp` に残った一時ファイルを、reason `orphan_tmp` でゴミ箱へ移す（消さない）。
    pub fn open(path: &Path, read_only: bool, env: &RootEnv) -> Result<Self> {
        check_input_path(path)?;
        // 入力のパスを、リンクをたどって開き、そのハンドルの最終のパスを根にし、ID を覚える
        let (root, root_id) = match guard::resolve_root(path) {
            Ok(resolved) => resolved,
            Err(e) if is_not_found(&e) => {
                return Err(not_a_vault("保管庫のフォルダが見つからない"));
            }
            Err(e) => return Err(VaultError::io("保管庫のフォルダを調べられない", &e)),
        };
        check_root(&root, env)?;
        check_no_ancestor_vault(&root)?;

        let vault_dir = root.join(RESERVED_NAME);
        check_management_dir(&vault_dir, true)?;
        for sub in SUB_DIRS {
            check_management_dir(&vault_dir.join(sub), false)?;
        }
        // 索引と vault.json は、根から開き直した .vault のハンドルを基準に、リンクをたどらずに開く
        let op_root = Root::open(&root, &root_id)?;
        let vault_handle = op_root.open_vault_dir()?;
        index::check_file(&vault_handle)?;
        let meta = read_meta(&vault_handle)?;
        drop((vault_handle, op_root));
        let root_len_utf16 = display_len_utf16(&root);
        let vault = Self {
            root,
            root_id,
            root_len_utf16,
            read_only,
            meta,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            scan_state: Mutex::new(ScanState::default()),
            min_rescan_interval: MIN_RESCAN_INTERVAL,
            rescan_interval_factor: RESCAN_INTERVAL_FACTOR,
        };
        // 書ける状態で開いたときは、.vault/tmp に残った一時ファイルをゴミ箱へ移す（消さない）
        if !read_only {
            crate::trash_ops::move_orphan_tmp(&vault);
        }
        Ok(vault)
    }

    /// 保管庫のフォルダの名前だけ（絶対パスは出さない）。
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// `vault.json` の `vault_id`。
    pub fn vault_id(&self) -> &str {
        &self.meta.vault_id
    }

    /// `vault.json` の `format`。
    pub fn format(&self) -> u32 {
        self.meta.format
    }

    /// `vault.json` の `created_at`（RFC 3339）。
    pub fn created_at(&self) -> &str {
        &self.meta.created_at
    }

    /// 読み取り専用で開いたか。
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// ロックを待つ時間を変える（既定は 10 秒）。
    pub fn set_lock_timeout(&mut self, timeout: Duration) {
        self.lock_timeout = timeout;
    }

    /// 保管庫の中の相対パスを、文字列の規則とファイルシステムの規則で確かめて、絶対パスにする。
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        self.resolve_rel(&RelPath::parse(rel)?)
    }

    /// 文字列の規則を通った相対パスを、ファイルシステムの規則で確かめて、絶対パスにする。
    ///
    /// 入口で、深さ（要素は64個まで）と長さ（保管庫のフォルダ＋区切り＋相対パスが UTF-16 で260未満）を確かめ、
    /// 超えたら `INVALID_PATH`（OS に関係なく同じ規則）。
    /// 正規化した根に要素を1つずつ push し（文字列のまま join しない）、存在する要素ごとに
    /// `symlink_metadata` を取って、リンク（Windows ではジャンクションも含む）なら `OUTSIDE_VAULT`。
    /// いちばん深い存在する祖先を canonicalize し、根の中にあって、どの要素も `.vault` でないことを確かめる。
    pub fn resolve_rel(&self, rel: &RelPath) -> Result<PathBuf> {
        self.check_depth_and_length(rel)?;
        let mut path = self.root.clone();
        let mut deepest_existing = self.root.clone();
        let mut exists = true;
        for part in rel.components() {
            path.push(part);
            if !exists {
                continue;
            }
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(VaultError::new(
                        ErrorCode::OutsideVault,
                        "リンク（シンボリックリンク・ジャンクション）はたどれない",
                    ));
                }
                Ok(_) => deepest_existing.clone_from(&path),
                Err(e) if is_not_found(&e) => exists = false,
                Err(e) => return Err(VaultError::io("パスを調べられない", &e)),
            }
        }
        let canon = fs::canonicalize(&deepest_existing)
            .map_err(|e| VaultError::io("パスを調べられない", &e))?;
        check_inside(&self.root, &canon)?;
        Ok(path)
    }

    /// 深さと長さの上限（どちらも超えたら `INVALID_PATH`）。
    fn check_depth_and_length(&self, rel: &RelPath) -> Result<()> {
        self.check_extent(rel, Extent::default(), "このパス")
    }

    /// 保管庫の中の相対パス（`/` 区切りの文字列）が、上限（深さ64・根を含めて UTF-16 で260未満）に収まるか。
    /// resolve の上限と同じ計算。上限を超えるものは、利用者に見える場所（一覧など）に出さない。
    pub(crate) fn fits(&self, rel: &str) -> bool {
        if rel.is_empty() {
            return true;
        }
        rel.split('/').count() <= MAX_PATH_DEPTH
            && self.root_len_utf16 + 1 + rel.encode_utf16().count() < MAX_FULL_PATH_UTF16
    }

    /// `rel` の下に、`below`（いちばん深い深さと、いちばん長い相対パスの長さ）の中身を置いたときに、
    /// 深さ（64個まで）と長さ（根を含めて UTF-16 で260未満）に収まるか。resolve の上限と同じ計算。
    /// 収まらなければ `INVALID_PATH`（`what` は文に入れる、何についての判定か）。
    pub(crate) fn check_extent(&self, rel: &RelPath, below: Extent, what: &str) -> Result<()> {
        let depth = rel.components().count() + below.depth;
        if depth > MAX_PATH_DEPTH {
            return Err(VaultError::new(
                ErrorCode::InvalidPath,
                format!("パスが深すぎる（要素は {MAX_PATH_DEPTH} 個まで。{what}は {depth} 個）"),
            ));
        }
        // 根 ＋（区切り＋相対パス）＋（区切り＋中のいちばん長い相対パス）
        let mut total = self.root_len_utf16;
        if !rel.is_root() {
            total += 1 + rel.to_string().encode_utf16().count();
        }
        if below.depth > 0 {
            total += 1 + below.len_utf16;
        }
        if total >= MAX_FULL_PATH_UTF16 {
            return Err(VaultError::new(
                ErrorCode::InvalidPath,
                format!(
                    "パスが長すぎる（保管庫のフォルダを含めて、UTF-16 で {} 文字まで。{what}は {} 文字多い）",
                    MAX_FULL_PATH_UTF16 - 1,
                    total + 1 - MAX_FULL_PATH_UTF16
                ),
            ));
        }
        Ok(())
    }

    /// 走査の最短の間隔を変える（既定は 2 秒）。走査の間隔は、この値と、前の走査にかかった時間の倍数
    /// （`set_rescan_interval_factor`。既定は 10。10 分まで）の長いほう。前の走査からこれより短い rescan は、
    /// 走査せずに履歴だけを返す。
    pub fn set_min_rescan_interval(&mut self, interval: Duration) {
        self.min_rescan_interval = interval;
    }

    /// 前の走査にかかった時間に掛ける倍数を変える（既定は 10。0 なら、最短の間隔だけで決める）。
    pub fn set_rescan_interval_factor(&mut self, factor: u32) {
        self.rescan_interval_factor = factor;
    }

    fn scan_state(&self) -> std::sync::MutexGuard<'_, ScanState> {
        self.scan_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 走査してよいか：前の走査（捨てたものも）が終わってから、最短の間隔と、前の走査の時間の倍数（10 分まで）の
    /// 長いほうが過ぎたか。
    pub(crate) fn rescan_due(&self) -> bool {
        let Some((ended, took)) = self.scan_state().last else {
            return true;
        };
        let grown = took
            .saturating_mul(self.rescan_interval_factor)
            .min(MAX_RESCAN_INTERVAL);
        scan_now().saturating_duration_since(ended) >= self.min_rescan_interval.max(grown)
    }

    /// 走査の始まりの時刻（走査の時計）。
    pub(crate) fn scan_started(&self) -> Instant {
        scan_now()
    }

    /// 走査が終わったことを覚える（捨てたもの・エラーで終わったものも、間隔に数える）。終えたら、続けて捨てた回数を
    /// 0 に戻し、印を替える。捨てたなら、続けて捨てた回数を覚える。
    pub(crate) fn scan_ended(&self, started: Instant, outcome: ScanEnd) {
        let ended = scan_now();
        #[cfg(feature = "test-support")]
        let took = crate::hooks::take_scan_duration()
            .unwrap_or_else(|| ended.saturating_duration_since(started));
        #[cfg(not(feature = "test-support"))]
        let took = ended.saturating_duration_since(started);
        let mut state = self.scan_state();
        state.last = Some((ended, took));
        match outcome {
            ScanEnd::Finished(rechecks) => {
                state.discards = 0;
                state.incompletes = 0;
                state.rechecks = rechecks;
            }
            ScanEnd::Incomplete(rechecks) => {
                state.incompletes = state.incompletes.saturating_add(1);
                state.rechecks = rechecks;
            }
            ScanEnd::Discarded(count) => state.discards = count,
            ScanEnd::Failed => {}
        }
    }

    /// 合わせ直せずに、続けて捨てた回数。
    pub(crate) fn scan_discards(&self) -> u32 {
        self.scan_state().discards
    }

    /// 続けて未完了で終えた回数。
    pub(crate) fn scan_incompletes(&self) -> u32 {
        self.scan_state().incompletes
    }

    /// 合わせ直せずに「次の走査で調べ直す」印を付けたパス（テストの差し込み口）。
    #[cfg(feature = "test-support")]
    pub(crate) fn scan_rechecks(&self) -> Vec<String> {
        self.scan_state().rechecks.iter().cloned().collect()
    }

    /// 書き込みのロックを取る。
    ///
    /// 呼ぶたびに `.vault/lock` を新しく開き（同じハンドルを2回ロックしない）、`try_lock` を
    /// 50ms おきに試す。待つ時間（既定 10 秒）を過ぎたら `LOCKED`。ロックは返した値の Drop で外れる。
    /// lock は、その操作の根から開き直した `.vault` のハンドルを基準に、リンクをたどらずに開く。
    pub fn lock(&self) -> Result<VaultLock> {
        acquire_lock(&self.op_root()?, self.lock_timeout)
    }

    /// 待つ時間を指定してロックを取る（open のときの片づけで、長く待たないため）。
    pub(crate) fn lock_with_timeout(&self, timeout: Duration) -> Result<VaultLock> {
        acquire_lock(&self.op_root()?, timeout)
    }

    /// その操作の根を開き直す（最終のパスと、開いたときの根の ID が同じことを確かめる。違えば NOT_A_VAULT）。
    /// 操作の根は、すべてこれで開く（`Root::open` を呼ぶのは、ここと `open` の中だけ）。
    pub(crate) fn op_root(&self) -> Result<Root> {
        Root::open(&self.root, &self.root_id)
    }

    /// 保管庫の根（canonicalize した絶対パス）。ツールの返り値とエラーの文には入れない。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// ロックのファイルを開いて、`try_lock` を 50ms おきに試す。`timeout` を過ぎたら `LOCKED`。
///
/// ロックのファイルは、根から開いた `.vault` のハンドルを基準に、リンクをたどらずに開く（ないときは
/// FILE_CREATE で作る）。リンクやフォルダなら `NOT_A_VAULT`。DELETE を共有しないので、ロックを持っているあいだ、
/// ほかのプロセスはロックのファイルの名前を変える・消すことができない。
fn acquire_lock(root: &Root, timeout: Duration) -> Result<VaultLock> {
    let vault_dir = root.open_vault_dir()?;
    let file = guard::open_lock(root, &vault_dir)?;
    drop(vault_dir);
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(VaultLock { file }),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => {
                return Err(VaultError::io("ロックを取れない", &e));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(VaultError::new(
                ErrorCode::Locked,
                "ほかの処理が書き込み中で、時間内にロックを取れなかった",
            ));
        }
        thread::sleep(LOCK_RETRY_INTERVAL.min(deadline - now));
    }
}

/// すでに保管庫のフォルダに init したときの文。
const ALREADY_A_VAULT: &str =
    "すでに保管庫になっている（.vault がある）。作り直さずに、そのまま開いて使う";

/// すでに保管庫のフォルダに init したときのエラー（コードは `InvalidRoot`。呼ぶ側は `ErrorDetail::AlreadyAVault` の印で見分ける）。
pub(crate) fn already_a_vault() -> VaultError {
    invalid_root(ALREADY_A_VAULT).with_detail(ErrorDetail::AlreadyAVault)
}

/// init の本体。作ったものは `scope` に記録し、失敗したら呼ぶ側が片づける。
fn init_at(scope: &mut InitScope, env: &RootEnv, actor: &Actor) -> Result<()> {
    // 根を用意する（ないフォルダを作る・あるなら空であることを確かめる）。作ったあとの本当の場所で、根の判定と
    // 入れ子の判定をもう一度通す。根のハンドルは InitScope が持ち、ロック・履歴・索引・vault.json は、このハンドルから
    // 開く
    scope.prepare_root()?;
    let root = scope.root()?.path().to_path_buf();
    check_root(&root, env)?;
    check_no_ancestor_vault(&root)?;
    scope.create_management()?;
    let root_handle = scope.root()?;
    let vault_handle = scope.vault()?;

    // 履歴と索引は、ロックを持って書く（vault.json がまだないので、ほかの処理は開けない）
    let lock = acquire_lock(root_handle, DEFAULT_LOCK_TIMEOUT)?;
    let mut journal = JournalWriter::open(root_handle, vault_handle, &lock)?;
    // 履歴のファイルを作ったら、1行目を書く前に片づけの対象にする（書くのに失敗しても残さない）
    if let Some(made) = journal.take_created() {
        scope.record(made);
    }
    journal.push(actor, Op::Init, Record::default())?;
    journal.commit()?;
    let written = index::save(root_handle, vault_handle, &lock, &Index::default())?;
    scope.record_written(ManagedName::IndexJson, &written);

    let now = time::now_unix_millis();
    let meta = VaultMeta {
        format: FORMAT,
        vault_id: format!("{now}-{}", std::process::id()),
        created_at: time::format_unix_millis(now)
            .ok_or_else(|| VaultError::new(ErrorCode::Internal, "今の時刻を書けない"))?,
    };
    let mut bytes = serde_json::to_vec(&meta)
        .map_err(|_| VaultError::new(ErrorCode::Internal, "vault.json を作れない"))?;
    bytes.push(b'\n');
    // vault.json を最後に書く（これがあるものだけが、保管庫として開ける。記録はしない）
    let _ = guard::atomic_write(root_handle, vault_handle, Replaceable::VaultJson, &bytes)?;
    // 開いたロックは、返る前に閉じる（履歴は commit で閉じた。失敗したときの片づけが、共有の違反にならないように）
    drop(lock);
    Ok(())
}

/// パスの長さ（`\\?\` を除いた形。`\\?\UNC\` は `\\` にする）を、UTF-16 で数える。
fn display_len_utf16(path: &Path) -> usize {
    let text = path.to_string_lossy();
    let shown = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text.into_owned()
    };
    shown.encode_utf16().count()
}

/// 管理用のフォルダ（`.vault` とその下）を確かめる。リンクやフォルダでないものは `NOT_A_VAULT`。
/// `required` でないものは、なくてもよい（使うときに作る）。
fn check_management_dir(dir: &Path, required: bool) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => Err(not_a_vault(
            "管理用のフォルダ（.vault とその下の trash・journal・tmp）がリンクになっている",
        )),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(not_a_vault(
            "管理用のフォルダ（.vault とその下の trash・journal・tmp）がフォルダでない",
        )),
        Err(e) if is_not_found(&e) => {
            if required {
                Err(not_a_vault("保管庫でない（.vault/vault.json がない）"))
            } else {
                Ok(())
            }
        }
        Err(e) => Err(VaultError::io("管理用のフォルダを調べられない", &e)),
    }
}

/// `.vault/vault.json` を読む。ない・リンク・大きすぎる・形が正しくない・知らない形式なら `NOT_A_VAULT`。
/// `.vault` のハンドルを基準に、リンクをたどらずに開き、ふつうのファイルかをハンドルで確かめる。
fn read_meta(vault_dir: &Pass) -> Result<VaultMeta> {
    test_hook!(Step::Open(Place::VaultJson));
    let file = match guard::open_managed_file(
        vault_dir.parent(),
        &ManagedName::VaultJson,
        ManagedUse::Read,
        "vault.json を開けない",
    )? {
        ManagedFile::File(file) => file,
        ManagedFile::NotFile => return Err(not_a_vault("vault.json が、ふつうのファイルでない")),
        ManagedFile::Missing => {
            return Err(not_a_vault("保管庫でない（.vault/vault.json がない）"));
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_VAULT_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| VaultError::io("vault.json を読めない", &e))?;
    if bytes.len() as u64 > MAX_VAULT_JSON_BYTES {
        return Err(not_a_vault("vault.json が大きすぎる"));
    }
    let meta: VaultMeta = serde_json::from_slice(&bytes)
        .map_err(|_| not_a_vault("vault.json を読めない（形が正しくない）"))?;
    if meta.format != FORMAT {
        return Err(not_a_vault(format!(
            "この版では開けない形式の保管庫（format {}）",
            meta.format
        )));
    }
    Ok(meta)
}

/// canonicalize したパスが、根の中にあって、どの要素も `.vault`（大文字小文字を問わない）でないこと。
/// ファイルを読まない純粋な関数。
pub(crate) fn check_inside(root: &Path, canon: &Path) -> Result<()> {
    let rel = canon
        .strip_prefix(root)
        .map_err(|_| VaultError::new(ErrorCode::OutsideVault, "保管庫の外を指している"))?;
    for c in rel.components() {
        match c {
            Component::Normal(name) if name.eq_ignore_ascii_case(RESERVED_NAME) => {
                return Err(VaultError::new(
                    ErrorCode::ReservedPath,
                    ".vault（管理用のフォルダ）を指している",
                ));
            }
            Component::Normal(_) => {}
            _ => {
                return Err(VaultError::new(
                    ErrorCode::OutsideVault,
                    "保管庫の外を指している",
                ));
            }
        }
    }
    Ok(())
}

fn invalid_root(message: &str) -> VaultError {
    VaultError::new(ErrorCode::InvalidRoot, message)
}

fn not_a_vault(message: impl Into<String>) -> VaultError {
    VaultError::new(ErrorCode::NotAVault, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"\\?\D:\vaults\v")
        } else {
            PathBuf::from("/vaults/v")
        }
    }

    #[test]
    fn check_inside_accepts_paths_inside_the_root() {
        let root = root();
        for p in [root.clone(), root.join("a"), root.join("a").join("b.md")] {
            assert!(check_inside(&root, &p).is_ok(), "{}", p.display());
        }
        // 名前が似ているだけのもの
        assert!(check_inside(&root, &root.join(".vaultx")).is_ok());
    }

    #[test]
    fn check_inside_rejects_paths_outside_the_root() {
        let root = root();
        let parent = root.parent().unwrap().to_path_buf();
        for p in [
            parent.clone(),
            parent.join("other"),
            parent.join("v2").join("a"), // 名前の頭が同じだけの隣のフォルダ
        ] {
            let err = check_inside(&root, &p).unwrap_err();
            assert_eq!(err.code(), ErrorCode::OutsideVault, "{}", p.display());
        }
    }

    #[test]
    fn check_inside_rejects_the_management_folder_in_any_case() {
        let root = root();
        for p in [
            root.join(".vault"),
            root.join(".VAULT").join("journal"),
            root.join("a").join(".Vault").join("x"),
        ] {
            let err = check_inside(&root, &p).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ReservedPath, "{}", p.display());
        }
    }

    #[test]
    fn display_length_ignores_the_verbatim_prefix() {
        assert_eq!(display_len_utf16(Path::new(r"\\?\D:\v")), 4);
        assert_eq!(display_len_utf16(Path::new(r"D:\v")), 4);
        // \\s\sh\v
        assert_eq!(display_len_utf16(Path::new(r"\\?\UNC\s\sh\v")), 8);
        assert_eq!(display_len_utf16(Path::new("/home/u/v")), 9);
        // 𝄞 は UTF-16 で2つ
        assert_eq!(display_len_utf16(Path::new("/日本/𝄞")), 6);
    }

    #[test]
    fn path_limits_match_the_decision() {
        assert_eq!(MAX_PATH_DEPTH, 64);
        assert_eq!(MAX_FULL_PATH_UTF16, 260);
    }

    #[test]
    fn lock_timing_matches_the_adr() {
        assert_eq!(DEFAULT_LOCK_TIMEOUT, Duration::from_secs(10));
        assert_eq!(LOCK_RETRY_INTERVAL, Duration::from_millis(50));
    }
}
