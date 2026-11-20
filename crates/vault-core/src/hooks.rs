//! テストだけの差し込み口（feature `test-support` のときだけ入る。release には入らない）。
//!
//! 決まった順で差し替えを起こすため：
//! - `on_next(step, hook)`：このスレッドで次にその `Step` に来たとき、1回だけ `hook` を呼ぶ。
//! - `fake_final_path(at, f)`：このスレッドで次にその種類の確かめで読む最終のパスを、`f` の結果に差し替える。
//! - `fake_free_space(v)`：このスレッドで次に問い合わせる空き容量を、偽の値か失敗にする。
//!
//! 本番のコードの呼ぶ所は `test_hook!(Step::…)`・`final_path_hook!(At, path)`・`free_space_hook!()` で書く
//! （lib.rs のマクロ。feature がなければ何も残らない）。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 差し込む所（表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// ツールの最初、その操作の根を開く前（Vault::open では、根を決めたあと）。
    BeforeRoot,
    /// 1つめの門（パスでの確かめ）のあと、利用者のパスをハンドルで開く前。
    AfterGate,
    /// 管理用の場所を開く直前。
    Open(Place),
    /// 利用者のフォルダを作る直前（create_dirs の親・vault_mkdir）。
    BeforeCreate,
    /// 一時ファイルを作って最終のパスを確かめたあと、中身を書く前。
    AfterTmpCreated,
    /// 作ったフォルダを閉じたあと、開き直す前。
    AfterCreatedClosed,
    /// rename の直前。
    BeforeRename(Rename),
    /// 「起きないはず」で元へ戻す rename の直前。
    BeforeRenameBack,
    /// 走査で、索引を読んだ直後（歩く前。ロックは持っていない）。
    AfterScanIndexLoaded,
    /// ゴミ箱の `<id>` を消す直前（持っていた `<id>` を閉じたあと、開き直す前。remove_trash_dir）。
    BeforeTrashCleanup,
    /// vault_list で、一覧するフォルダを開いて最終のパスを確かめたあと、列挙の前。
    BeforeList,
    /// 列挙のあと、下のフォルダを親を基準に開く直前（vault_list の recursive・走査・フォルダの中を歩く処理）。
    BeforeChildOpen,
    /// 走査で、下のフォルダを列挙したあと（そのフォルダを持っている）。
    AfterChildList,
    /// 走査で、下のフォルダを列挙して持ったあと、その下へ進む前。
    WalkHolding,
    /// 走査で、ロックの外で合わせたあと、書くためにロックを取る直前（ロックを持っていない）。
    BeforeScanLock,
    /// 走査で、書くためにロックを取ったあと、歩いた結果を合わせる前（ロックを持っている）。
    BeforeReconcile,
    /// 走査を終えた直後（ロックを外したあと。履歴を読んで返す前）。
    AfterScan,
    /// init が失敗して、片づけを始める直前（init の中で開いたロック・履歴のハンドルは閉じたあと）。
    BeforeInitUndo,
    /// init の片づけで、作ったフォルダを消すために開き直す直前（そのフォルダのハンドルは閉じたあと）。
    BeforeInitRemove(Removing),
    /// 既定の保管庫：場所を調べた（リンクでもファイルでもないと分かった）あと、作る・開く前（確かめのあとに場所を差し替える瞬間を作るため）。
    AfterDefaultVaultCheck,
}

/// init の片づけで消すフォルダの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removing {
    /// `.vault`。
    VaultDir,
    /// init が作った保管庫のフォルダと、その上に作ったフォルダ（深いほうから1つずつ呼ぶ）。
    CreatedDir,
}

/// 管理用の場所。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// `.vault/tmp`。
    Tmp,
    /// `.vault/journal`。
    Journal,
    /// `.vault/index.json`（読む・消す）。
    Index,
    /// `.vault/lock`。
    Lock,
    /// `.vault/vault.json`（Vault::open）。
    VaultJson,
    /// `.vault/trash`（ゴミ箱へ移す・一覧・復元）。
    Trash,
    /// `.vault/trash/<id>/entry.json`（一覧・復元で読む）。
    Entry,
}

