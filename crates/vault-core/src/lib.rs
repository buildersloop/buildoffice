//! 保管庫のロジック。
//!
//! いま（Phase 0 の範囲で）あるもの：
//! - パスの検証（文字列の規則 `RelPath` と、ファイルシステムの規則 `Vault::resolve`）
//! - 根の判定（`check_root`・`RootEnv`・入れ子の判定・入力の要素の判定）と、`Vault::init`・`Vault::open`。
//!   置き場所と名前から保管庫のパスを作る `vault_path_in` と、置き場所がすでにあるフォルダかを確かめる `check_parent_dir`
//!   （アプリの画面から作るとき）
//! - 既定の保管庫（`open_or_create_in`・`open_or_create_in_home`。ホームの直下の決まった名前を、なければ作って開く）
//! - 書き込みのロック（`Vault::lock`）
//! - base64（RFC 4648）と RFC 3339 の時刻
//! - ツールの受付口 `api::call` と `api::tool_specs()`（10個）。10個すべてを作った
//!   （info・list・read・write・mkdir・move・delete・trash_list・restore・changes）
//! - 操作の履歴（journal）・索引（index）・ゴミ箱へ移す共通の関数
//! - 走査（外での変更の検知と、索引の作り直し）と vault_changes（`scan.rs`）
#![forbid(unsafe_code)]

/// テストだけの差し込み口を呼ぶ（`hooks.rs`。feature `test-support` がなければ何も残らない）。
macro_rules! test_hook {
    ($step:expr) => {
        #[cfg(feature = "test-support")]
        $crate::hooks::fire({
            #[allow(unused_imports)]
            use $crate::hooks::{Place, Removing, Rename, Step};
            $step
        });
    };
}

/// 最終のパスを確かめる所で、テストだけの差し替えを通す（feature がなければ、そのまま返す）。
#[allow(unused_macros)]
macro_rules! final_path_hook {
    ($at:ident, $path:expr) => {{
        #[cfg(feature = "test-support")]
        let path = $crate::hooks::final_path($crate::hooks::FinalAt::$at, $path);
        #[cfg(not(feature = "test-support"))]
        let path = $path;
        path
    }};
}

mod actor;
pub mod api;
pub mod base64;
mod error;
mod fsutil;
mod guard;
mod home_vault;
#[cfg(feature = "test-support")]
mod hooks;
mod index;
mod journal;
mod ops;
mod relpath;
mod root;
mod scan;
pub mod time;
mod trash;
mod trash_ops;
mod vault;

pub use actor::{Actor, ActorKind, MAX_ACTOR_ID_CHARS, UNKNOWN_ACTOR_ID, process_session};
pub use error::{ErrorCode, ErrorDetail, Result, VaultError};
pub use fsutil::EntryKind;
pub use guard::MIN_FREE_AFTER_WRITE;
pub use home_vault::{HomeVault, open_or_create_in, open_or_create_in_home};
pub use index::{Index, IndexEntry};
pub use journal::{JournalEntry, MAX_JOURNAL_FILE_BYTES, Op};
pub use ops::{
    DEFAULT_LIST_LIMIT, DEFAULT_READ_BYTES, Encoding, InfoResult, ListEntry, ListResult,
    MAX_LIST_LIMIT, MAX_READ_BYTES, MAX_WRITE_BASE64_CHARS, MAX_WRITE_BYTES, MkdirResult,
    MoveResult, ReadResult, WriteRequest, WriteResult,
};
pub use relpath::{MAX_COMPONENT_BYTES, MAX_PATH_BYTES, RelPath};
pub use root::{RootEnv, check_parent_dir, check_root, vault_path_in};
pub use scan::{ChangesResult, DEFAULT_CHANGES_LIMIT, MAX_CHANGES_LIMIT};
pub use trash::{MAX_ENTRY_BYTES, TrashEntry, TrashReason};
pub use trash_ops::{
    DEFAULT_TRASH_LIST_LIMIT, DeleteResult, MAX_TRASH_LIST_LIMIT, RestoreResult, TrashItem,
    TrashListResult,
};
pub use vault::{
    DEFAULT_LOCK_TIMEOUT, FORMAT, LINK_SWEEP_PARTS, LOCK_RETRY_INTERVAL, MAX_DISCARDED_SCANS,
    MAX_FULL_PATH_UTF16, MAX_LOCKED_RECHECKS, MAX_LOCKED_SCAN_TIME, MAX_PATH_DEPTH,
    MAX_RESCAN_INTERVAL, MIN_RESCAN_INTERVAL, RESCAN_INTERVAL_FACTOR, Vault, VaultLock,
};

/// テストだけが使う部品（公開の API ではない。結合テストが `CARGO_TARGET_TMPDIR` の下で使う）。
/// feature `test-support` のときだけ入る（vault-core と vault-mcp の dev-dependencies からだけ有効にする。配布のビルドには入らない）。
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    #[cfg(not(windows))]
    use std::path::PathBuf;

    /// init の片づけ（Windows 以外の `InitScope::undo` が呼ぶ guard/other.rs の `undo_items`）と同じ処理を、記録したもの（`(パス, フォルダか)` を作った順）に対して動かす。
    /// 消すのは記録したものだけで、フォルダは空のときだけ消える。
    #[cfg(not(windows))]
    pub fn undo_created(items: Vec<(PathBuf, bool)>) {
        crate::guard::undo_items(items);
    }

    /// 好きなコード・文・印のエラーを作る（呼ぶ側が、文ではなく印で見分けていることを確かめるため）。
    pub fn error(
        code: crate::ErrorCode,
        message: &str,
        detail: Option<crate::ErrorDetail>,
    ) -> crate::VaultError {
        let error = crate::VaultError::new(code, message);
        match detail {
            Some(detail) => error.with_detail(detail),
            None => error,
        }
    }

    pub use crate::hooks::{
        DirHold, FakeFreeSpace, FakeLinks, FinalAt, Place, Removing, Rename, Step,
        advance_scan_clock, fake_final_path, fake_free_space, fake_journal_links,
        fake_scan_duration, on_next, record_dir_holds, take_dir_holds,
    };

    /// 環境変数の値だけから `RootEnv` を作る（OS には問い合わせない）。偽のホームをテスト用のフォルダの下に置いて、
    /// 守る場所での振る舞いを確かめるテストに使う。
    pub fn root_env_from_vars(vars: &[(&str, &std::path::Path)]) -> crate::Result<crate::RootEnv> {
        let get = |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.as_os_str().to_owned())
        };
        crate::RootEnv::from_sources(&get, crate::root::OsPlaces::NotAsked)
    }

    /// 合わせ直せずに「次の走査で調べ直す」印を付けたパス。
    pub fn scan_rechecks(vault: &crate::Vault) -> Vec<String> {
        vault.scan_rechecks()
    }

    /// 走査で、ほかの名前とハードリンクでつながっていると分かったファイル（毎回開いて読み直す印）。
    pub fn link_marks(vault: &crate::Vault) -> Vec<String> {
        vault.link_marks()
    }

    /// 走査の回り番の次の番を決める（始めの番はプロセスごとにずれるので、テストで固定する）。
    pub fn set_sweep_slot(vault: &crate::Vault, slot: u32) {
        vault.set_sweep_slot(slot);
    }

    /// このスレッドの次の走査で、歩き終わったあと・ロックを取る前に、`hook` を1回だけ呼ぶ
    /// （走査のあいだに入ったコアの操作の扱いを、決まった順で試すため）。
    pub fn after_next_walk(hook: impl FnOnce() + 'static) {
        crate::scan::AFTER_WALK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }
}