/// rename の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rename {
    /// vault_write の最後（一時ファイルを書く先へ）。
    WriteFinal,
    /// ゴミ箱へ移したものを、もとへ戻す。
    TakeBack,
    /// 索引・vault.json を置き換える。
    ReplaceManaged,
    /// vault_move で移す。
    Move,
    /// vault_restore で、ゴミ箱の中身を戻す先へ移す。
    Restore,
    /// ゴミ箱へ移す（`trash/<id>/data` へ）。
    ToTrash,
}

/// 最終のパスを確かめる所の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalAt {
    /// 操作の根。
    Root,
    /// 読む対象・一覧するフォルダ・2つめの門の対象・すでにあるフォルダ。
    Target,
    /// 作ったもの（一時ファイル・フォルダ）。
    Created,
    /// rename したもの。
    Renamed,
    /// 作ったフォルダを閉じて開き直したもの。
    Reopened,
    /// ゴミ箱へ移したもの（`<id>/data`）。
    Trashed,
}

/// 偽の空き容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeFreeSpace {
    /// この値（バイト）を返す。
    Bytes(u64),
    /// 問い合わせが失敗する。
    Fail,
}

/// 偽のリンクの数（履歴のファイルのリンクの数を読む所）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeLinks {
    /// この数を返す。
    Count(u32),
    /// 読めない。
    Fail,
}

type Hook = Box<dyn FnOnce()>;
type PathHook = Box<dyn FnOnce(PathBuf) -> PathBuf>;

thread_local! {
    static HOOKS: RefCell<Vec<(Step, Hook)>> = const { RefCell::new(Vec::new()) };
    static FINAL_PATHS: RefCell<Vec<(FinalAt, PathHook)>> = const { RefCell::new(Vec::new()) };
    static FREE_SPACE: RefCell<Option<FakeFreeSpace>> = const { RefCell::new(None) };
    static JOURNAL_LINKS: RefCell<Vec<FakeLinks>> = const { RefCell::new(Vec::new()) };
}

/// このスレッドで次に `step` に来たとき、1回だけ `hook` を呼ぶ。複数を登録できる（登録した順に使う）。
pub fn on_next(step: Step, hook: impl FnOnce() + 'static) {
    HOOKS.with(|hooks| {
        hooks
            .try_borrow_mut()
            .expect("__vault_core_test_hook: 差し込み口の登録が入れ子になった")
            .push((step, Box::new(hook)));
    });
}

/// このスレッドで次に `at` の確かめで読む最終のパスを、`f` の結果に差し替える（1回だけ）。
pub fn fake_final_path(at: FinalAt, f: impl FnOnce(PathBuf) -> PathBuf + 'static) {
    FINAL_PATHS.with(|paths| {
        paths
            .try_borrow_mut()
            .expect("__vault_core_test_hook: 差し込み口の登録が入れ子になった")
            .push((at, Box::new(f)));
    });
}

/// このスレッドで次に問い合わせる空き容量を、偽の値か失敗にする（1回だけ）。
pub fn fake_free_space(value: FakeFreeSpace) {
    FREE_SPACE.with(|slot| *slot.borrow_mut() = Some(value));
}

/// このスレッドで次に履歴のファイルのリンクの数を読む所（開いたときと、書く直前）を、偽の値か失敗にする。
/// 複数を登録できる（読むたびに、登録した順に1つずつ使う）。
pub fn fake_journal_links(value: FakeLinks) {
    JOURNAL_LINKS.with(|slot| slot.borrow_mut().push(value));
}

thread_local! {
    static SCAN_CLOCK: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static SCAN_DURATION: Cell<Option<Duration>> = const { Cell::new(None) };
}

/// このスレッドの走査の時計（走査の間隔の判定に使う今の時刻）を、`by` だけ進める（偽の時間）。
pub fn advance_scan_clock(by: Duration) {
    SCAN_CLOCK.with(|clock| clock.set(clock.get() + by));
}

/// このスレッドの次の走査（捨てたものも）にかかった時間を、`duration` として扱う（偽の時間）。
pub fn fake_scan_duration(duration: Duration) {
    SCAN_DURATION.with(|slot| slot.set(Some(duration)));
}

/// 走査の時計の今（本当の時刻に、進めた分を足したもの）。
pub(crate) fn scan_now() -> Instant {
    Instant::now() + SCAN_CLOCK.with(Cell::get)
}

/// 偽の走査の時間があれば取り出す。
#[allow(dead_code)]
pub(crate) fn take_scan_duration() -> Option<Duration> {
    SCAN_DURATION.with(Cell::take)
}

/// 走査で、1つのフォルダを持っていた時間（record_dir_holds の測定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirHold {
    /// 根が 0、根の直下が 1。
    pub depth: usize,
    /// 開いてから閉じるまで（そのあいだ、ほかのアプリはそのフォルダの名前を変えられない）。
    pub held: Duration,
}

thread_local! {
    static HOLDS: RefCell<Option<Vec<DirHold>>> = const { RefCell::new(None) };
}

/// このスレッドの走査で、フォルダを持っていた時間を集め始める（前に集めたものは捨てる）。
pub fn record_dir_holds() {
    HOLDS.with(|holds| *holds.borrow_mut() = Some(Vec::new()));
}

/// 集めた時間を取り出し、集めるのをやめる。
pub fn take_dir_holds() -> Vec<DirHold> {
    HOLDS.with(|holds| holds.borrow_mut().take().unwrap_or_default())
}

/// 走査が持つフォルダの時計（落とすときに、集めていれば時間を残す）。
pub(crate) struct HoldTimer {
    depth: usize,
    started: Instant,
}

impl HoldTimer {
    pub(crate) fn start(depth: usize) -> Self {
        Self {
            depth,
            started: Instant::now(),
        }
    }
}

impl Drop for HoldTimer {
    fn drop(&mut self) {
        let hold = DirHold {
            depth: self.depth,
            held: self.started.elapsed(),
        };
        HOLDS.with(|holds| {
            if let Ok(mut holds) = holds.try_borrow_mut()
                && let Some(holds) = holds.as_mut()
            {
                holds.push(hold);
            }
        });
    }
}

/// 呼ぶ所：登録した hook があれば、取り出してから呼ぶ（入れ子にしない）。
pub(crate) fn fire(step: Step) {
    let hook = HOOKS.with(|hooks| {
        let mut hooks = hooks
            .try_borrow_mut()
            .expect("__vault_core_test_hook: 差し込み口の呼び出しが入れ子になった");
        hooks
            .iter()
            .position(|(s, _)| *s == step)
            .map(|i| hooks.remove(i).1)
    });
    if let Some(hook) = hook {
        hook();
    }
}

/// 確かめる所：差し替えが登録されていれば、読んだ最終のパスを差し替える。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn final_path(at: FinalAt, real: PathBuf) -> PathBuf {
    let fake = FINAL_PATHS.with(|paths| {
        let mut paths = paths
            .try_borrow_mut()
            .expect("__vault_core_test_hook: 差し込み口の呼び出しが入れ子になった");
        paths
            .iter()
            .position(|(a, _)| *a == at)
            .map(|i| paths.remove(i).1)
    });
    match fake {
        Some(f) => f(real),
        None => real,
    }
}

/// 空き容量を問い合わせる所：偽の値が登録されていれば、それを取り出す。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn take_free_space() -> Option<FakeFreeSpace> {
    FREE_SPACE.with(|slot| slot.borrow_mut().take())
}

/// 履歴のファイルのリンクの数を読む所：偽の値が登録されていれば、いちばん先に登録したものを取り出す。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn take_journal_links() -> Option<FakeLinks> {
    JOURNAL_LINKS.with(|slot| {
        let mut slot = slot.borrow_mut();
        (!slot.is_empty()).then(|| slot.remove(0))
    })
}
