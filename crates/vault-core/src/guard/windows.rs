//! Windows の部品（os-sys の上）。
//!
//! 決まり
//! - 開き方は下の定数だけ。フォルダの開き方には DELETE の共有を入れない。DELETE の権利つきのハンドル（作った直後・
//!   2つめの門の `Target`・消す直前の `Managed`）は、この中の関数の外へ貸さない（`NewDir`・`Target` は Dir・File を
//!   貸さない。持つのは、作った・開いた呼び出しのあいだだけ）。
//! - rename の移す先の親に渡せるのは `Held` と `Pass` だけ（`RenameParent`。DELETE の権利を持たない）。
//! - 消すのは `delete` の1つの関数だけ。渡せるのは `Deletable`（作ったもの `Created<File>`・`Created<Dir>` と、
//!   決まった名前の管理用のもの `Managed`）だけで、利用者の名前から開いたハンドルは渡せない。
//!   利用者のファイル・フォルダを消すのは、今までどおりゴミ箱へ移す rename だけ。
//! - 置き換える rename（C'）で置けるのは、索引と vault.json だけ（`Replaceable`）。
//! - 使用中（32・33）と拒否（5）は、100ms おきに3回まで試す。
//! - 最終のパスは、根・読む対象・作ったもの・開き直したもの・rename したもの・2つめの門の対象で確かめる。

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::thread;

use os_sys::{
    Access, Created, Dir, FileId, KnownFolder, Links, Name, Opened, OpenedPass, PassDir, Share,
    Want,
};

use super::{ManagedName, Replaceable, TrashId};
use crate::error::{ErrorCode, Result, VaultError};
use crate::fsutil::{
    BUSY_ATTEMPTS, BUSY_INTERVAL, EntryKind, FileInfo, access_denied, io_or_disk_full,
    is_access_denied, is_busy,
};
use crate::relpath::{RESERVED_NAME, RelPath};
use crate::root::{KnownPlaces, OsPlaces, is_not_found};
use crate::trash::DATA_NAME;

// ---------------------------------------------------------------------------
// 開き方の定数（これ以外の開き方はしない）
// ---------------------------------------------------------------------------

/// 共有：READ・WRITE（フォルダ・履歴・lock。DELETE を共有しない）。
const SHARE_RW: Share = Share::READ.with(Share::WRITE);
/// 共有：READ だけ（作った一時ファイル）。
const SHARE_R: Share = Share::READ;
/// 共有：READ・WRITE・DELETE（読むだけのファイル。ほかのアプリの名前の変更・削除を邪魔しない）。
const SHARE_ALL: Share = Share::READ.with(Share::WRITE).with(Share::DELETE);

/// 持つフォルダ（根・列挙するフォルダ・.vault/tmp・.vault/journal）。データの権利を含め、DELETE を共有しない。
const HOLD_DIR: Access = Access::LIST_DIRECTORY
    .with(Access::TRAVERSE)
    .with(Access::READ_ATTRIBUTES);
/// 作るフォルダ（作った直後に最終のパスと ID を読むあいだだけ。持たない）。
const CREATE_DIR: Access = HOLD_DIR.with(Access::DELETE);
/// 読む（vault_read・索引・vault.json・履歴）。
const LEAF_READ: Access = Access::READ_DATA.with(Access::READ_ATTRIBUTES);
/// 属性だけを読む（2つめの門・書いたあとの版・索引があるか）。
const LEAF_ATTRS: Access = Access::READ_ATTRIBUTES;
/// 履歴のファイル（追記と読み）。
const JOURNAL_APPEND: Access = Access::READ_DATA
    .with(Access::APPEND_DATA)
    .with(Access::READ_ATTRIBUTES)
    .with(Access::WRITE_ATTRIBUTES);
/// .vault/lock。
const LOCK_FILE: Access = Access::READ_DATA
    .with(Access::WRITE_DATA)
    .with(Access::READ_ATTRIBUTES);
/// 一時ファイル（作るとき。中身を書き、rename で移し、だめなら作ったハンドルで消す）。
const TMP_CREATE: Access = Access::WRITE_DATA
    .with(Access::READ_ATTRIBUTES)
    .with(Access::WRITE_ATTRIBUTES)
    .with(Access::DELETE);
/// 消す管理用のファイル（書けなかった索引・戻したあとの entry.json）。open_managed の中だけ。
const MANAGED_FILE: Access = Access::DELETE.with(Access::READ_ATTRIBUTES);
/// ゴミ箱の `<id>` を消す直前だけ（remove_trash_dir の中。開いて ID を照らし、消して、すぐ閉じる）。
const MANAGED_DIR: Access = HOLD_DIR.with(Access::DELETE);
/// 移す・ゴミ箱へ送るもののうち、管理用のもの（.vault/tmp に残った一時ファイル）と、利用者のリンクそのもの。
/// 2つめの門。利用者のファイルには使わない（TARGET_FILE_W に落とさない）。
const TARGET_FILE: Access = Access::DELETE.with(Access::READ_ATTRIBUTES);
/// 利用者のファイルを上書き・移動・削除・置き換えるときの2つめの門：書き込みの権利も求め、
/// 書き込みか削除を拒否する設定（ACL）が付いたファイルを開けないようにする。共有は READ・WRITE（DELETE の共有なし）。
const TARGET_FILE_W: Access = Access::DELETE
    .with(Access::WRITE_DATA)
    .with(Access::READ_ATTRIBUTES);
/// 移す・ゴミ箱へ送るフォルダ（2つめの門。このハンドルで列挙する）。
const TARGET_DIR: Access = Access::DELETE
    .with(Access::LIST_DIRECTORY)
    .with(Access::TRAVERSE)
    .with(Access::READ_ATTRIBUTES);
/// entry.json を作る（一時ファイルと同じ）。
const ENTRY_CREATE: Access = TMP_CREATE;

// ---------------------------------------------------------------------------
// エラーの見分け
// ---------------------------------------------------------------------------

/// OBJ_DONT_REPARSE で断った（4395）か、名前を解くあいだにリパースポイントに当たって解けなかった（1921。
/// STATUS_REPARSE_POINT_NOT_RESOLVED）。どちらも、利用者のパスは OUTSIDE_VAULT、管理用は NOT_A_VAULT。
fn is_reparse(e: &io::Error) -> bool {
    os_sys::is_reparse_point_encountered(e) || e.raw_os_error() == Some(1921)
}

/// フォルダを求めたのにフォルダでなかった（267＝ERROR_DIRECTORY）。
fn is_not_a_dir(e: &io::Error) -> bool {
    e.raw_os_error() == Some(267)
}

fn is_collision(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::AlreadyExists || e.raw_os_error() == Some(183)
}

/// 使用中（32・33）と拒否（5）は、100ms おきに3回まで試す。
fn with_retry<T>(mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempt = 1;
    loop {
        match f() {
            Err(e) if (is_busy(&e) || is_access_denied(&e)) && attempt < BUSY_ATTEMPTS => {
                attempt += 1;
                thread::sleep(BUSY_INTERVAL);
            }
            other => return other,
        }
    }
}

/// 読む道：使用中（32・33）だけを、100ms おきに3回まで試す（拒否は試し直さない。今の vault_read と同じ）。
fn with_busy_retry<T>(mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempt = 1;
    loop {
        match f() {
            Err(e) if is_busy(&e) && attempt < BUSY_ATTEMPTS => {
                attempt += 1;
                thread::sleep(BUSY_INTERVAL);
            }
            other => return other,
        }
    }
}

/// 読む道で開けなかったとき：試し直したあとの使用中は BUSY、ほかは今までどおり IO_ERROR。
fn read_failed(what: &str, e: &io::Error) -> VaultError {
    if is_busy(e) {
        VaultError::new(
            ErrorCode::Busy,
            format!("{what}（使用中。ほかのアプリが開いている など）"),
        )
    } else {
        VaultError::io(what, e)
    }
}

/// 試し直したあとの使用中・拒否を、BUSY・ACCESS_DENIED にする。ほかは IO_ERROR（ディスクがいっぱいなら DISK_FULL）。
fn busy_or_io(what: &str, e: &io::Error) -> VaultError {
    if is_busy(e) {
        VaultError::new(
            ErrorCode::Busy,
            "ファイルが使用中で、置き換えられない（ほかのアプリが開いている など）",
        )
    } else if is_access_denied(e) {
        access_denied(
            "アクセスが拒否された（読み取り専用の印・権限・ほかのアプリが開いている など）",
        )
    } else {
        io_or_disk_full(what, e)
    }
}

/// 開く・作るときの、試し直したあとの使用中・拒否（使用中は「{what}（使用中。…）」）。ほかは IO_ERROR か DISK_FULL。
fn open_failed(what: &str, e: &io::Error) -> VaultError {
    if is_busy(e) {
        VaultError::new(
            ErrorCode::Busy,
            format!("{what}（使用中。ほかのアプリが開いている など）"),
        )
    } else if is_access_denied(e) {
        access_denied(
            "アクセスが拒否された（読み取り専用の印・権限・ほかのアプリが開いている など）",
        )
    } else {
        io_or_disk_full(what, e)
    }
}

fn not_a_vault(message: &str) -> VaultError {
    VaultError::new(ErrorCode::NotAVault, message)
}

/// 管理用のフォルダ（tmp・journal）がリンクかフォルダでないときの文（今までの ensure_management_dir と同じ）。
pub(crate) const MANAGEMENT_DIR_NOT_DIR: &str =
    "管理用のフォルダ（.vault の下）が、リンクかフォルダでないものになっている";

fn managed_not_regular() -> VaultError {
    not_a_vault("管理用のファイル・フォルダ（.vault の下）が、リンクかふつうでないものになっている")
}

fn root_changed() -> VaultError {
    not_a_vault(
        "保管庫のフォルダが、開いたときと違う（名前が変わった・移された・差し替えられた）。保管庫を開き直す",
    )
}

/// 作ったフォルダが、閉じてから開き直すまでに変えられた。
fn created_changed(rel: &str) -> VaultError {
    VaultError::new(
        ErrorCode::Conflict,
        format!("作ったフォルダが、確かめたあとで、ほかのアプリに変えられた：{rel}。もう一度試す"),
    )
}

fn single_name(name: &str) -> Result<Name> {
    Name::new(OsStr::new(name))
        .map_err(|_| VaultError::new(ErrorCode::Internal, "名前を、OS に渡す形にできない"))
}

// ---------------------------------------------------------------------------
// 親（ハンドル）
// ---------------------------------------------------------------------------

/// 親として渡すハンドル（`Held`・`Pass`、または歩き始めのフォルダ `Target`。この中でだけ作る）。
#[derive(Clone, Copy)]
pub(crate) struct ParentRef<'a>(ParentKind<'a>);

#[derive(Clone, Copy)]
enum ParentKind<'a> {
    Held(&'a Dir),
    Pass(&'a PassDir),
    /// 移す・消すフォルダ（TARGET_DIR。DELETE の権利を持つ）を、その下を歩くために親として使うもの
    /// （`walk_from` の始まりのフォルダだけ）。できるのは、項目を開くこと（`open_child`）
    /// だけ。通り抜けて開く・作る・rename の移す先にはできない。
    Target(&'a Dir),
    /// init の「いちばん深いある祖先」（INIT_BASE で開いたもの。InitScope の中だけで作る）。保管庫のフォルダの
    /// 最初の段を作り、開き直し、片づけで開く親としてだけ使う（ファイルは作れない。rename の移す先にはできない）。
    InitBase(&'a Dir),
}

/// Target の種類の親でできない操作（通り抜けて開く・作る）と、init の祖先の親でできない操作（ファイルを作る）。
fn not_for_target() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "この親を基準に、通り抜けて開く・作ることはできない",
    )
}

impl ParentRef<'_> {
    fn open_child(
        self,
        name: &Name,
        want: Want,
        access: Access,
        share: Share,
    ) -> io::Result<Opened> {
        match self.0 {
            ParentKind::Held(d) | ParentKind::Target(d) | ParentKind::InitBase(d) => {
                os_sys::open_child(d, name, want, access, share)
            }
            ParentKind::Pass(p) => os_sys::open_child(p, name, want, access, share),
        }
    }

    fn open_pass(self, name: &Name) -> io::Result<OpenedPass> {
        match self.0 {
            ParentKind::Held(d) | ParentKind::InitBase(d) => os_sys::open_pass_dir(d, name),
            ParentKind::Pass(p) => os_sys::open_pass_dir(p, name),
            ParentKind::Target(_) => Err(not_for_target()),
        }
    }

    fn create_dir(self, name: &Name) -> io::Result<Created<Dir>> {
        match self.0 {
            ParentKind::Held(d) | ParentKind::InitBase(d) => {
                os_sys::create_dir_child(d, name, CREATE_DIR, SHARE_RW)
            }
            ParentKind::Pass(p) => os_sys::create_dir_child(p, name, CREATE_DIR, SHARE_RW),
            ParentKind::Target(_) => Err(not_for_target()),
        }
    }

    fn create_file(self, name: &Name, access: Access, share: Share) -> io::Result<Created<File>> {
        match self.0 {
            ParentKind::Held(d) => os_sys::create_file_child(d, name, access, share),
            ParentKind::Pass(p) => os_sys::create_file_child(p, name, access, share),
            ParentKind::Target(_) | ParentKind::InitBase(_) => Err(not_for_target()),
        }
    }

    /// 親のハンドルの最終のパス（init の片づけで、作ったフォルダの場所を確かめる）。
    fn final_path(self) -> io::Result<PathBuf> {
        match self.0 {
            ParentKind::Held(d) | ParentKind::Target(d) | ParentKind::InitBase(d) => {
                os_sys::final_path(d.as_file())
            }
            ParentKind::Pass(p) => os_sys::final_path(p.as_file()),
        }
    }
}

/// rename の移す先の親（`Held` の Dir か `Pass` の PassDir だけ。`RenameParent::rename_to` からしか取れない。
/// DELETE の権利を持つフォルダ（Target）は入らない）。
#[derive(Clone, Copy)]
pub(crate) struct RenameTo<'a>(RenameKind<'a>);

#[derive(Clone, Copy)]
enum RenameKind<'a> {
    Held(&'a Dir),
    Pass(&'a PassDir),
}

impl RenameTo<'_> {
    fn rename_no_replace(self, file: &File, name: &Name) -> io::Result<()> {
        match self.0 {
            RenameKind::Held(d) => os_sys::rename_no_replace(file, d, name),
            RenameKind::Pass(p) => os_sys::rename_no_replace(file, p, name),
        }
    }

    fn rename_replace(self, file: &File, name: &Name) -> io::Result<()> {
        match self.0 {
            RenameKind::Held(d) => os_sys::rename_replace(file, d, name),
            RenameKind::Pass(p) => os_sys::rename_replace(file, p, name),
        }
    }
}

/// 親として使えるもの（`Held`・`Pass`）。
pub(crate) trait ParentDir {
    fn parent(&self) -> ParentRef<'_>;
}

/// rename の移す先の親に渡せるもの。DELETE の権利を持たない `Held` と `Pass` だけが実装する。
pub(crate) trait RenameParent: ParentDir {
    /// rename の移す先として渡すもの。
    fn rename_to(&self) -> RenameTo<'_>;
}

/// 持つフォルダ（HOLD_DIR。根・.vault/tmp・.vault/journal）。
#[derive(Debug)]
pub(crate) struct Held(Dir);

/// 通り抜けるだけのフォルダ（PassDir。途中のフォルダ・書く先の親・作って開き直したフォルダ・.vault）。
#[derive(Debug)]
pub(crate) struct Pass(PassDir);

impl ParentDir for Held {
    fn parent(&self) -> ParentRef<'_> {
        ParentRef(ParentKind::Held(&self.0))
    }
}

impl ParentDir for Pass {
    fn parent(&self) -> ParentRef<'_> {
        ParentRef(ParentKind::Pass(&self.0))
    }
}

impl RenameParent for Held {
    fn rename_to(&self) -> RenameTo<'_> {
        RenameTo(RenameKind::Held(&self.0))
    }
}

impl RenameParent for Pass {
    fn rename_to(&self) -> RenameTo<'_> {
        RenameTo(RenameKind::Pass(&self.0))
    }
}

/// 親の並び（根からの PassDir の並び）の、いちばん下の親。並びが空なら根。
pub(crate) fn parent_of<'a>(root: &'a Root, chain: &'a [Pass]) -> ParentRef<'a> {
    match chain.last() {
        Some(p) => p.parent(),
        None => root.held.parent(),
    }
}

// ---------------------------------------------------------------------------
// 根
// ---------------------------------------------------------------------------

/// その操作の根（HOLD_DIR で開き、最終のパスを確かめたもの。操作のあいだだけ持つ）。
#[derive(Debug)]
pub(crate) struct Root {
    held: Held,
    path: PathBuf,
}

/// 根の ID（保管庫を開いたときに読んだ、根のフォルダのファイルの ID）。
/// ID を受け付けないファイルシステム（FAT 系の見込み）では None で、そのときは最終のパスだけで確かめる。
pub(crate) type RootId = Option<FileId>;

/// 保管庫を開くとき：入力のパスを、リンクをたどって開き、そのハンドルの最終のパス（根にする。canonicalize と同じ値）
/// と、根の ID を読む。ID が None になるのは、受け付けないことを示すエラー（87・50・1）のときだけで、ほかの
/// エラーでは Err（開くのをやめる）。
pub(crate) fn resolve_root(path: &Path) -> io::Result<(PathBuf, RootId)> {
    let opened = os_sys::open_path(path, Links::Follow, Access::READ_ATTRIBUTES, SHARE_ALL)?;
    let file = match &opened {
        Opened::Dir(d) => d.as_file(),
        Opened::File(f) | Opened::Link(f) => f,
    };
    let final_path = os_sys::final_path(file)?;
    let id = match os_sys::file_id(file) {
        Ok(id) => Some(id),
        Err(e) if is_id_unsupported(&e) => None,
        Err(e) => return Err(e),
    };
    Ok((final_path, id))
}

/// ファイルの ID を受け付けないことを示すエラーか（ERROR_INVALID_PARAMETER 87・ERROR_NOT_SUPPORTED 50・
/// ERROR_INVALID_FUNCTION 1。os-sys が古い OS・ファイルシステムへの切り替えに使うものと同じ）。
fn is_id_unsupported(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(87 | 50 | 1))
}

/// 根の判定の場所を整えるとき：入力のパスを、リンクをたどって開き、そのハンドルの最終のパスを返す。
pub(crate) fn resolve_root_path(path: &Path) -> io::Result<PathBuf> {
    let opened = os_sys::open_path(path, Links::Follow, Access::READ_ATTRIBUTES, SHARE_ALL)?;
    let file = match &opened {
        Opened::Dir(d) => d.as_file(),
        Opened::File(f) | Opened::Link(f) => f,
    };
    os_sys::final_path(file)
}

/// 既定の保管庫の置き場所にするホーム：OS の既知のフォルダ Profile（自分のプロセスのトークンで問い合わせ、
/// KF_FLAG_DONT_VERIFY。環境変数は使わない）。問い合わせられなければ `INVALID_ROOT`。文にパスを入れない。
/// ホームのパスは vault-core の外へ出さない（呼ぶのは home_vault.rs だけ）。
pub(crate) fn os_home() -> Result<PathBuf> {
    os_sys::known_folder(KnownFolder::Profile).map_err(|_| {
        VaultError::new(
            ErrorCode::InvalidRoot,
            "OS にホームのフォルダを問い合わせられないので、既定の保管庫を開けない",
        )
    })
}

/// `path` を、最後の要素のリンクをたどらずに開き、フォルダで、その ID が `id` と同じか（既定の保管庫で、場所が
/// リンクに差し替えられていないことを確かめる）。途中の要素のリンクは OS がたどる。ない・リンク・
/// フォルダでない・ID が違うなら false。`id` が None（ID を受け付けないファイルシステム）なら、フォルダでリンクで
/// ないことだけを見る。調べられないときは Err。
pub(crate) fn is_same_dir_without_links(path: &Path, id: &RootId) -> io::Result<bool> {
    let opened = match os_sys::open_path(path, Links::DontFollow, LEAF_ATTRS, SHARE_ALL) {
        Ok(opened) => opened,
        Err(e) if is_not_found(&e) => return Ok(false),
        Err(e) => return Err(e),
    };
    let Opened::Dir(dir) = opened else {
        return Ok(false);
    };
    match id {
        Some(id) => Ok(os_sys::file_id(dir.as_file())? == *id),
        None => Ok(true),
    }
}

/// 既知のフォルダの場所を OS に問い合わせる（根の判定の RootEnv に足す）。取れなかった
/// ものは None（ホームが取れないときに断るのは RootEnv）。
pub(crate) fn os_places() -> OsPlaces {
    let get = |folder| os_sys::known_folder(folder).ok();
    OsPlaces::Asked(Box::new(KnownPlaces {
        profile: get(KnownFolder::Profile),
        roaming_app_data: get(KnownFolder::RoamingAppData),
        local_app_data: get(KnownFolder::LocalAppData),
        documents: get(KnownFolder::Documents),
        windows: get(KnownFolder::Windows),
        program_files: get(KnownFolder::ProgramFiles),
        program_files_x86: get(KnownFolder::ProgramFilesX86),
        program_data: get(KnownFolder::ProgramData),
    }))
}

impl Root {
    /// 操作のたびに、根を開き直す。`expected`（保管庫を開いたときの根）を、リンクを
    /// たどらずに開き、フォルダでリンクでないこと、最終のパスが完全に一致することを確かめる。`id`（開いたときの
    /// 根の ID）があれば、開いたハンドルの ID も同じことを確かめる（開いたまま、同じ名前の別のフォルダに
    /// 差し替えられたら気づく）。違う・ID を読めないなら NOT_A_VAULT。
    pub(crate) fn open(expected: &Path, id: &RootId) -> Result<Self> {
        test_hook!(Step::BeforeRoot);
        let opened = match os_sys::open_path(expected, Links::DontFollow, HOLD_DIR, SHARE_RW) {
            Ok(opened) => opened,
            Err(e) if is_not_found(&e) || is_reparse(&e) => return Err(root_changed()),
            Err(e) => return Err(open_failed("保管庫のフォルダを開けない", &e)),
        };
        let Opened::Dir(dir) = opened else {
            return Err(root_changed());
        };
        let path = os_sys::final_path(dir.as_file())
            .map_err(|e| VaultError::io("保管庫のフォルダの場所を読めない", &e))?;
        let path = final_path_hook!(Root, path);
        if path.as_os_str() != expected.as_os_str() {
            return Err(root_changed());
        }
        if let Some(id) = id {
            match os_sys::file_id(dir.as_file()) {
                Ok(now) if now == *id => {}
                _ => return Err(root_changed()),
            }
        }
        Ok(Self {
            held: Held(dir),
            path,
        })
    }

    /// 根の最終のパス（`\\?\` の付いた形）。エラーの文には入れない。
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// `.vault` を開く（通り抜けるだけ）。ない・リンク・フォルダでないなら NOT_A_VAULT。
    pub(crate) fn open_vault_dir(&self) -> Result<Pass> {
        let name = single_name(RESERVED_NAME)?;
        match with_retry(|| self.held.parent().open_pass(&name)) {
            Ok(OpenedPass::Dir(p)) => Ok(Pass(p)),
            Ok(OpenedPass::Link(_)) => Err(managed_not_regular()),
            Err(e) if is_not_found(&e) => Err(not_a_vault("保管庫でない（.vault がない）")),
            Err(e) if is_not_a_dir(&e) || is_reparse(&e) => Err(managed_not_regular()),
            Err(e) => Err(open_failed("管理用のフォルダを開けない", &e)),
        }
    }

    /// 呼ぶユーザーが使える空き容量（根のハンドルの最終のパスで問い合わせる）。
    pub(crate) fn free_space(&self) -> Result<u64> {
        #[cfg(feature = "test-support")]
        if let Some(fake) = crate::hooks::take_free_space() {
            return match fake {
                crate::hooks::FakeFreeSpace::Bytes(n) => Ok(n),
                crate::hooks::FakeFreeSpace::Fail => Err(VaultError::new(
                    ErrorCode::IoError,
                    "空き容量を調べられない（テストの差し込み）",
                )),
            };
        }
        os_sys::free_space(&self.held.0).map_err(|e| VaultError::io("空き容量を調べられない", &e))
    }
}

// ---------------------------------------------------------------------------
// 最終のパスの比べ
// ---------------------------------------------------------------------------

/// どの確かめか（テストの差し替えの種類に合わせる）。
#[derive(Clone, Copy)]
pub(crate) enum At {
    Target,
    Renamed,
    Reopened,
    Trashed,
}

fn read_final(handle: &File, at: At) -> Result<PathBuf> {
    let path = os_sys::final_path(handle).map_err(|e| VaultError::io("場所を読めない", &e))?;
    Ok(match at {
        At::Target => final_path_hook!(Target, path),
        At::Renamed => final_path_hook!(Renamed, path),
        At::Reopened => final_path_hook!(Reopened, path),
        At::Trashed => final_path_hook!(Trashed, path),
    })
}

/// 根の下の残りの要素（`\` で分けたもの）。先頭が根と完全に一致し、続く1文字が `\` でなければ None。
fn below_root(root: &Path, path: &Path) -> Option<Vec<String>> {
    let root_w: Vec<u16> = root.as_os_str().encode_wide().collect();
    let path_w: Vec<u16> = path.as_os_str().encode_wide().collect();
    if path_w.len() <= root_w.len() + 1
        || path_w[..root_w.len()] != root_w[..]
        || path_w[root_w.len()] != u16::from(b'\\')
    {
        return None;
    }
    String::from_utf16(&path_w[root_w.len() + 1..])
        .ok()
        .map(|rest| rest.split('\\').map(str::to_owned).collect())
}

/// 利用者の対象：根の下で、要素の数が `rel` と同じで、どれも `.vault` でない（ASCII の大文字小文字を問わない）。
/// 実際の名前（`/` でつないだもの）を返す。要素の名前どうしは比べない。
fn user_rel(root: &Path, path: &Path, rel: &RelPath) -> Result<String> {
    let outside = || VaultError::new(ErrorCode::OutsideVault, "保管庫の外を指している");
    let parts = below_root(root, path).ok_or_else(outside)?;
    if parts.len() != rel.components().count() || parts.iter().any(String::is_empty) {
        return Err(outside());
    }
    if parts.iter().any(|p| p.eq_ignore_ascii_case(RESERVED_NAME)) {
        return Err(VaultError::new(
            ErrorCode::ReservedPath,
            ".vault（管理用のフォルダ）を指している",
        ));
    }
    Ok(parts.join("/"))
}

/// 管理用の対象：根の下の並びが `.vault` と `below` に、ASCII の大文字小文字を問わずに一致する。
fn check_managed(root: &Path, path: &Path, below: &[&str]) -> Result<()> {
    let parts = below_root(root, path).ok_or_else(managed_not_regular)?;
    let expected = std::iter::once(RESERVED_NAME).chain(below.iter().copied());
    if parts.len() != below.len() + 1
        || !parts
            .iter()
            .zip(expected)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        return Err(managed_not_regular());
    }
    Ok(())
}

/// 開いたハンドルが、利用者の対象 `rel` の場所にあることを確かめ、実際の名前を返す。
pub(crate) fn check_user_target(root: &Root, file: &File, rel: &RelPath, at: At) -> Result<String> {
    user_rel(&root.path, &read_final(file, at)?, rel)
}

/// 開いた PassDir が、利用者の対象 `rel` の場所にあることを確かめ、実際の名前を返す。
pub(crate) fn check_user_dir(root: &Root, dir: &Pass, rel: &RelPath, at: At) -> Result<String> {
    check_user_target(root, dir.0.as_file(), rel, at)
}

// ---------------------------------------------------------------------------
// 利用者のパス（A：開く・B：作る）
// ---------------------------------------------------------------------------

/// 途中のフォルダを1段開いた結果。
pub(crate) enum DirStep {
    Dir(Pass),
    Missing,
    NotDir,
    Link,
}

/// 親を基準に、途中のフォルダを1段、通り抜けるだけの形で開く（リンクをたどらない。使用中・拒否は試し直す）。
pub(crate) fn step_dir(parent: ParentRef<'_>, name: &str) -> Result<DirStep> {
    step_dir_with(parent, name, true)
}

/// `step_dir` の、試し直さない形（走査の調べ直し：使用中・拒否はすぐエラー）。
pub(crate) fn step_dir_once(parent: ParentRef<'_>, name: &str) -> Result<DirStep> {
    step_dir_with(parent, name, false)
}

fn step_dir_with(parent: ParentRef<'_>, name: &str, retry: bool) -> Result<DirStep> {
    let name = single_name(name)?;
    let open = || parent.open_pass(&name);
    let opened = if retry { with_retry(open) } else { open() };
    match opened {
        Ok(OpenedPass::Dir(p)) => Ok(DirStep::Dir(Pass(p))),
        Ok(OpenedPass::Link(_)) => Ok(DirStep::Link),
        Err(e) if is_not_found(&e) => Ok(DirStep::Missing),
        Err(e) if is_not_a_dir(&e) => Ok(DirStep::NotDir),
        Err(e) if is_reparse(&e) => Ok(DirStep::Link),
        Err(e) => Err(open_failed("フォルダを開けない", &e)),
    }
}

/// 読むために最後の要素を開いた結果。
pub(crate) enum LeafRead {
    File(File),
    Dir,
    Link,
    Missing,
}

/// 親を基準に、読むために最後の要素を開く（LEAF_READ。種類は求めず、ハンドルで確かめる。使用中は試し直して BUSY）。
pub(crate) fn open_leaf_read(parent: ParentRef<'_>, name: &str) -> Result<LeafRead> {
    let name = single_name(name)?;
    match with_busy_retry(|| parent.open_child(&name, Want::Any, LEAF_READ, SHARE_ALL)) {
        Ok(Opened::File(f)) => Ok(LeafRead::File(f)),
        Ok(Opened::Dir(_)) => Ok(LeafRead::Dir),
        Ok(Opened::Link(_)) => Ok(LeafRead::Link),
        Err(e) if is_not_found(&e) => Ok(LeafRead::Missing),
        Err(e) if is_reparse(&e) => Ok(LeafRead::Link),
        Err(e) => Err(read_failed("ファイルを開けない", &e)),
    }
}

/// 属性を読むために開いた結果（書いたあとの版）。
pub(crate) enum LeafState {
    File(FileInfo),
    Dir,
    Link,
    Missing,
}

/// 親を基準に、属性を読むために最後の要素を開き、値を読んで閉じる（LEAF_ATTRS。最終のパスを確かめる）。
pub(crate) fn read_leaf_attrs(
    root: &Root,
    parent: ParentRef<'_>,
    name: &str,
    rel: &RelPath,
) -> Result<LeafState> {
    let n = single_name(name)?;
    let file = match with_retry(|| parent.open_child(&n, Want::Any, LEAF_ATTRS, SHARE_ALL)) {
        Ok(Opened::File(f)) => f,
        Ok(Opened::Dir(_)) => return Ok(LeafState::Dir),
        Ok(Opened::Link(_)) => return Ok(LeafState::Link),
        Err(e) if is_not_found(&e) => return Ok(LeafState::Missing),
        Err(e) if is_reparse(&e) => return Ok(LeafState::Link),
        Err(e) => return Err(open_failed("ファイルを調べられない", &e)),
    };
    let meta = file
        .metadata()
        .map_err(|e| VaultError::io("ファイルを調べられない", &e))?;
    check_user_target(root, &file, rel, At::Target)?;
    Ok(LeafState::File(FileInfo::from_metadata(&meta)))
}

/// 作ったフォルダ（確かめたら、ID と値を読んで閉じる。Dir は貸さない）。
pub(crate) struct NewDir {
    created: Created<Dir>,
    id: Option<FileId>,
    info: FileInfo,
    actual: String,
}

/// 親を基準に、利用者のフォルダを作る（B。FILE_CREATE）。最終のパスが `rel` の場所でなければ（起きないはず）、
/// 作ったハンドルで消して OUTSIDE_VAULT（確かめる前なので消してよい）。同じ名前があれば ALREADY_EXISTS。
pub(crate) fn create_user_dir(
    root: &Root,
    parent: ParentRef<'_>,
    name: &str,
    rel: &RelPath,
) -> Result<NewDir> {
    let n = single_name(name)?;
    let created = match parent.create_dir(&n) {
        Ok(c) => c,
        Err(e) if is_collision(&e) => {
            return Err(VaultError::new(
                ErrorCode::AlreadyExists,
                format!("すでにある：{rel}"),
            ));
        }
        Err(e) if is_reparse(&e) => return Err(crate::ops::outside_link()),
        Err(e) => return Err(open_failed("フォルダを作れない", &e)),
    };
    let checked = created
        .final_path()
        .map_err(|e| VaultError::io("作ったフォルダの場所を読めない", &e))
        .map(|p| final_path_hook!(Created, p))
        .and_then(|p| user_rel(&root.path, &p, rel));
    let actual = match checked {
        Ok(actual) => actual,
        Err(e) => {
            // 起きないはず。確かめる前なので、作ったハンドルで消す（空のフォルダ）
            let _ = delete(created);
            return Err(e);
        }
    };
    let meta = created
        .metadata()
        .map_err(|e| VaultError::io("作ったフォルダを調べられない", &e))?;
    let id = created.file_id().ok();
    Ok(NewDir {
        created,
        id,
        info: FileInfo::from_metadata(&meta),
        actual,
    })
}

impl NewDir {
    /// 実際の名前（`/` 区切り）。
    pub(crate) fn actual(&self) -> &str {
        &self.actual
    }

    /// 索引に入れる値。
    pub(crate) fn info(&self) -> FileInfo {
        self.info
    }

    /// 閉じる（このあと使わないとき。vault_mkdir の最後の要素）。
    pub(crate) fn close(self) {
        drop(self.created);
    }

    /// 閉じてから、親を基準に PassDir で開き直す。フォルダでリンクでないこと・最終のパス・
    /// ID が作ったときと同じことを確かめる。違う・消えた・ファイルなら CONFLICT、リンクなら OUTSIDE_VAULT。
    /// 作ったフォルダは消さない。
    pub(crate) fn reopen(self, root: &Root, parent: ParentRef<'_>, rel: &RelPath) -> Result<Pass> {
        let Self { created, id, .. } = self;
        drop(created);
        test_hook!(Step::AfterCreatedClosed);
        let name = rel.components().last().unwrap_or_default();
        let pass = match step_dir(parent, name)? {
            DirStep::Dir(p) => p,
            DirStep::Link => return Err(crate::ops::outside_link()),
            DirStep::Missing | DirStep::NotDir => return Err(created_changed(&rel.to_string())),
        };
        check_user_dir(root, &pass, rel, At::Reopened)?;
        if let Some(id) = id
            && os_sys::file_id(pass.0.as_file()).ok() != Some(id)
        {
            return Err(created_changed(&rel.to_string()));
        }
        Ok(pass)
    }
}

/// 空き容量を確かめる（vault_write だけ。`空き >= 中身＋1 GiB`）。
pub(crate) fn check_free_space(root: &Root, len: usize) -> Result<()> {
    let free = root.free_space()?;
    let need = u64::try_from(len)
        .unwrap_or(u64::MAX)
        .saturating_add(super::MIN_FREE_AFTER_WRITE);
    if free < need {
        return Err(VaultError::new(
            ErrorCode::DiskFull,
            "ディスクの空きが足りない（書いたあとに 1 GiB 残らない）。何も変えていない",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 管理用（.vault の下）
// ---------------------------------------------------------------------------

/// 管理用のフォルダ（.vault/tmp・.vault/journal）を開く。ないときは、`create` なら作って開き直す。
/// ない・`create` でないなら None。リンク・フォルダでないものなら NOT_A_VAULT（文は `not_regular`）。
/// 作ったものが、閉じてから開き直すまでに変えられた（ID が違う・消えた・リンク）なら NOT_A_VAULT。
pub(crate) fn open_place(
    root: &Root,
    vault: &Pass,
    name: &ManagedName,
    create: bool,
    not_regular: &str,
) -> Result<Option<Held>> {
    let n = single_name(name.as_str())?;
    for _ in 0..2 {
        match with_retry(|| vault.parent().open_child(&n, Want::Dir, HOLD_DIR, SHARE_RW)) {
            Ok(Opened::Dir(d)) => return Ok(Some(Held(d))),
            Ok(_) => return Err(not_a_vault(not_regular)),
            Err(e) if is_not_found(&e) => {
                if !create {
                    return Ok(None);
                }
            }
            Err(e) if is_not_a_dir(&e) || is_reparse(&e) => return Err(not_a_vault(not_regular)),
            Err(e) => return Err(open_failed("管理用のフォルダを開けない", &e)),
        }
        let created = match vault.parent().create_dir(&n) {
            Ok(c) => c,
            // 同時に作られた：もう一度開く
            Err(e) if is_collision(&e) => continue,
            // 作るときに名前を解けなかった（4395・1921）
            Err(e) if is_reparse(&e) => return Err(not_a_vault(not_regular)),
            Err(e) => return Err(open_failed("管理用のフォルダを作れない", &e)),
        };
        let below = name.below_vault();
        let checked = created
            .final_path()
            .map_err(|e| VaultError::io("作ったフォルダの場所を読めない", &e))
            .map(|p| final_path_hook!(Created, p))
            .and_then(|p| check_managed(&root.path, &p, &below));
        if let Err(e) = checked {
            let _ = delete(created);
            return Err(e);
        }
        let id = created.file_id().ok();
        drop(created);
        test_hook!(Step::AfterCreatedClosed);
        let held = match with_retry(|| vault.parent().open_child(&n, Want::Dir, HOLD_DIR, SHARE_RW))
        {
            Ok(Opened::Dir(d)) => Held(d),
            Ok(_) => return Err(created_replaced()),
            Err(e) if is_not_found(&e) || is_not_a_dir(&e) || is_reparse(&e) => {
                return Err(created_replaced());
            }
            Err(e) => return Err(open_failed("管理用のフォルダを開けない", &e)),
        };
        check_managed(
            &root.path,
            &read_final(held.0.as_file(), At::Reopened)?,
            &below,
        )?;
        if let Some(id) = id
            && os_sys::file_id(held.0.as_file()).ok() != Some(id)
        {
            return Err(created_replaced());
        }
        return Ok(Some(held));
    }
    Err(not_a_vault(not_regular))
}

/// 管理用のファイルを開いた結果。
pub(crate) enum ManagedFile {
    File(File),
    NotFile,
    Missing,
}

/// どの用途で開くか。
#[derive(Clone, Copy)]
pub(crate) enum ManagedUse {
    /// 読む（索引・vault.json）。
    Read,
    /// 属性だけ（索引がふつうのファイルか）。
    Attrs,
}

/// `parent`（.vault か .vault/journal）を基準に、管理用のファイルを開く（リンクをたどらない）。
/// 開けない（ない・ふつうのファイルでない、のほか）ときの IO_ERROR の文は `what`。使用中は試し直して BUSY。
pub(crate) fn open_managed_file(
    parent: ParentRef<'_>,
    name: &ManagedName,
    usage: ManagedUse,
    what: &str,
) -> Result<ManagedFile> {
    let n = single_name(name.as_str())?;
    let access = match usage {
        ManagedUse::Read => LEAF_READ,
        ManagedUse::Attrs => LEAF_ATTRS,
    };
    match with_busy_retry(|| parent.open_child(&n, Want::Any, access, SHARE_ALL)) {
        Ok(Opened::File(f)) => Ok(ManagedFile::File(f)),
        Ok(_) => Ok(ManagedFile::NotFile),
        Err(e) if is_not_found(&e) => Ok(ManagedFile::Missing),
        Err(e) if is_reparse(&e) => Ok(ManagedFile::NotFile),
        Err(e) => Err(read_failed(what, &e)),
    }
}

/// .vault/lock を開く。ないときは作る（FILE_CREATE）。リンク・フォルダなら NOT_A_VAULT。
/// ほかの処理が開いていて使用中（32・33）なら、試し直してから LOCKED。
pub(crate) fn open_lock(root: &Root, vault: &Pass) -> Result<File> {
    test_hook!(Step::Open(Place::Lock));
    let name = ManagedName::Lock;
    let n = single_name(name.as_str())?;
    for _ in 0..2 {
        match with_retry(|| {
            vault
                .parent()
                .open_child(&n, Want::Any, LOCK_FILE, SHARE_RW)
        }) {
            Ok(Opened::File(f)) => return Ok(f),
            Ok(_) => {
                return Err(not_a_vault(
                    "ロックのファイル（.vault/lock）が、ふつうのファイルでない",
                ));
            }
            Err(e) if is_not_found(&e) => {}
            Err(e) if is_reparse(&e) => {
                return Err(not_a_vault(
                    "ロックのファイル（.vault/lock）が、ふつうのファイルでない",
                ));
            }
            Err(e) if is_busy(&e) => {
                return Err(VaultError::new(
                    ErrorCode::Locked,
                    "ほかの処理が、ロックのファイルを使っている（時間内に開けなかった）",
                ));
            }
            Err(e) => return Err(VaultError::io("ロックのファイルを開けない", &e)),
        }
        match vault.parent().create_file(&n, LOCK_FILE, SHARE_RW) {
            Ok(created) => {
                let path = created
                    .get()
                    .metadata()
                    .and_then(|_| os_sys::final_path(created.get()))
                    .map_err(|e| VaultError::io("ロックのファイルを調べられない", &e))?;
                // 外と分かっても（起きないはず。親の .vault は持っている）消さない：作ったハンドルは DELETE の権利を
                // 持たない（LOCK_FILE）。空のファイルが残るだけで、NOT_A_VAULT で止める
                check_managed(
                    &root.path,
                    &final_path_hook!(Created, path),
                    &name.below_vault(),
                )?;
                return Ok(created.into_inner());
            }
            // 同時に作られた：もう一度開く
            Err(e) if is_collision(&e) => {}
            Err(e) if is_reparse(&e) => {
                return Err(not_a_vault(
                    "ロックのファイル（.vault/lock）が、ふつうのファイルでない",
                ));
            }
            Err(e) => return Err(VaultError::io("ロックのファイルを作れない", &e)),
        }
    }
    Err(VaultError::new(
        ErrorCode::IoError,
        "ロックのファイルを開けない（作るのと同時に、ほかの処理が置き換えた）",
    ))
}

/// 履歴のファイルを、追記と読みのために開く（JOURNAL_APPEND。DELETE を共有しない）。
/// リンク・ふつうのファイルでないものは NOT_A_VAULT。使用中は試し直してから BUSY。
pub(crate) fn open_journal_file(dir: &Held, name: &ManagedName) -> Result<File> {
    let n = single_name(name.as_str())?;
    match with_retry(|| {
        dir.parent()
            .open_child(&n, Want::Any, JOURNAL_APPEND, SHARE_RW)
    }) {
        Ok(Opened::File(f)) => Ok(f),
        Ok(_) => Err(not_a_vault("履歴のファイルが、ふつうのファイルでない")),
        Err(e) if is_reparse(&e) => Err(not_a_vault("履歴のファイルが、ふつうのファイルでない")),
        Err(e) => Err(open_failed("履歴のファイルを開けない", &e)),
    }
}

/// 開いた履歴のファイルが、ほかの名前とハードリンクでつながっているか（同じハンドルで読んだリンクの数が2以上）。
/// 読めなければ IO_ERROR（書かない側）。
///
/// 履歴の追記は、その場で中身を書く唯一の道なので、書く前にリンクの数を読むのはこの関数だけ（JournalWriter の
/// open と、commit の書く直前から呼ぶ）。つながっていれば、呼ぶ側はそのファイルに書かず、新しい
/// 履歴のファイルに切り替える（ハードリンクで残すバックアップの写しを書き換えない）。
pub(crate) fn journal_is_linked(file: &File) -> Result<bool> {
    #[cfg(feature = "test-support")]
    if let Some(fake) = crate::hooks::take_journal_links() {
        return match fake {
            crate::hooks::FakeLinks::Count(n) => Ok(n >= 2),
            crate::hooks::FakeLinks::Fail => Err(VaultError::new(
                ErrorCode::IoError,
                "履歴のファイルを調べられない（テストの差し込み）",
            )),
        };
    }
    os_sys::link_count(file)
        .map(|n| n >= 2)
        .map_err(|e| VaultError::io("履歴のファイルを調べられない", &e))
}

/// 新しい履歴のファイルを作る（FILE_CREATE。JOURNAL_APPEND）。最終のパスが `.vault\journal\<名前>` でなければ
/// （起きないはず。親の journal は持っている）NOT_A_VAULT。作ったハンドルは DELETE の権利を持たないので、
/// そのときも消さない（空のファイルが残るだけ）。作ったものの記録（ID は書く前に
/// 作ったハンドルで読む）も返す（init の片づけ）。同じ名前のものがすでにあれば None（上書きしない）。
pub(crate) fn create_journal_file(
    root: &Root,
    dir: &Held,
    name: &ManagedName,
) -> Result<Option<(File, Made)>> {
    let n = single_name(name.as_str())?;
    let created = match dir.parent().create_file(&n, JOURNAL_APPEND, SHARE_RW) {
        Ok(c) => c,
        // 同じ名前のものがすでにある（FILE_CREATE なので、何も上書きしていない）：呼ぶ側が名前を決め直す
        Err(e) if is_collision(&e) => return Ok(None),
        Err(e) if is_reparse(&e) => {
            return Err(not_a_vault("履歴のファイルが、ふつうのファイルでない"));
        }
        Err(e) => return Err(io_or_disk_full("履歴のファイルを作れない", &e)),
    };
    let path = os_sys::final_path(created.get())
        .map_err(|e| VaultError::io("履歴のファイルの場所を読めない", &e))?;
    check_managed(
        &root.path,
        &final_path_hook!(Created, path),
        &name.below_vault(),
    )?;
    let made = Made::from_file(MadeItem::InitFile(name.clone()), &created);
    Ok(Some((created.into_inner(), made)))
}

/// 履歴のファイルを、読むために開く（LEAF_READ）。リンク・ふつうのファイルでないものは NOT_A_VAULT。
pub(crate) fn open_journal_for_reading(dir: &Held, name: &ManagedName) -> Result<File> {
    match open_managed_file(
        dir.parent(),
        name,
        ManagedUse::Read,
        "履歴のファイルを開けない",
    )? {
        ManagedFile::File(f) => Ok(f),
        ManagedFile::NotFile => Err(not_a_vault("履歴のファイルが、ふつうのファイルでない")),
        ManagedFile::Missing => Err(VaultError::new(
            ErrorCode::IoError,
            "履歴のファイルを開けない（読むあいだに消えた）",
        )),
    }
}

/// 書けなかった索引を消す（決まった名前の管理用のファイルだけ。ふつうのファイルで `.vault\index.json` にあるなら、
/// 読み取り専用の印が付いていても消す。リンク・フォルダは消さない）。
pub(crate) fn remove_index(root: &Root, vault: &Pass) {
    test_hook!(Step::Open(Place::Index));
    if let Some(index) = open_managed(&root.path, vault.parent(), Removable::IndexJson) {
        let _ = delete(index);
    }
}

// ---------------------------------------------------------------------------
// 一時ファイル
// ---------------------------------------------------------------------------

/// 作った一時ファイル（`.vault/tmp` の下。中身を書く前に、最終のパスを確かめたもの）。
pub(crate) struct TmpFile {
    dir: Held,
    file: Created<File>,
    name: ManagedName,
}

/// rename の結果。
pub(crate) enum Moved {
    Done,
    /// 同じ名前がすでにあった（置き換えない rename）。何も動いていない。
    Collision,
}

/// `.vault/tmp` を開き（ないなら作る）、一時ファイルを作る（B）。最終のパスが `.vault\tmp\<名前>` でなければ、
/// 中身を書かずに、作ったハンドルで消して NOT_A_VAULT。
pub(crate) fn create_tmp(root: &Root, vault: &Pass) -> Result<TmpFile> {
    test_hook!(Step::Open(Place::Tmp));
    let dir = open_place(root, vault, &ManagedName::Tmp, true, MANAGEMENT_DIR_NOT_DIR)?
        .ok_or_else(|| VaultError::new(ErrorCode::Internal, "一時ファイルのフォルダがない"))?;
    let name = ManagedName::new_tmp_file();
    let n = single_name(name.as_str())?;
    let file = match dir.parent().create_file(&n, TMP_CREATE, SHARE_R) {
        Ok(f) => f,
        Err(e) if is_reparse(&e) => return Err(managed_not_regular()),
        Err(e) => return Err(io_or_disk_full("一時ファイルを作れない", &e)),
    };
    let checked = os_sys::final_path(file.get())
        .map_err(|e| VaultError::io("一時ファイルの場所を読めない", &e))
        .map(|p| final_path_hook!(Created, p))
        .and_then(|p| check_managed(&root.path, &p, &name.below_vault()));
    if let Err(e) = checked {
        let _ = delete(file);
        return Err(e);
    }
    test_hook!(Step::AfterTmpCreated);
    Ok(TmpFile { dir, file, name })
}

impl TmpFile {
    /// 中身を書いて sync する。失敗したら、作ったハンドルで消す。
    pub(crate) fn write_all(self, bytes: &[u8]) -> Result<Self> {
        let mut writer: &File = self.file.get();
        if let Err(e) = writer.write_all(bytes).and_then(|()| writer.sync_all()) {
            self.discard();
            return Err(io_or_disk_full("一時ファイルに書けない", &e));
        }
        Ok(self)
    }

    /// 作ったハンドルで消す（D）。
    pub(crate) fn discard(self) {
        let _ = delete(self.file);
    }

    /// 置き換えない rename（C）で、書く先（`parent` の `name`）へ置く。使用中・拒否は試し直す。
    /// 置けたら `Placed`（消す関数に渡せない型）に替わる。同じ名前があれば `Collision`。
    pub(crate) fn place(self, parent: &dyn RenameParent, name: &str) -> Placing {
        let n = match single_name(name) {
            Ok(n) => n,
            Err(e) => return Placing::Failed(self, e),
        };
        let to = parent.rename_to();
        match with_retry(|| to.rename_no_replace(self.file.get(), &n)) {
            Ok(()) => Placing::Placed(Placed { tmp: self }),
            Err(e) if is_collision(&e) => Placing::Collision(self),
            Err(e) if is_reparse(&e) => Placing::Failed(self, crate::ops::outside_link()),
            Err(e) => Placing::Failed(self, busy_or_io("書いたファイルを置き換えられない", &e)),
        }
    }

    /// 置き換える rename（C'。置いてよいのは `Replaceable`＝索引と vault.json だけ）。失敗したら、作ったハンドルで消す。
    /// rename のあと、最終のパスが `.vault\<名前>` であることを確かめる。
    pub(crate) fn replace_managed(
        self,
        root: &Root,
        vault: &Pass,
        name: Replaceable,
    ) -> Result<Written> {
        let name = name.name();
        let n = single_name(name.as_str())?;
        test_hook!(Step::BeforeRename(Rename::ReplaceManaged));
        if let Err(e) = with_retry(|| vault.rename_to().rename_replace(self.file.get(), &n)) {
            self.discard();
            return Err(busy_or_io("書いたファイルを置き換えられない", &e));
        }
        let path = os_sys::final_path(self.file.get())
            .map_err(|e| VaultError::io("書いたファイルの場所を読めない", &e))?;
        check_managed(
            &root.path,
            &final_path_hook!(Renamed, path),
            &name.below_vault(),
        )?;
        // 置いたファイルの ID（init の片づけで、作った索引を照らす）
        Ok(Written(os_sys::file_id(self.file.get()).ok()))
    }
}

/// 置き換えない rename（C）で、一時ファイルを書く先へ置いた結果。
pub(crate) enum Placing {
    /// 置いた。消す関数に渡せない `Placed` に替わる。
    Placed(Placed),
    /// 同じ名前がすでにあった。何も動いていない（一時ファイルのまま）。
    Collision(TmpFile),
    /// rename に失敗した（一時ファイルのまま）。
    Failed(TmpFile, VaultError),
}

/// 書く先へ置いた一時ファイル。`Deletable` を実装しない（利用者の場所にあるものを、作ったハンドルで消す道を作らない）。
/// 消せるのは、「起きないはず」で `.vault/tmp` へ戻せたあとだけ（`move_back`）。
pub(crate) struct Placed {
    tmp: TmpFile,
}

impl Placed {
    /// rename のあと、同じハンドルで最終のパスを読み直し、利用者の対象 `rel` の場所かを確かめて、実際の名前を返す。
    pub(crate) fn check_renamed(&self, root: &Root, rel: &RelPath) -> Result<String> {
        check_user_target(root, self.tmp.file.get(), rel, At::Renamed)
    }

    /// 「起きないはず」：置いたものを、置き換えずに .vault/tmp の元の名前へ戻し、戻せたら作ったハンドルで消す。
    /// 戻せなければ消さずに、その旨をエラーの文に足す。
    pub(crate) fn move_back(self, e: VaultError, rel: &RelPath) -> VaultError {
        test_hook!(Step::BeforeRenameBack);
        let tmp = self.tmp;
        let back = single_name(tmp.name.as_str())
            .ok()
            .map(|n| with_retry(|| tmp.dir.rename_to().rename_no_replace(tmp.file.get(), &n)));
        match back {
            Some(Ok(())) => {
                tmp.discard();
                e
            }
            _ => e.with_note(&format!(
                "書いたファイルを元の場所へ戻せなかった（書いた先：{rel}）"
            )),
        }
    }
}

/// 管理用のファイル（索引・vault.json）を、一時ファイルに書いてから置き換える（B のあと C'）。置いたファイルの ID
/// を返す（init の片づけの記録）。
pub(crate) fn atomic_write(
    root: &Root,
    vault: &Pass,
    name: Replaceable,
    bytes: &[u8],
) -> Result<Written> {
    create_tmp(root, vault)?
        .write_all(bytes)?
        .replace_managed(root, vault, name)
}

/// 管理用のファイルを置き換えた結果：置いたファイルの ID（置いた一時ファイルのハンドルで読む。読めなければ None）。
#[must_use]
pub(crate) struct Written(Option<FileId>);

// ---------------------------------------------------------------------------
// init の作る処理と片づけ
// ---------------------------------------------------------------------------

/// init の「いちばん深いある祖先」を開く権利（作る親としてだけ使う。列挙しない。リンクをたどって開く）。
const INIT_BASE: Access = Access::TRAVERSE.with(Access::READ_ATTRIBUTES);

/// init が作ったものの種類。
#[derive(Debug)]
enum MadeItem {
    /// 保管庫のフォルダとその上に作ったフォルダ（何段目か、親を基準にした名前）。
    TailDir(usize, Name),
    /// `.vault`。
    VaultDir,
    /// `.vault/trash`・`journal`・`tmp`。
    InitDir(ManagedName),
    /// `.vault/lock`・履歴のファイル・`.vault/index.json`。
    InitFile(ManagedName),
}

/// init が作ったものの記録。作ったハンドル（`Created<Dir>`・`Created<File>`）と、管理用のファイルを置き換えた
/// 結果（`Written`）からだけ、guard の中で作る（外では作れない）。ID を読めなければ None（片づけで消さずに
/// 残す。設計の決定）。
#[derive(Debug)]
pub(crate) struct Made {
    item: MadeItem,
    id: Option<FileId>,
}

impl Made {
    fn from_dir(item: MadeItem, created: &Created<Dir>) -> Self {
        Self {
            item,
            id: created.file_id().ok(),
        }
    }

    fn from_file(item: MadeItem, created: &Created<File>) -> Self {
        Self {
            item,
            id: os_sys::file_id(created.get()).ok(),
        }
    }

    fn from_written(item: MadeItem, written: &Written) -> Self {
        Self {
            item,
            id: written.0,
        }
    }
}

/// init の作る処理と片づけ。
///
/// - `begin`：入力のパスを深いほうから開き（リンクをたどる）、最初に開けたもの（base）を持つ。保管庫にするフォルダ
///   そのものなら HOLD_DIR、祖先なら INIT_BASE。target は「base の最終のパス＋残りの名前（tail）」。
/// - `prepare_root`：tail がなければ base（空のフォルダ）が根。あれば1段ずつ、持っている親を基準に作り
///   （CREATE_DIR）、最終のパスが「親＋1つの要素」か確かめ（外なら作ったハンドルで消して CONFLICT）、ID を記録して
///   閉じ、開き直して（途中は PassDir、根は HOLD_DIR）最終のパスと ID を照らす（違えば CONFLICT）。
/// - `create_management`：`.vault`（開き直して ID を照らす）・`trash`・`journal`・`tmp`・`lock` を作り、記録する。
/// - `undo`：記録したものだけを、逆の順で、持っている親を基準にリンクをたどらずに開き直し、場所と ID が記録と
///   一致したときだけ消す（guard::delete）。残したものは、もとのエラーに注で書く（相対の名前だけ）。
pub(crate) struct InitScope {
    /// 祖先（tail があるとき。INIT_BASE）。
    base: Option<Dir>,
    base_path: PathBuf,
    base_id: Option<FileId>,
    /// 入力のパスの途中か、そのものが、フォルダでない（check_root のあとでエラーにする）。
    not_dir: bool,
    /// 入力のパスそのものが、先のないリンク（宙ぶらりんのジャンクションなど）。check_root のあとで、今までと同じ
    /// INVALID_ROOT「フォルダでないものは保管庫にできない」にする。
    target_is_link: bool,
    tail: Vec<Name>,
    target: PathBuf,
    /// 作って開き直した途中の段（最後の段は `root`）。
    passes: Vec<Pass>,
    root: Option<Root>,
    vault: Option<Pass>,
    made: RefCell<Vec<Made>>,
}

fn init_invalid_root(message: &str) -> VaultError {
    VaultError::new(ErrorCode::InvalidRoot, message)
}

fn init_internal() -> VaultError {
    VaultError::new(ErrorCode::Internal, "init の順番が正しくない")
}

impl InitScope {
    /// 入力のパスを深いほうから開き、最初に開けたものを base にする（何も作らない）。
    pub(crate) fn begin(path: &Path) -> Result<Self> {
        // 名前を、何かを開く前に確かめる（`:` などがあれば、何も作らない）
        let names_ok = path.components().all(|c| match c {
            std::path::Component::Normal(name) => Name::new(name).is_ok(),
            _ => true,
        });
        if !names_ok {
            return Err(init_invalid_root(
                "保管庫のフォルダの名前に使えない文字がある",
            ));
        }
        let mut target_is_link = false;
        for (depth, ancestor) in path.ancestors().enumerate() {
            let is_target = depth == 0;
            let access = if is_target { HOLD_DIR } else { INIT_BASE };
            let opened = match os_sys::open_path(ancestor, Links::Follow, access, SHARE_RW) {
                Ok(opened) => opened,
                Err(e) if is_not_found(&e) || is_not_a_dir(&e) => {
                    // そのものが見つからないときは、リンクをたどらずに開き直し、先のないリンクかを見る
                    if is_target {
                        target_is_link = matches!(
                            os_sys::open_path(ancestor, Links::DontFollow, LEAF_ATTRS, SHARE_RW),
                            Ok(Opened::Link(_))
                        );
                    }
                    continue;
                }
                // 名前が正しくない（123＝ERROR_INVALID_NAME。`*`・`?` など、Name の決まりは通るが OS が断るもの）
                Err(e) if e.raw_os_error() == Some(123) => {
                    return Err(init_invalid_root(
                        "保管庫のフォルダの名前に使えない文字がある",
                    ));
                }
                Err(e) => return Err(VaultError::io("保管庫のフォルダの場所を調べられない", &e)),
            };
            let rest = path
                .strip_prefix(ancestor)
                .map_err(|_| VaultError::new(ErrorCode::Internal, "パスを分けられない"))?;
            let mut tail = Vec::new();
            for c in rest.components() {
                let std::path::Component::Normal(name) = c else {
                    return Err(init_invalid_root("パスの形が正しくない"));
                };
                tail.push(Name::new(name).map_err(|_| {
                    init_invalid_root("保管庫のフォルダの名前に使えない文字がある")
                })?);
            }
            let (dir, not_dir, file) = match opened {
                Opened::Dir(d) => (Some(d), false, None),
                Opened::File(f) | Opened::Link(f) => (None, true, Some(f)),
            };
            let handle = match (&dir, &file) {
                (Some(d), _) => d.as_file(),
                (None, Some(f)) => f,
                (None, None) => return Err(init_internal()),
            };
            let base_path = os_sys::final_path(handle)
                .map_err(|e| VaultError::io("保管庫のフォルダの場所を調べられない", &e))?;
            let base_id = os_sys::file_id(handle).ok();
            let mut target = base_path.clone();
            for name in &tail {
                target.push(name.to_os_string());
            }
            let mut scope = Self {
                base: None,
                base_path: base_path.clone(),
                base_id,
                not_dir,
                target_is_link,
                tail,
                target,
                passes: Vec::new(),
                root: None,
                vault: None,
                made: RefCell::new(Vec::new()),
            };
            match dir {
                Some(d) if is_target => {
                    scope.root = Some(Root {
                        held: Held(d),
                        path: base_path,
                    });
                }
                Some(d) => scope.base = Some(d),
                None => {}
            }
            return Ok(scope);
        }
        Err(init_invalid_root(
            "保管庫のフォルダの場所が見つからない（ドライブがない など）",
        ))
    }

    /// 保管庫にするフォルダの場所（base の最終のパス＋tail）。check_root に渡す。
    pub(crate) fn target(&self) -> &Path {
        &self.target
    }

    /// 保管庫にするフォルダがすでにあり、begin で開いて持っているとき、入力のパスを最後の要素のリンクをたどらずに
    /// 開いたものが、そのフォルダ（同じ ID）か（既定の保管庫だけ）。まだないときは true（作る所で、
    /// 作ったものの最終のパスが「置き場所＋名前」と同じかを確かめる）。持っているフォルダは名前を変えられないので、
    /// あいだで差し替えられることはない。
    pub(crate) fn reached_without_links(&self, path: &Path) -> Result<bool> {
        if self.root.is_none() {
            return Ok(true);
        }
        is_same_dir_without_links(path, &self.base_id)
            .map_err(|e| VaultError::io("保管庫のフォルダの場所を調べられない", &e))
    }

    /// `tail` の最初の `n` 個（相対の名前。注とエラーの文に使う）。
    fn tail_rel(&self, n: usize) -> String {
        self.tail[..n]
            .iter()
            .map(|name| name.to_os_string().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// 根を用意する（check_root のあと）。tail がなければ base が空のフォルダであることを確かめ、あれば1段ずつ作る。
    pub(crate) fn prepare_root(&mut self) -> Result<()> {
        if self.target_is_link {
            return Err(init_invalid_root("フォルダでないものは保管庫にできない"));
        }
        if self.not_dir {
            return Err(if self.tail.is_empty() {
                init_invalid_root("フォルダでないものは保管庫にできない")
            } else {
                VaultError::new(
                    ErrorCode::IoError,
                    "保管庫のフォルダを作れない（途中に、フォルダでないものがある）",
                )
            });
        }
        if let Some(root) = &mut self.root {
            let entries = list_dir(&mut root.held.0, "フォルダの中を調べられない")?;
            if entries
                .iter()
                .any(|e| e.name.eq_ignore_ascii_case(RESERVED_NAME))
            {
                return Err(crate::vault::already_a_vault());
            }
            if !entries.is_empty() {
                return Err(init_invalid_root(
                    "空でないフォルダは保管庫にできない（隠しファイルも数える）",
                ));
            }
            return Ok(());
        }
        let mut parent_path = self.base_path.clone();
        for level in 0..self.tail.len() {
            let name = self.tail[level].clone();
            let last = level + 1 == self.tail.len();
            let rel = self.tail_rel(level + 1);
            let expected = parent_path.join(name.to_os_string());
            let parent = match level {
                0 => ParentRef(ParentKind::InitBase(
                    self.base.as_ref().ok_or_else(init_internal)?,
                )),
                _ => self.passes[level - 1].parent(),
            };
            // ①：作る（同じ名前が先にできていた・作れない（拒否も）は、今と同じ IO_ERROR）
            let created = parent.create_dir(&name).map_err(|e| {
                if is_reparse(&e) {
                    created_changed(&rel)
                } else {
                    VaultError::io("保管庫のフォルダを作れない", &e)
                }
            })?;
            let path = created
                .final_path()
                .map_err(|e| VaultError::io("作ったフォルダの場所を読めない", &e))?;
            if final_path_hook!(Created, path) != expected {
                // ②：外と分かった（起きないはず。親は持っている）：作ったハンドルで消す
                let _ = delete(created);
                return Err(created_changed(&rel));
            }
            // ③：ID を記録して閉じる（base と同じ ID なら記録しない）
            let made = Made::from_dir(MadeItem::TailDir(level, name.clone()), &created);
            let id = made.id;
            if id.is_none() || id != self.base_id {
                self.made.borrow_mut().push(made);
            }
            drop(created);
            test_hook!(Step::AfterCreatedClosed);
            // ④・⑤：開き直して、フォルダでリンクでないこと・最終のパス・ID を確かめる（違えば CONFLICT。作ったものは
            // 消さず、片づけに任せる）
            let replaced = |e: &io::Error| is_not_found(e) || is_not_a_dir(e) || is_reparse(e);
            if last {
                let held =
                    match with_retry(|| parent.open_child(&name, Want::Dir, HOLD_DIR, SHARE_RW)) {
                        Ok(Opened::Dir(d)) => Held(d),
                        Ok(_) => return Err(created_changed(&rel)),
                        Err(e) if replaced(&e) => return Err(created_changed(&rel)),
                        Err(e) => return Err(VaultError::io("保管庫のフォルダを開けない", &e)),
                    };
                check_reopened(held.0.as_file(), &expected, id, &rel)?;
                self.root = Some(Root {
                    held,
                    path: expected.clone(),
                });
            } else {
                let pass = match with_retry(|| parent.open_pass(&name)) {
                    Ok(OpenedPass::Dir(p)) => Pass(p),
                    Ok(OpenedPass::Link(_)) => return Err(created_changed(&rel)),
                    Err(e) if replaced(&e) => return Err(created_changed(&rel)),
                    Err(e) => return Err(VaultError::io("保管庫のフォルダを開けない", &e)),
                };
                check_reopened(pass.0.as_file(), &expected, id, &rel)?;
                self.passes.push(pass);
            }
            parent_path = expected;
        }
        Ok(())
    }

    /// 用意した根（prepare_root のあと）。
    pub(crate) fn root(&self) -> Result<&Root> {
        self.root.as_ref().ok_or_else(init_internal)
    }

    /// 作って開き直した `.vault`（create_management のあと）。
    pub(crate) fn vault(&self) -> Result<&Pass> {
        self.vault.as_ref().ok_or_else(init_internal)
    }

    /// `.vault` とその下の `trash`・`journal`・`tmp`・`lock` を作り、記録する。
    pub(crate) fn create_management(&mut self) -> Result<()> {
        let root = self.root.as_ref().ok_or_else(init_internal)?;
        let n = single_name(RESERVED_NAME)?;
        let created = match root.held.parent().create_dir(&n) {
            Ok(c) => c,
            // create_dir_all でなく作るので、同じフォルダを同時に作ろうとした2つめを止める
            Err(e) if is_collision(&e) => return Err(crate::vault::already_a_vault()),
            Err(e) if is_reparse(&e) => return Err(managed_not_regular()),
            Err(e) => return Err(VaultError::io("管理用のフォルダを作れない", &e)),
        };
        let path = created
            .final_path()
            .map_err(|e| VaultError::io("作ったフォルダの場所を読めない", &e))?;
        if check_managed(&root.path, &final_path_hook!(Created, path), &[]).is_err() {
            let _ = delete(created);
            return Err(managed_not_regular());
        }
        let made = Made::from_dir(MadeItem::VaultDir, &created);
        let id = made.id;
        self.made.borrow_mut().push(made);
        drop(created);
        test_hook!(Step::AfterCreatedClosed);
        // 開き直して、場所と ID を照らす（違えば NOT_A_VAULT。差し替えたものは消さず、片づけに任せる）
        let reopened = root.open_vault_dir()?;
        check_managed(
            &root.path,
            &read_final(reopened.0.as_file(), At::Reopened)?,
            &[],
        )?;
        if let Some(id) = id
            && os_sys::file_id(reopened.0.as_file()).ok() != Some(id)
        {
            return Err(created_replaced());
        }
        let vault = self.vault.insert(reopened);
        for name in [ManagedName::Trash, ManagedName::Journal, ManagedName::Tmp] {
            let n = single_name(name.as_str())?;
            let created = vault.parent().create_dir(&n).map_err(|e| {
                if is_reparse(&e) {
                    managed_not_regular()
                } else {
                    VaultError::io("管理用のフォルダを作れない", &e)
                }
            })?;
            let path = created
                .final_path()
                .map_err(|e| VaultError::io("作ったフォルダの場所を読めない", &e))?;
            if check_managed(
                &root.path,
                &final_path_hook!(Created, path),
                &name.below_vault(),
            )
            .is_err()
            {
                let _ = delete(created);
                return Err(managed_not_regular());
            }
            self.made
                .borrow_mut()
                .push(Made::from_dir(MadeItem::InitDir(name), &created));
        }
        let name = ManagedName::Lock;
        let n = single_name(name.as_str())?;
        let created = vault
            .parent()
            .create_file(&n, LOCK_FILE, SHARE_RW)
            .map_err(|e| {
                if is_reparse(&e) {
                    managed_not_regular()
                } else {
                    VaultError::io("ロックのファイルを作れない", &e)
                }
            })?;
        let path = os_sys::final_path(created.get())
            .map_err(|e| VaultError::io("ロックのファイルを調べられない", &e))?;
        // 外と分かっても（起きないはず）消さない：作ったハンドルは DELETE の権利を持たない（open_lock と同じ）
        check_managed(
            &root.path,
            &final_path_hook!(Created, path),
            &name.below_vault(),
        )?;
        self.made
            .borrow_mut()
            .push(Made::from_file(MadeItem::InitFile(name), &created));
        Ok(())
    }

    /// 作ったものの記録を足す（履歴のファイル）。
    pub(crate) fn record(&self, made: Made) {
        self.made.borrow_mut().push(made);
    }

    /// 置き換えた管理用のファイル（索引）を記録する。
    pub(crate) fn record_written(&self, name: ManagedName, written: &Written) {
        self.record(Made::from_written(MadeItem::InitFile(name), written));
    }

    /// 片づける（①〜⑥）。消すのは記録したものだけで、場所と ID が記録と一致したときだけ。消せなかった・
    /// 確かめられなかったものは残し、もとのエラーに注を足して返す（片づけで出たエラーで、もとのエラーを替えない）。
    pub(crate) fn undo(self, e: VaultError) -> VaultError {
        // ①
        test_hook!(Step::BeforeInitUndo);
        let tail_names: Vec<String> = self
            .tail
            .iter()
            .map(|name| name.to_os_string().to_string_lossy().into_owned())
            .collect();
        let prefix = tail_names.join("/");
        let label = |below: &[&str]| {
            let mut parts: Vec<&str> = Vec::new();
            if !prefix.is_empty() {
                parts.push(&prefix);
            }
            parts.push(RESERVED_NAME);
            parts.extend(below);
            parts.join("/")
        };
        let Self {
            base,
            passes,
            root,
            vault,
            made,
            ..
        } = self;
        let made = made.into_inner();
        let mut left: Vec<String> = Vec::new();
        let removed = |managed: Option<Managed>| matches!(managed.map(delete), Some(Ok(())));

        if let Some(root) = &root {
            // ②：ファイル（索引・履歴のファイル・lock）を逆の順で
            if let Some(vault) = &vault {
                let journal_id = made.iter().find_map(|m| match &m.item {
                    MadeItem::InitDir(ManagedName::Journal) => m.id,
                    _ => None,
                });
                let mut journal: Option<Pass> = None;
                for m in made.iter().rev() {
                    let MadeItem::InitFile(name) = &m.item else {
                        continue;
                    };
                    let below = name.below_vault();
                    let gone = match (m.id, name) {
                        (Some(id), ManagedName::JournalFile(_)) => {
                            if journal.is_none() {
                                journal = open_init_journal(vault, journal_id);
                            }
                            journal.as_ref().is_some_and(|dir| {
                                removed(open_managed(
                                    &root.path,
                                    dir.parent(),
                                    Removable::InitFile(name.clone(), id),
                                ))
                            })
                        }
                        (Some(id), _) => removed(open_managed(
                            &root.path,
                            vault.parent(),
                            Removable::InitFile(name.clone(), id),
                        )),
                        (None, _) => false,
                    };
                    if !gone {
                        left.push(label(&below));
                    }
                }
                // 履歴のファイルを照らすために開いた journal は、③の前に閉じる
                drop(journal);
                // ③：フォルダ（tmp・journal・trash）を逆の順で
                for m in made.iter().rev() {
                    let MadeItem::InitDir(name) = &m.item else {
                        continue;
                    };
                    let gone = m.id.is_some_and(|id| {
                        removed(open_managed(
                            &root.path,
                            vault.parent(),
                            Removable::InitDir(name.clone(), id),
                        ))
                    });
                    if !gone {
                        left.push(label(&name.below_vault()));
                    }
                }
            }
            // ④：.vault（持っていたハンドルを閉じてから、根を基準に開き直す）
            drop(vault);
            if let Some(m) = made.iter().find(|m| matches!(m.item, MadeItem::VaultDir)) {
                test_hook!(Step::BeforeInitRemove(Removing::VaultDir));
                let gone = m.id.is_some_and(|id| {
                    removed(open_managed(
                        &root.path,
                        root.held.parent(),
                        Removable::VaultDir(id),
                    ))
                });
                if !gone {
                    left.push(label(&[]));
                }
            }
        }

        // ⑤：作った保管庫のフォルダとその上のフォルダを、深いほうから1段ずつ（その段のハンドルを閉じてから、持っている
        // 1つ上の親を基準に開き直す）
        let mut root = root;
        let mut passes = passes;
        let mut tails: Vec<(usize, &Name, Option<FileId>)> = made
            .iter()
            .filter_map(|m| match &m.item {
                MadeItem::TailDir(level, name) => Some((*level, name, m.id)),
                _ => None,
            })
            .collect();
        tails.sort_by_key(|(level, _, _)| std::cmp::Reverse(*level));
        for (level, name, id) in tails {
            if level + 1 == tail_names.len() {
                root = None;
            }
            passes.truncate(level);
            test_hook!(Step::BeforeInitRemove(Removing::CreatedDir));
            let parent = match level {
                0 => base.as_ref().map(|d| ParentRef(ParentKind::InitBase(d))),
                _ => passes.get(level - 1).map(ParentDir::parent),
            };
            let gone = match (parent, id) {
                (Some(parent), Some(id)) => removed(open_managed(
                    Path::new(""),
                    parent,
                    Removable::TailDir(name.clone(), id),
                )),
                _ => false,
            };
            if !gone {
                left.push(tail_names[..=level].join("/"));
            }
        }
        drop((root, passes, base));

        // ⑥
        if left.is_empty() {
            e
        } else {
            e.with_note(&format!(
                "片づけで残したもの：{}（ほかの処理が変えた・中に何かある。中を確かめてから消す）",
                left.join("・")
            ))
        }
    }
}

/// 作って開き直したフォルダが、作ったものと同じか（最終のパスと ID）。違えば CONFLICT。
fn check_reopened(file: &File, expected: &Path, id: Option<FileId>, rel: &str) -> Result<()> {
    if read_final(file, At::Reopened)? != expected {
        return Err(created_changed(rel));
    }
    if let Some(id) = id
        && os_sys::file_id(file).ok() != Some(id)
    {
        return Err(created_changed(rel));
    }
    Ok(())
}

/// init の片づけで、履歴のファイルを照らすために `.vault/journal` を通り抜けるだけの形で開く（記録の ID と同じ
/// ときだけ）。
fn open_init_journal(vault: &Pass, id: Option<FileId>) -> Option<Pass> {
    let id = id?;
    let n = single_name(ManagedName::Journal.as_str()).ok()?;
    match with_retry(|| vault.parent().open_pass(&n)).ok()? {
        OpenedPass::Dir(p) if os_sys::file_id(p.as_file()).ok() == Some(id) => Some(Pass(p)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 消す（D）：型で縛った1つの関数
// ---------------------------------------------------------------------------

mod deletable {
    use std::fs::File;

    use os_sys::{Created, Dir};

    /// 消すハンドル。
    pub enum Doomed<'a> {
        /// 作ったファイル・決まった名前の管理用のもの（ファイルか、ゴミ箱の `<id>`）。
        Handle(&'a File),
        /// 作ったフォルダ（確かめる前だけ。os-sys は Dir を貸さない）。
        CreatedDir(&'a Created<Dir>),
    }

    /// この中（guard の Windows の部品）だけで実装する。外では実装できない。
    pub trait Sealed {
        fn doomed(&self) -> Doomed<'_>;
    }
}

/// 消してよいもの：作ったもの（`Created<File>`・`Created<Dir>`）と、決まった名前の管理用のもの（`Managed`）だけ。
/// 利用者の名前から開いたハンドル（`Target`・`Pass` など）は実装しない。
trait Deletable: deletable::Sealed {}

impl deletable::Sealed for Created<File> {
    fn doomed(&self) -> deletable::Doomed<'_> {
        deletable::Doomed::Handle(self.get())
    }
}
impl Deletable for Created<File> {}

impl deletable::Sealed for Created<Dir> {
    fn doomed(&self) -> deletable::Doomed<'_> {
        deletable::Doomed::CreatedDir(self)
    }
}
impl Deletable for Created<Dir> {}

impl deletable::Sealed for Managed {
    fn doomed(&self) -> deletable::Doomed<'_> {
        deletable::Doomed::Handle(self.file())
    }
}
impl Deletable for Managed {}

/// 消す（D。消す処理はこの関数だけ）。FileDispositionInfoEx（POSIX・読み取り専用の印も）で消し、閉じる。
/// 中が空でないフォルダは 145 で失敗し、そのまま残る。
fn delete(item: impl Deletable) -> io::Result<()> {
    let result = match item.doomed() {
        deletable::Doomed::Handle(file) => os_sys::delete_by_handle(file),
        deletable::Doomed::CreatedDir(dir) => dir.delete_by_handle(),
    };
    drop(item);
    result
}

/// 決まった名前の管理用のもの（`open_managed` だけが作る。消すためだけに開く）。
struct Managed(ManagedHandle);

enum ManagedHandle {
    File(File),
    Dir(Dir),
}

impl Managed {
    fn file(&self) -> &File {
        match &self.0 {
            ManagedHandle::File(f) => f,
            ManagedHandle::Dir(d) => d.as_file(),
        }
    }
}

/// 消してよい、決まった名前の管理用のものと、init が作ったもの。利用者の名前（RelPath の
/// 要素）からは作れない。init の4つ（InitFile・InitDir・VaultDir・TailDir）を作るのは InitScope の片づけの中だけで、
/// どれも記録の ID と一致したときだけ消す。
enum Removable {
    /// `.vault/index.json`（書けなかったとき）。
    IndexJson,
    /// `.vault/trash/<id>/entry.json`（戻したあと）。
    EntryJson(TrashId),
    /// `.vault/trash/<id>`（後始末・取り戻し・戻したあと）。作ったとき・開いたときに読んだ ID と照らす。
    TrashDir(TrashId, FileId),
    /// init が作った管理用のファイル（`.vault/lock`・履歴のファイル・`.vault/index.json`）。
    InitFile(ManagedName, FileId),
    /// init が作った管理用のフォルダ（`.vault/trash`・`journal`・`tmp`）。
    InitDir(ManagedName, FileId),
    /// init が作った `.vault`。
    VaultDir(FileId),
    /// init が作った保管庫のフォルダとその上のフォルダ（親を基準にした1つの名前。場所は「親の最終のパス＋名前」）。
    TailDir(Name, FileId),
}

/// 消すものの場所の確かめ方。
enum Spot {
    /// `.vault` の下の決まった名前。
    Managed(ManagedName),
    /// `<根>\.vault`。
    VaultDir,
    /// 親の最終のパス＋この名前。
    Child(Name),
}

/// 消すために、`parent` を基準に開く（MANAGED_FILE か MANAGED_DIR。リンクはたどらず、使用中・拒否は試し直す）。
/// ふつうのファイル（フォルダのものはフォルダ）で、最終のパスが決まった場所（`root` は根の最終のパス。TailDir は
/// 親の最終のパス＋名前）にあり、ID を持つものは ID が一致したときだけ返す。ほか（ない・リンク・種類が違う・
/// 場所が違う・ID が違う・開けない）は None（消さない）。
fn open_managed(root: &Path, parent: ParentRef<'_>, item: Removable) -> Option<Managed> {
    let (spot, want, access, id) = match item {
        Removable::IndexJson => (
            Spot::Managed(ManagedName::IndexJson),
            Want::Any,
            MANAGED_FILE,
            None,
        ),
        Removable::EntryJson(t) => (
            Spot::Managed(ManagedName::EntryJson(t)),
            Want::Any,
            MANAGED_FILE,
            None,
        ),
        Removable::TrashDir(t, id) => (
            Spot::Managed(ManagedName::TrashItem(t)),
            Want::Dir,
            MANAGED_DIR,
            Some(id),
        ),
        Removable::InitFile(name, id) => (Spot::Managed(name), Want::Any, MANAGED_FILE, Some(id)),
        Removable::InitDir(name, id) => (Spot::Managed(name), Want::Dir, MANAGED_DIR, Some(id)),
        Removable::VaultDir(id) => (Spot::VaultDir, Want::Dir, MANAGED_DIR, Some(id)),
        Removable::TailDir(name, id) => (Spot::Child(name), Want::Dir, MANAGED_DIR, Some(id)),
    };
    let n = match &spot {
        Spot::Managed(name) => single_name(name.as_str()).ok()?,
        Spot::VaultDir => single_name(RESERVED_NAME).ok()?,
        Spot::Child(name) => name.clone(),
    };
    let handle = match with_retry(|| parent.open_child(&n, want, access, SHARE_RW)).ok()? {
        Opened::File(f) if want == Want::Any => ManagedHandle::File(f),
        Opened::Dir(d) if want == Want::Dir => ManagedHandle::Dir(d),
        _ => return None,
    };
    let managed = Managed(handle);
    let path = os_sys::final_path(managed.file()).ok()?;
    match &spot {
        Spot::Managed(name) => check_managed(root, &path, &name.below_vault()).ok()?,
        Spot::VaultDir => check_managed(root, &path, &[]).ok()?,
        Spot::Child(name) => {
            if path != parent.final_path().ok()?.join(name.to_os_string()) {
                return None;
            }
        }
    }
    if let Some(id) = id
        && os_sys::file_id(managed.file()).ok() != Some(id)
    {
        return None;
    }
    Some(managed)
}

// ---------------------------------------------------------------------------
// 移すもの（2つめの門。TARGET_FILE・TARGET_DIR）
// ---------------------------------------------------------------------------

/// 移す・ゴミ箱へ送るもの（2つめの門で、TARGET_FILE か TARGET_DIR で開いたもの）。DELETE の権利つきなので、
/// ハンドルは guard の外へ貸さない。消す関数には渡せない（`Deletable` を実装しない）。
pub(crate) struct Target {
    handle: TargetHandle,
    info: FileInfo,
    read_only: bool,
}

enum TargetHandle {
    File(File),
    Dir(Dir),
}

impl Target {
    fn from_opened(opened: Opened) -> Result<Self> {
        let handle = match opened {
            Opened::File(f) | Opened::Link(f) => TargetHandle::File(f),
            Opened::Dir(d) => TargetHandle::Dir(d),
        };
        let file = match &handle {
            TargetHandle::File(f) => f,
            TargetHandle::Dir(d) => d.as_file(),
        };
        let meta = file
            .metadata()
            .map_err(|e| VaultError::io("移すものを調べられない", &e))?;
        Ok(Self {
            info: FileInfo::from_metadata(&meta),
            read_only: meta.permissions().readonly(),
            handle,
        })
    }

    fn file(&self) -> &File {
        match &self.handle {
            TargetHandle::File(f) => f,
            TargetHandle::Dir(d) => d.as_file(),
        }
    }

    /// 開いたハンドルの値（種類・大きさ・時刻）。
    pub(crate) fn info(&self) -> FileInfo {
        self.info
    }

    /// 読み取り専用の印が付いているか。
    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }

    /// 最終のパスが、利用者の対象 `rel` の場所にあることを確かめ、実際の名前（`/` 区切り）を返す。
    pub(crate) fn actual(&self, root: &Root, rel: &RelPath) -> Result<String> {
        check_user_target(root, self.file(), rel, At::Target)
    }

    /// 最終のパスが、管理用の決まった場所（`name`）にあることを確かめる（open の片づけの項目）。
    pub(crate) fn check_managed(&self, root: &Root, name: &ManagedName) -> Result<()> {
        check_managed(
            &root.path,
            &read_final(self.file(), At::Target)?,
            &name.below_vault(),
        )
    }

    /// 置き換えない rename（C）。使用中・拒否は試し直す。同じ名前があれば `Collision`（何も動いていない）。
    pub(crate) fn rename_into(
        &self,
        parent: &dyn RenameParent,
        name: &str,
        what: &str,
    ) -> Result<Moved> {
        let n = single_name(name)?;
        let to = parent.rename_to();
        match with_retry(|| to.rename_no_replace(self.file(), &n)) {
            Ok(()) => Ok(Moved::Done),
            Err(e) if is_collision(&e) => Ok(Moved::Collision),
            Err(e) if is_reparse(&e) => Err(crate::ops::outside_link()),
            Err(e) => Err(busy_or_io(what, &e)),
        }
    }

    /// rename のあと、同じハンドルで最終のパスを読み直し、利用者の対象 `rel` の場所かを確かめて、実際の名前を返す。
    pub(crate) fn check_renamed(&self, root: &Root, rel: &RelPath) -> Result<String> {
        check_user_target(root, self.file(), rel, At::Renamed)
    }

    /// 「起きないはず」：rename したものを、置き換えずに `parent` の `name` へ戻す。戻せたら true。
    pub(crate) fn move_back(&self, parent: &dyn RenameParent, name: &str) -> bool {
        test_hook!(Step::BeforeRenameBack);
        let Ok(n) = single_name(name) else {
            return false;
        };
        let to = parent.rename_to();
        with_retry(|| to.rename_no_replace(self.file(), &n)).is_ok()
    }
}

/// 2つめの門で開くものの用途。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetUse {
    /// 利用者のもの（vault_write の上書き・second_gate＝削除・移動・restore の replace）。ファイルは TARGET_FILE_W。
    User,
    /// 管理用のもの（.vault/tmp に残った一時ファイル）。TARGET_FILE。
    Managed,
}

/// 2つめの門で開いた結果。
pub(crate) enum TargetOpen {
    Target(Target),
    /// ない（確かめたあとで消えた）。
    Missing,
    /// 書き込みか削除を拒否された（ACL・読み取り専用の印）。属性だけで開き直して読んだ今の値と、読み取り専用の印
    /// （呼ぶ側が門の値と比べ、同じなら ACCESS_DENIED、違えば CONFLICT）。
    WriteDenied {
        info: FileInfo,
        read_only: bool,
    },
}

/// 2つめの門：親を基準に、移す・ゴミ箱へ送るものを開く。門の種類がフォルダなら TARGET_DIR、利用者のファイルなら
/// TARGET_FILE_W（書き込みの権利も求める）、ほか（リンクそのもの・管理用のファイル）は TARGET_FILE。種類は
/// 求めず、リンクはリンクそのものが開く。使用中・拒否は試し直す。
/// - 試し直したあとも拒否（5）で、TARGET_FILE_W で開いたなら、属性だけで開き直して値を読み、`WriteDenied`
///   （開き直しも開けなければ、今の ACCESS_DENIED）。TARGET_FILE に落として開き直すことはしない。
/// - 使用中（32）は今の BUSY。ないなら `Missing`。
///
/// 開けないときの文は `what`（「消すものを開けない」など、ツールに合わせる）。
pub(crate) fn open_target(
    parent: ParentRef<'_>,
    name: &str,
    kind: EntryKind,
    usage: TargetUse,
    what: &str,
) -> Result<TargetOpen> {
    let n = single_name(name)?;
    let write_check = kind == EntryKind::File && usage == TargetUse::User;
    let access = if kind == EntryKind::Dir {
        TARGET_DIR
    } else if write_check {
        TARGET_FILE_W
    } else {
        TARGET_FILE
    };
    match with_retry(|| parent.open_child(&n, Want::Any, access, SHARE_RW)) {
        Ok(opened) => Target::from_opened(opened).map(TargetOpen::Target),
        Err(e) if is_not_found(&e) => Ok(TargetOpen::Missing),
        Err(e) if is_reparse(&e) => Err(crate::ops::outside_link()),
        Err(e) if write_check && is_access_denied(&e) => {
            // 書き込みか削除を拒否された：属性だけで開き直して、今の値を読む（門の値と比べるため）
            let reopened = match parent.open_child(&n, Want::Any, LEAF_ATTRS, SHARE_ALL) {
                Ok(opened) => opened,
                Err(e) if is_not_found(&e) => return Ok(TargetOpen::Missing),
                Err(_) => return Err(open_failed(what, &e)),
            };
            let file = match &reopened {
                Opened::Dir(d) => d.as_file(),
                Opened::File(f) | Opened::Link(f) => f,
            };
            let meta = file.metadata().map_err(|_| open_failed(what, &e))?;
            Ok(TargetOpen::WriteDenied {
                info: FileInfo::from_metadata(&meta),
                read_only: meta.permissions().readonly(),
            })
        }
        Err(e) => Err(open_failed(what, &e)),
    }
}

/// 親の並びのいちばん下（並びが空なら根）を、rename の移す先の親として渡す。
pub(crate) fn rename_parent<'a>(root: &'a Root, chain: &'a [Pass]) -> &'a dyn RenameParent {
    match chain.last() {
        Some(p) => p,
        None => &root.held,
    }
}

// ---------------------------------------------------------------------------
// ゴミ箱（.vault/trash。trash.rs・trash_ops.rs）
// ---------------------------------------------------------------------------

/// ゴミ箱のフォルダがリンクかフォルダでないときの文（一覧・復元。今までの check_trash_dir と同じ）。
const TRASH_NOT_DIR: &str =
    "ゴミ箱のフォルダ（.vault/trash）が、リンクかフォルダでないものになっている";

/// 作った管理用のフォルダが、閉じてから開き直すまでに変えられた（ID が違う・消えた・リンク）。
fn created_replaced() -> VaultError {
    not_a_vault("管理用のフォルダ（.vault の下）が、作ったあとで差し替えられた")
}

/// ゴミ箱へ移したもの（Trashed）。取り戻す（`take_back`）か、閉じる（`into_id`）。
pub(crate) struct Trashed {
    id: TrashId,
    trash: Held,
    dir: Pass,
    dir_id: Option<FileId>,
    entry: Created<File>,
    data: Target,
}

impl Trashed {
    /// trash_id。
    pub(crate) fn id(&self) -> &TrashId {
        &self.id
    }

    /// 済んだ：持っているハンドルを閉じ、trash_id を返す（entry.json と data は残る）。
    pub(crate) fn into_id(self) -> TrashId {
        self.id
    }
}

/// ゴミ箱へ移す（B・C）。`.vault/trash` を開き（ないなら作る）、`<id>` を作って確かめ、閉じて開き直し（ID を照らす）、
/// その中に entry.json を作って `entry` を書き、`target`（2つめの門のハンドル）を置き換えない rename で `<id>/data` へ
/// 移す（RootDirectory は開き直した `<id>`）。移せなければ、entry.json は作ったハンドルで、`<id>` は remove_trash_dir で
/// 片づけ、`target` には触れない。rename のあとの最終のパスが `.vault\trash\<id>\data` でなければ（起きないはず）、
/// `origin` の `origin_name` へ戻して NOT_A_VAULT。
pub(crate) fn trash_target(
    root: &Root,
    vault: &Pass,
    target: Target,
    origin: &dyn RenameParent,
    origin_name: &str,
    entry: &[u8],
) -> Result<Trashed> {
    test_hook!(Step::Open(Place::Trash));
    let trash = open_place(
        root,
        vault,
        &ManagedName::Trash,
        true,
        MANAGEMENT_DIR_NOT_DIR,
    )?
    .ok_or_else(|| VaultError::new(ErrorCode::Internal, "ゴミ箱のフォルダがない"))?;
    let id = TrashId::new();
    let data = ManagedName::TrashData(id.clone());
    let data_name = single_name(data.as_str())?;
    let (dir, dir_id) = create_trash_item(root, &trash, &id)?;
    let entry = match create_entry(root, &dir, &id, entry) {
        Ok(entry) => entry,
        Err(e) => {
            remove_trash_dir(root, &trash, &id, dir, dir_id);
            return Err(e);
        }
    };
    test_hook!(Step::BeforeRename(Rename::ToTrash));
    if let Err(e) = with_retry(|| dir.rename_to().rename_no_replace(target.file(), &data_name)) {
        let _ = delete(entry);
        remove_trash_dir(root, &trash, &id, dir, dir_id);
        return Err(if is_reparse(&e) {
            managed_not_regular()
        } else {
            busy_or_io("ゴミ箱へ移せない", &e)
        });
    }
    // rename のあと、同じハンドルで最終のパスを読み直す（起きないはず：違えば元の場所へ戻す）
    let checked = read_final(target.file(), At::Trashed)
        .and_then(|p| check_managed(&root.path, &p, &data.below_vault()));
    if let Err(e) = checked {
        if target.move_back(origin, origin_name) {
            let _ = delete(entry);
            remove_trash_dir(root, &trash, &id, dir, dir_id);
            return Err(e);
        }
        return Err(e.with_note(&format!(
            "元の場所へ戻せなかった（ゴミ箱の trash_id: {}）",
            id.as_str()
        )));
    }
    Ok(Trashed {
        id,
        trash,
        dir,
        dir_id,
        entry,
        data: target,
    })
}

/// `.vault/trash/<id>` を作る（B。CREATE_DIR）。最終のパスを確かめ（外なら作ったハンドルで消して NOT_A_VAULT。
/// 消すのは確かめる前のここだけ）、ID を読んで閉じ、PassDir で開き直す（A・最終のパス・ID。違えば NOT_A_VAULT で、
/// 消さない）。
fn create_trash_item(root: &Root, trash: &Held, id: &TrashId) -> Result<(Pass, Option<FileId>)> {
    let name = ManagedName::TrashItem(id.clone());
    let n = single_name(name.as_str())?;
    let created = match trash.parent().create_dir(&n) {
        Ok(c) => c,
        Err(e) if is_reparse(&e) => return Err(managed_not_regular()),
        Err(e) => return Err(open_failed("ゴミ箱のフォルダを作れない", &e)),
    };
    let checked = created
        .final_path()
        .map_err(|e| VaultError::io("作ったフォルダの場所を読めない", &e))
        .map(|p| final_path_hook!(Created, p))
        .and_then(|p| check_managed(&root.path, &p, &name.below_vault()));
    if let Err(e) = checked {
        let _ = delete(created);
        return Err(e);
    }
    let dir_id = created.file_id().ok();
    drop(created);
    test_hook!(Step::AfterCreatedClosed);
    let n = single_name(name.as_str())?;
    let dir = match with_retry(|| trash.parent().open_pass(&n)) {
        Ok(OpenedPass::Dir(p)) => Pass(p),
        Ok(OpenedPass::Link(_)) => return Err(created_replaced()),
        Err(e) if is_not_found(&e) || is_not_a_dir(&e) || is_reparse(&e) => {
            return Err(created_replaced());
        }
        Err(e) => return Err(open_failed("ゴミ箱のフォルダを開けない", &e)),
    };
    check_managed(
        &root.path,
        &read_final(dir.0.as_file(), At::Reopened)?,
        &name.below_vault(),
    )?;
    if let Some(id) = dir_id
        && os_sys::file_id(dir.0.as_file()).ok() != Some(id)
    {
        return Err(created_replaced());
    }
    Ok((dir, dir_id))
}

/// `<id>` の中に entry.json を作り（B。ENTRY_CREATE）、最終のパスを確かめてから中身を書いて sync する。
/// だめなら作ったハンドルで消す。
fn create_entry(root: &Root, dir: &Pass, id: &TrashId, bytes: &[u8]) -> Result<Created<File>> {
    let name = ManagedName::EntryJson(id.clone());
    let n = single_name(name.as_str())?;
    let file = match dir.parent().create_file(&n, ENTRY_CREATE, SHARE_R) {
        Ok(f) => f,
        Err(e) if is_reparse(&e) => return Err(managed_not_regular()),
        Err(e) => return Err(io_or_disk_full("entry.json を作れない", &e)),
    };
    let checked = os_sys::final_path(file.get())
        .map_err(|e| VaultError::io("entry.json の場所を読めない", &e))
        .map(|p| final_path_hook!(Created, p))
        .and_then(|p| check_managed(&root.path, &p, &name.below_vault()));
    if let Err(e) = checked {
        let _ = delete(file);
        return Err(e);
    }
    let mut writer: &File = file.get();
    if let Err(e) = writer.write_all(bytes).and_then(|()| writer.sync_all()) {
        let _ = delete(file);
        return Err(io_or_disk_full("entry.json に書けない", &e));
    }
    Ok(file)
}

/// ゴミ箱の `<id>` を片づける。持っていた `<id>` を値で受け取って閉じ（閉じてから開く順を型で強いる）、
/// ID を照らして開き直し（open_managed）、一致したときだけ消す（中身が残っていれば 145 で残る）。
/// ID がない（読めない FS）・違う・リンク・開けないなら、何もしない。
fn remove_trash_dir(root: &Root, trash: &Held, id: &TrashId, dir: Pass, dir_id: Option<FileId>) {
    drop(dir);
    test_hook!(Step::BeforeTrashCleanup);
    let Some(dir_id) = dir_id else {
        return;
    };
    if let Some(managed) = open_managed(
        &root.path,
        trash.parent(),
        Removable::TrashDir(id.clone(), dir_id),
    ) {
        let _ = delete(managed);
    }
}

/// ゴミ箱へ移したばかりのものを、もとの場所（`parent` の `name`）へ戻す（C。置き換えない）。
/// 戻せたら、entry.json を作ったハンドルで消し、`<id>` を remove_trash_dir で片づけて true。
/// ぶつかった・戻せなければ、ゴミ箱に残して false（復元で戻せる）。
pub(crate) fn take_back(
    root: &Root,
    trashed: Trashed,
    parent: &dyn RenameParent,
    name: &str,
    rel: &RelPath,
) -> bool {
    let Trashed {
        id,
        trash,
        dir,
        dir_id,
        entry,
        data,
    } = trashed;
    let Ok(n) = single_name(name) else {
        return false;
    };
    test_hook!(Step::BeforeRename(Rename::TakeBack));
    let to = parent.rename_to();
    if with_retry(|| to.rename_no_replace(data.file(), &n)).is_err() {
        return false;
    }
    if data.check_renamed(root, rel).is_err() {
        // 起きないはず：ゴミ箱の data へ戻す
        let _ = data.move_back(&dir, DATA_NAME);
        return false;
    }
    drop(data);
    let _ = delete(entry);
    remove_trash_dir(root, &trash, &id, dir, dir_id);
    true
}

/// `.vault/trash` を開く（一覧・復元）。ないなら None。リンク・フォルダでないなら NOT_A_VAULT。
pub(crate) fn open_trash(root: &Root, vault: &Pass) -> Result<Option<Held>> {
    test_hook!(Step::Open(Place::Trash));
    open_place(root, vault, &ManagedName::Trash, false, TRASH_NOT_DIR)
}

/// `.vault/tmp` を開く（open の片づけ）。ないなら None。リンク・フォルダでないなら NOT_A_VAULT。
pub(crate) fn open_tmp_dir(root: &Root, vault: &Pass) -> Result<Option<Held>> {
    test_hook!(Step::Open(Place::Tmp));
    open_place(
        root,
        vault,
        &ManagedName::Tmp,
        false,
        MANAGEMENT_DIR_NOT_DIR,
    )
}

/// 開いたゴミ箱の `<id>`（PassDir で持ち、ID を読んだもの）。
pub(crate) struct TrashItem<'a> {
    trash: &'a Held,
    id: TrashId,
    dir: Pass,
    dir_id: Option<FileId>,
}

/// `<id>` を開いた結果。
pub(crate) enum ItemState<'a> {
    Item(TrashItem<'a>),
    /// ない（消えた）。
    Missing,
    /// フォルダでない・リンク。
    NotDir,
    /// 使用中（一覧では、試し直さずに飛ばす）。
    Busy,
}

/// `.vault/trash` を基準に `<id>` を PassDir で開き、最終のパスを確かめ、ID を読む。`retry` なら使用中・拒否を
/// 試し直す（復元）。一覧では試し直さず、使用中なら `Busy`。
pub(crate) fn open_trash_item<'a>(
    root: &Root,
    trash: &'a Held,
    id: TrashId,
    retry: bool,
) -> Result<ItemState<'a>> {
    let name = ManagedName::TrashItem(id.clone());
    let n = single_name(name.as_str())?;
    let opened = if retry {
        with_retry(|| trash.parent().open_pass(&n))
    } else {
        trash.parent().open_pass(&n)
    };
    let dir = match opened {
        Ok(OpenedPass::Dir(p)) => Pass(p),
        Ok(OpenedPass::Link(_)) => return Ok(ItemState::NotDir),
        Err(e) if is_not_found(&e) => return Ok(ItemState::Missing),
        Err(e) if is_not_a_dir(&e) || is_reparse(&e) => return Ok(ItemState::NotDir),
        Err(e) if !retry && is_busy(&e) => return Ok(ItemState::Busy),
        Err(e) => return Err(open_failed("ゴミ箱の entry を開けない", &e)),
    };
    check_managed(
        &root.path,
        &read_final(dir.0.as_file(), At::Target)?,
        &name.below_vault(),
    )?;
    let dir_id = os_sys::file_id(dir.0.as_file()).ok();
    Ok(ItemState::Item(TrashItem {
        trash,
        id,
        dir,
        dir_id,
    }))
}

/// ゴミ箱の data を開いた結果（復元）。
pub(crate) enum DataState {
    Data(Target),
    /// リンク（ジャンクションを含む）。
    Link,
    Missing,
    /// 開いたあいだに、種類が変わった。
    Changed,
}

impl TrashItem<'_> {
    /// entry.json を読む（LEAF_READ。リンクはたどらない。使用中は試し直す）。ふつうのファイルでない・
    /// 64 KiB を超える・読めないなら None。
    pub(crate) fn read_entry(&self) -> Option<Vec<u8>> {
        test_hook!(Step::Open(Place::Entry));
        let n = single_name(ManagedName::EntryJson(self.id.clone()).as_str()).ok()?;
        let parent = self.dir.parent();
        let file = match with_busy_retry(|| parent.open_child(&n, Want::Any, LEAF_READ, SHARE_ALL))
            .ok()?
        {
            Opened::File(f) => f,
            _ => return None,
        };
        let max = crate::trash::MAX_ENTRY_BYTES as u64;
        if file.metadata().ok()?.len() > max {
            return None;
        }
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut io::Read::take(&file, max + 1), &mut bytes).ok()?;
        (bytes.len() as u64 <= max).then_some(bytes)
    }

    /// data があるか（種類は問わない。LEAF_ATTRS）。
    pub(crate) fn has_data(&self) -> bool {
        let Ok(n) = single_name(DATA_NAME) else {
            return false;
        };
        self.dir
            .parent()
            .open_child(&n, Want::Any, LEAF_ATTRS, SHARE_ALL)
            .is_ok()
    }

    /// data を、戻すために開く（TARGET_FILE。フォルダなら TARGET_DIR で開き直す。使用中・拒否は試し直す）。
    pub(crate) fn open_data(&self) -> Result<DataState> {
        let n = single_name(DATA_NAME)?;
        let parent = self.dir.parent();
        let first = match with_retry(|| parent.open_child(&n, Want::Any, TARGET_FILE, SHARE_RW)) {
            Ok(opened) => opened,
            Err(e) if is_not_found(&e) => return Ok(DataState::Missing),
            Err(e) if is_reparse(&e) => return Ok(DataState::Link),
            Err(e) => return Err(open_failed("ゴミ箱の中身を開けない", &e)),
        };
        let opened = match first {
            Opened::Link(_) => return Ok(DataState::Link),
            Opened::File(f) => Opened::File(f),
            Opened::Dir(d) => {
                drop(d);
                match with_retry(|| parent.open_child(&n, Want::Dir, TARGET_DIR, SHARE_RW)) {
                    Ok(Opened::Dir(d)) => Opened::Dir(d),
                    Ok(_) => return Ok(DataState::Changed),
                    Err(e) if is_not_found(&e) || is_not_a_dir(&e) || is_reparse(&e) => {
                        return Ok(DataState::Changed);
                    }
                    Err(e) => return Err(open_failed("ゴミ箱の中身を開けない", &e)),
                }
            }
        };
        Target::from_opened(opened).map(DataState::Data)
    }

    /// 「起きないはず」：戻す先へ移した data を、置き換えずに `<id>/data` へ戻す。戻せたら true。
    pub(crate) fn put_data_back(&self, data: &Target) -> bool {
        data.move_back(&self.dir, DATA_NAME)
    }

    /// 戻したあとの後始末：entry.json を消し（open_managed）、`<id>` を remove_trash_dir で片づける
    /// （開いたときに読んだ ID と照らす）。
    pub(crate) fn finish(self, root: &Root) {
        let Self {
            trash,
            id,
            dir,
            dir_id,
        } = self;
        if let Some(entry) =
            open_managed(&root.path, dir.parent(), Removable::EntryJson(id.clone()))
        {
            let _ = delete(entry);
        }
        remove_trash_dir(root, trash, &id, dir, dir_id);
    }
}

// ---------------------------------------------------------------------------
// 列挙（E：持っているフォルダのハンドルで列挙し、下のフォルダは親を基準に開く）
// ---------------------------------------------------------------------------

/// 列挙で見つけた1つの項目（名前と、列挙の値）。
pub(crate) struct Listed {
    pub name: OsString,
    /// 列挙の値（ハンドルで開いて読み直していない。種類の決め方は std と同じ）。
    pub info: FileInfo,
}

/// 列挙の値を、種類・大きさ・更新時刻にする（`FileInfo::from_metadata` と同じ決め方。フォルダの大きさは 0）。
fn listed_info(entry: &os_sys::DirEntry) -> FileInfo {
    let kind = match entry.kind() {
        os_sys::EntryKind::Dir => EntryKind::Dir,
        os_sys::EntryKind::File => EntryKind::File,
        os_sys::EntryKind::Link => EntryKind::Link,
    };
    FileInfo {
        kind,
        size: if kind == EntryKind::Dir {
            0
        } else {
            entry.end_of_file()
        },
        mtime_ns: entry.modified_unix_ns(),
    }
}

/// 持っているフォルダ（LIST_DIRECTORY の権利で開いたもの）の中を、そのハンドルで列挙する。
fn list_dir(dir: &mut Dir, what: &str) -> Result<Vec<Listed>> {
    let entries = os_sys::read_dir_by_handle(dir).map_err(|e| open_failed(what, &e))?;
    Ok(entries
        .iter()
        .map(|entry| Listed {
            name: entry.name().clone(),
            info: listed_info(entry),
        })
        .collect())
}

/// 列挙で得た名前を、OS に渡す単一の名前にする。
fn listed_name(name: &OsStr) -> Option<Name> {
    Name::new(name).ok()
}

/// 項目の今の値を読んだ結果。
pub(crate) enum ChildInfo {
    Info(FileInfo),
    /// 読む前に消えた。
    Missing,
    /// 開けない（列挙の値を使う）。
    Unreadable,
}

/// 親を基準に、項目を属性だけで開いて今の値を読む（LEAF_ATTRS。リンクはたどらない。属性だけなので使用中にならない）。
fn child_info(parent: ParentRef<'_>, name: &OsStr) -> ChildInfo {
    child_info_links(parent, name, false).0
}

/// `child_info` と同じく今の値を読み、`read_links` なら、ファイルのときだけ同じハンドルでリンクの数も読む（走査。
///）。返す真偽は「ほかの名前とハードリンクでつながっている（2以上）」。読めなければ
/// つながっていない（1）として扱う（印は読み直しの手がかりにすぎない。外を書き換えない守りは書く道の rename）。
fn child_info_links(parent: ParentRef<'_>, name: &OsStr, read_links: bool) -> (ChildInfo, bool) {
    let Some(n) = listed_name(name) else {
        return (ChildInfo::Unreadable, false);
    };
    match parent.open_child(&n, Want::Any, LEAF_ATTRS, SHARE_ALL) {
        Ok(opened) => {
            let file = match &opened {
                Opened::Dir(d) => d.as_file(),
                Opened::File(f) | Opened::Link(f) => f,
            };
            match file.metadata() {
                Ok(meta) => {
                    let info = FileInfo::from_metadata(&meta);
                    let linked = read_links && is_linked_file(file, &info);
                    (ChildInfo::Info(info), linked)
                }
                Err(_) => (ChildInfo::Unreadable, false),
            }
        }
        Err(e) if is_not_found(&e) => (ChildInfo::Missing, false),
        Err(_) => (ChildInfo::Unreadable, false),
    }
}

/// 開いたハンドルがふつうのファイルで、リンクの数が2以上か（走査の印。読めなければ false）。
fn is_linked_file(file: &File, info: &FileInfo) -> bool {
    info.kind == EntryKind::File && os_sys::link_count(file).is_ok_and(|n| n >= 2)
}

/// 項目を開いて読んだ、今の値と実際の名前。
pub(crate) enum ChildActual {
    /// 今の値と、実際の名前（開いたハンドルの最終のパスから。`/` 区切り）と、ほかの名前とハードリンクで
    /// つながったファイルか（同じハンドルで読む。読めなければ false）。
    Found(FileInfo, String, bool),
    Missing,
    /// 開けない・場所を確かめられない。
    Unreadable,
}

/// 親を基準に、項目を属性だけで開き（LEAF_ATTRS。リンクはたどらない。名前は大文字小文字を問わずに解く）、今の値と、
/// 最終のパスから実際の名前（利用者の対象 `rel` の場所にあれば）と、ファイルならリンクの数が2以上かを読む
/// （走査の調べ直し）。
pub(crate) fn child_actual(
    root: &Root,
    parent: ParentRef<'_>,
    name: &str,
    rel: &RelPath,
) -> ChildActual {
    let Ok(n) = single_name(name) else {
        return ChildActual::Unreadable;
    };
    let opened = match parent.open_child(&n, Want::Any, LEAF_ATTRS, SHARE_ALL) {
        Ok(opened) => opened,
        Err(e) if is_not_found(&e) => return ChildActual::Missing,
        Err(_) => return ChildActual::Unreadable,
    };
    let file = match &opened {
        Opened::Dir(d) => d.as_file(),
        Opened::File(f) | Opened::Link(f) => f,
    };
    let Ok(meta) = file.metadata() else {
        return ChildActual::Unreadable;
    };
    match check_user_target(root, file, rel, At::Target) {
        Ok(actual) => {
            let info = FileInfo::from_metadata(&meta);
            let linked = is_linked_file(file, &info);
            ChildActual::Found(info, actual, linked)
        }
        Err(_) => ChildActual::Unreadable,
    }
}

/// 下のフォルダを開いた結果。
pub(crate) enum ChildDir {
    /// 開いて持った（HOLD_DIR）。
    Dir(Held),
    /// フォルダでない・リンクに替わっていた（今の値。中へは進まない）。
    Other(FileInfo),
    /// 開く前に消えた。
    Missing,
    /// 開けない（使用中・拒否など）。
    Failed(VaultError),
}

/// 親を基準に、下のフォルダを HOLD_DIR で開く（リンクはたどらない）。`retry` なら使用中・拒否を試し直す
/// （vault_list）。走査は試し直さない（使用中・拒否は unknown）。
pub(crate) fn open_child_dir(parent: ParentRef<'_>, name: &OsStr, retry: bool) -> ChildDir {
    let Some(n) = listed_name(name) else {
        return ChildDir::Failed(VaultError::new(
            ErrorCode::Internal,
            "名前を、OS に渡す形にできない",
        ));
    };
    let open = || parent.open_child(&n, Want::Dir, HOLD_DIR, SHARE_RW);
    let opened = if retry { with_retry(open) } else { open() };
    match opened {
        Ok(Opened::Dir(d)) => ChildDir::Dir(Held(d)),
        Ok(Opened::File(f) | Opened::Link(f)) => match f.metadata() {
            Ok(meta) => ChildDir::Other(FileInfo::from_metadata(&meta)),
            Err(e) => ChildDir::Failed(VaultError::io("フォルダを調べられない", &e)),
        },
        Err(e) if is_not_found(&e) => ChildDir::Missing,
        // フォルダでない・リンク：今の値を読んで返す（中へは進まない）
        Err(e) if is_not_a_dir(&e) || is_reparse(&e) => match child_info(parent, name) {
            ChildInfo::Info(info) => ChildDir::Other(info),
            ChildInfo::Missing => ChildDir::Missing,
            ChildInfo::Unreadable => ChildDir::Failed(VaultError::io("フォルダを調べられない", &e)),
        },
        Err(e) => ChildDir::Failed(open_failed("フォルダを開けない", &e)),
    }
}

impl Held {
    /// 持っているフォルダの中を、このハンドルで列挙する（E）。読めないときの文は `what`。
    pub(crate) fn entries(&mut self, what: &str) -> Result<Vec<Listed>> {
        list_dir(&mut self.0, what)
    }

    /// 項目を属性だけで開き、今の値を読む（LEAF_ATTRS）。
    pub(crate) fn child_info(&self, name: &OsStr) -> ChildInfo {
        child_info(self.parent(), name)
    }

    /// 走査：項目を属性だけで開き、今の値と、ファイルならほかの名前とハードリンクでつながっているか（同じハンドルで
    /// リンクの数を読む。読めなければ false）を読む。
    pub(crate) fn scan_child_info(&self, name: &OsStr) -> (ChildInfo, bool) {
        child_info_links(self.parent(), name, true)
    }

    /// このハンドルを基準に、下のフォルダを HOLD_DIR で開く。
    pub(crate) fn open_child_dir(&self, name: &OsStr, retry: bool) -> ChildDir {
        open_child_dir(self.parent(), name, retry)
    }
}

impl Root {
    /// 根のハンドルを、列挙するフォルダとして渡す（根の最終のパスは開いたときに確かめた）。
    pub(crate) fn into_held(self) -> Held {
        self.held
    }
}

/// 一覧するフォルダを開いた結果。
pub(crate) enum ListDir {
    Dir(Held),
    Missing,
    NotDir,
    Link,
}

/// 親を基準に、一覧するフォルダを HOLD_DIR で開く（リンクはたどらない。使用中・拒否は試し直す）。
pub(crate) fn open_list_dir(parent: ParentRef<'_>, name: &str) -> Result<ListDir> {
    let n = single_name(name)?;
    match with_retry(|| parent.open_child(&n, Want::Dir, HOLD_DIR, SHARE_RW)) {
        Ok(Opened::Dir(d)) => Ok(ListDir::Dir(Held(d))),
        Ok(Opened::Link(_)) => Ok(ListDir::Link),
        Ok(Opened::File(_)) => Ok(ListDir::NotDir),
        Err(e) if is_not_found(&e) => Ok(ListDir::Missing),
        Err(e) if is_not_a_dir(&e) => Ok(ListDir::NotDir),
        Err(e) if is_reparse(&e) => Ok(ListDir::Link),
        Err(e) => Err(open_failed("フォルダを開けない", &e)),
    }
}

/// 開いた Held が、利用者の対象 `rel` の場所にあることを確かめ、実際の名前を返す（一覧するフォルダ）。
pub(crate) fn check_held_dir(root: &Root, dir: &Held, rel: &RelPath, at: At) -> Result<String> {
    check_user_target(root, dir.0.as_file(), rel, at)
}

/// 管理用のファイル（.vault の下）の値を、属性だけで開いて読む（LEAF_ATTRS。リンクはたどらない）。ないなら None。
/// 走査の印（Stamp）で使う。
pub(crate) fn managed_info(parent: ParentRef<'_>, name: &ManagedName) -> Result<Option<FileInfo>> {
    let n = single_name(name.as_str())?;
    match child_info(parent, n.to_os_string().as_os_str()) {
        ChildInfo::Info(info) => Ok(Some(info)),
        ChildInfo::Missing => Ok(None),
        ChildInfo::Unreadable => Err(VaultError::new(
            ErrorCode::IoError,
            "管理用のファイルを調べられない",
        )),
    }
}

/// 木を歩いて見つけた1つの項目。
pub(crate) struct Found<'a> {
    parent: ParentRef<'a>,
    /// 歩き始めたフォルダからの、親の相対パス（`/` 区切り。名前は lossy）。直下なら ""。
    pub parent_rel: &'a str,
    pub name: &'a OsStr,
    /// 歩き始めたフォルダの直下が 1。
    pub depth: usize,
    /// 列挙の値。
    pub listed: FileInfo,
}

impl Found<'_> {
    /// 項目を属性だけで開き直して、今の値を読む（LEAF_ATTRS）。開けなければ列挙の値。消えていれば None。
    pub(crate) fn current(&self) -> Option<FileInfo> {
        match child_info(self.parent, self.name) {
            ChildInfo::Info(info) => Some(info),
            ChildInfo::Missing => None,
            ChildInfo::Unreadable => Some(self.listed),
        }
    }
}

/// 歩いている途中の1段（持っているフォルダと、まだ見ていない項目）。
struct Frame {
    /// 持っているフォルダ（None なら歩き始めたフォルダ）。
    dir: Option<Held>,
    rel: String,
    depth: usize,
    entries: std::vec::IntoIter<Listed>,
}

/// 木を歩く（E。深さ優先で、再帰でなく繰り返しで）。`start` の中を列挙し、`visit` が true を返した項目のうち、
/// 列挙の値がフォルダのものだけ、親を基準に HOLD_DIR で開いて中へ進む（その下を歩き終えたら閉じる。祖先は
/// 開いたまま）。下のフォルダがリンクなどに替わっていた・消えていたら、中へは進まない。開けない・列挙できない
/// ときは、`strict` なら Err、そうでなければそのフォルダを飛ばす。
fn walk_from(
    start: &mut Dir,
    strict: bool,
    visit: &mut dyn FnMut(&Found<'_>) -> Result<bool>,
) -> Result<()> {
    let first = list_dir(start, "フォルダを読めない")?;
    let start: &Dir = start;
    let mut stack = vec![Frame {
        dir: None,
        rel: String::new(),
        depth: 1,
        entries: first.into_iter(),
    }];
    while let Some(frame) = stack.last_mut() {
        let Some(item) = frame.entries.next() else {
            stack.pop();
            continue;
        };
        let parent = match &frame.dir {
            Some(held) => held.parent(),
            // 歩き始めのフォルダ（TARGET_DIR）は、項目を開くことだけができる親として包む
            None => ParentRef(ParentKind::Target(start)),
        };
        let found = Found {
            parent,
            parent_rel: &frame.rel,
            name: &item.name,
            depth: frame.depth,
            listed: item.info,
        };
        if !visit(&found)? || item.info.kind != EntryKind::Dir {
            continue;
        }
        let name = item.name.to_string_lossy();
        let rel = if frame.rel.is_empty() {
            name.into_owned()
        } else {
            format!("{}/{name}", frame.rel)
        };
        let depth = frame.depth + 1;
        test_hook!(Step::BeforeChildOpen);
        let mut child = match open_child_dir(parent, &item.name, strict) {
            ChildDir::Dir(held) => held,
            ChildDir::Other(_) | ChildDir::Missing => continue,
            ChildDir::Failed(e) if strict => return Err(e),
            ChildDir::Failed(_) => continue,
        };
        let entries = match child.entries("フォルダを読めない") {
            Ok(entries) => entries,
            Err(e) if strict => return Err(e),
            Err(_) => continue,
        };
        stack.push(Frame {
            dir: Some(child),
            rel,
            depth,
            entries: entries.into_iter(),
        });
    }
    Ok(())
}

impl Target {
    /// 移すものがフォルダなら、そのハンドル（TARGET_DIR）で下の木を歩く（`walk_from`）。ファイル・リンクなら何もしない。
    pub(crate) fn walk(
        &mut self,
        strict: bool,
        visit: &mut dyn FnMut(&Found<'_>) -> Result<bool>,
    ) -> Result<()> {
        match &mut self.handle {
            TargetHandle::Dir(d) => walk_from(d, strict, visit),
            TargetHandle::File(_) => Ok(()),
        }
    }

    /// 移すものがフォルダで、中に何かあるか（そのハンドルで列挙する。判定と rename が同じものになる）。
    pub(crate) fn has_entries(&mut self) -> Result<bool> {
        match &mut self.handle {
            TargetHandle::Dir(d) => Ok(!list_dir(d, "フォルダを読めない")?.is_empty()),
            TargetHandle::File(_) => Ok(false),
        }
    }

    /// 開いたハンドルで、今の値を読み直す（rename のあとの索引の値）。
    pub(crate) fn current_info(&self) -> Result<FileInfo> {
        self.file()
            .metadata()
            .map(|meta| FileInfo::from_metadata(&meta))
            .map_err(|e| VaultError::io("移したものを調べられない", &e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 親にする開き方（HOLD_DIR・根）は、どれも os-sys の「親にできる」（データの権利があり、DELETE を共有しない）。
    /// フォルダの開き方の定数には DELETE の共有がなく、データの権利（LIST か TRAVERSE）がある。
    #[test]
    fn parent_constants_can_be_parents() {
        for (access, share) in [
            (HOLD_DIR, SHARE_RW),
            (CREATE_DIR, SHARE_RW),
            (TARGET_DIR, SHARE_RW),
            (MANAGED_DIR, SHARE_RW),
        ] {
            assert!(!share.contains(Share::DELETE));
            assert!(access.contains(Access::LIST_DIRECTORY) || access.contains(Access::TRAVERSE));
        }
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        match os_sys::open_path(crate_dir, Links::DontFollow, HOLD_DIR, SHARE_RW).unwrap() {
            Opened::Dir(d) => assert!(d.can_be_parent(), "HOLD_DIR（根）が親にできない"),
            other => panic!("{other:?}"),
        }
    }

    /// `impl RenameParent for` は Held と Pass の2つだけ（DELETE の権利つきのものを、rename の移す先の親にしない）。
    #[test]
    fn only_held_and_pass_are_rename_parents() {
        let needle = ["impl RenameParent", " for "].concat();
        let source = include_str!("windows.rs");
        let impls: Vec<&str> = source
            .lines()
            .filter(|line| line.starts_with(&needle))
            .collect();
        assert_eq!(
            impls,
            [
                "impl RenameParent for Held {",
                "impl RenameParent for Pass {"
            ]
        );
    }

    /// 本番のコードの部分（この tests の前まで）の行。
    fn production_lines() -> Vec<&'static str> {
        include_str!("windows.rs")
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .unwrap()
            .lines()
            .collect()
    }

    /// `start` で始まる行から、次の `}` だけの行（いちばん外の閉じ）までの行の番号の範囲。
    fn block(lines: &[&str], start: &str) -> std::ops::Range<usize> {
        let from = lines
            .iter()
            .position(|line| line.starts_with(start))
            .unwrap_or_else(|| panic!("{start} がない"));
        let to = lines[from..]
            .iter()
            .position(|line| *line == "}")
            .map(|n| from + n)
            .unwrap_or_else(|| panic!("{start} の閉じがない"));
        from..to
    }

    /// `needle` を含む行の番号。
    fn lines_with(lines: &[&str], needle: &str) -> Vec<usize> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains(needle))
            .map(|(n, _)| n)
            .collect()
    }

    /// TP1：Target の種類の親（歩き始めの、DELETE の権利を持つフォルダ）では、通り抜けて開く・作ることが
    /// InvalidInput（OS を呼ぶ前に断る）。項目を開くことはできる。このクレートのフォルダを読むだけで開く（何も
    /// 作らない）。
    #[test]
    fn tp1_a_target_parent_can_only_open_children() {
        // init の祖先（InitBase）は、作る・通り抜ける親にはなるが、ファイルは作れない
        {
            let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            let Opened::Dir(dir) =
                os_sys::open_path(crate_dir, Links::Follow, INIT_BASE, SHARE_RW).unwrap()
            else {
                panic!("このクレートのフォルダがフォルダでない");
            };
            let base = ParentRef(ParentKind::InitBase(&dir));
            let name = single_name("tp1-init-base-must-not-be-created").unwrap();
            assert_eq!(
                base.create_file(&name, Access::READ_ATTRIBUTES, SHARE_RW)
                    .map(drop)
                    .err()
                    .map(|e| e.kind()),
                Some(io::ErrorKind::InvalidInput)
            );
            assert!(!crate_dir.join("tp1-init-base-must-not-be-created").exists());
        }
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Opened::Dir(dir) =
            os_sys::open_path(crate_dir, Links::Follow, HOLD_DIR, SHARE_RW).unwrap()
        else {
            panic!("このクレートのフォルダがフォルダでない");
        };
        let parent = ParentRef(ParentKind::Target(&dir));
        let name = single_name("tp1-must-not-be-created").unwrap();
        let kind = |r: io::Result<()>| r.err().map(|e| e.kind());
        assert_eq!(
            kind(parent.create_dir(&name).map(drop)),
            Some(io::ErrorKind::InvalidInput)
        );
        assert_eq!(
            kind(
                parent
                    .create_file(&name, Access::READ_ATTRIBUTES, SHARE_RW)
                    .map(drop)
            ),
            Some(io::ErrorKind::InvalidInput)
        );
        assert_eq!(
            kind(parent.open_pass(&name).map(drop)),
            Some(io::ErrorKind::InvalidInput)
        );
        assert!(!crate_dir.join("tp1-must-not-be-created").exists());
        let src = single_name("src").unwrap();
        assert!(matches!(
            parent.open_child(&src, Want::Dir, HOLD_DIR, SHARE_RW),
            Ok(Opened::Dir(_))
        ));
    }

    /// TP2（ソースの検査）：
    /// - `fn rename_no_replace(`・`fn rename_replace(` は、`impl RenameTo` の中だけ。
    /// - `RenameTo(RenameKind::`（RenameTo を作る所）は、ちょうど2つで、`impl RenameParent for Held`・
    ///   `impl RenameParent for Pass` のブロックの中に1つずつ。
    /// - `ParentRef(ParentKind::Target(`（Target の種類の親を作る所）は、`walk_from` の中だけ。`ParentKind::Target(`
    ///   は、ほかに `impl ParentRef` の中の振り分けにだけ出る（テストは除く）。
    /// - `Held(` を作る所（`Held(` を含む行から、`ParentKind::Held(`・`RenameKind::Held(`・`struct Held(`・列挙の
    ///   項目の宣言・説明を除いたもの。変数の名前は問わない）は、今の6つで、どれも HOLD_DIR で開く関数の中
    ///   （`impl RenameParent for` が `Held`・`Pass` だけなのは、only_held_and_pass_are_rename_parents）
    #[test]
    fn tp2_rename_targets_and_held_dirs_come_only_from_their_places() {
        let lines = production_lines();
        let rename_to = block(&lines, "impl RenameTo<'_> {");
        for needle in ["fn rename_no_replace(", "fn rename_replace("] {
            let found = lines_with(&lines, needle);
            assert!(!found.is_empty(), "{needle} がない");
            for n in found {
                assert!(
                    rename_to.contains(&n),
                    "{needle} が impl RenameTo の外（{} 行）",
                    n + 1
                );
            }
        }

        let made = lines_with(&lines, "RenameTo(RenameKind::");
        assert_eq!(made.len(), 2, "RenameTo を作る所が2つでない：{made:?}");
        for (n, parent) in made.iter().zip(["Held", "Pass"]) {
            let inside = block(&lines, &format!("impl RenameParent for {parent} {{"));
            assert!(
                inside.contains(n),
                "RenameTo を impl RenameParent for {parent} の外で作る（{} 行）",
                n + 1
            );
        }

        let walk_from = block(&lines, "fn walk_from(");
        let parent_ref = block(&lines, "impl ParentRef<'_> {");
        let made = lines_with(&lines, "ParentRef(ParentKind::Target(");
        assert_eq!(made.len(), 1, "Target の種類の親を作る所が1つでない");
        assert!(made.iter().all(|n| walk_from.contains(n)));
        for n in lines_with(&lines, "ParentKind::Target(") {
            assert!(
                walk_from.contains(&n) || parent_ref.contains(&n),
                "ParentKind::Target( が walk_from・impl ParentRef の外（{} 行）",
                n + 1
            );
        }

        // `ParentRef(ParentKind::InitBase(`（init の祖先を、作る・通り抜ける親にする所）は、impl InitScope の中だけで
        // ちょうど2つ（prepare_root と undo）。`ParentKind::InitBase(` は、ほかに impl ParentRef の中の振り分けにだけ
        // 出る
        let init_scope = block(&lines, "impl InitScope {");
        let made = lines_with(&lines, "ParentRef(ParentKind::InitBase(");
        assert_eq!(
            made.len(),
            2,
            "InitBase の種類の親を作る所が2つでない：{made:?}"
        );
        for n in &made {
            assert!(
                init_scope.contains(n),
                "InitBase の種類の親を impl InitScope の外で作る（{} 行）",
                n + 1
            );
        }
        for n in lines_with(&lines, "ParentKind::InitBase(") {
            assert!(
                init_scope.contains(&n) || parent_ref.contains(&n),
                "ParentKind::InitBase( が impl InitScope・impl ParentRef の外（{} 行）",
                n + 1
            );
        }

        // RenameKind:: を含む行は、impl RenameTo の振り分けと、impl RenameParent for Held・Pass の中だけ
        let rename_blocks = [
            block(&lines, "impl RenameTo<'_> {"),
            block(&lines, "impl RenameParent for Held {"),
            block(&lines, "impl RenameParent for Pass {"),
        ];
        for n in lines_with(&lines, "RenameKind::") {
            assert!(
                rename_blocks.iter().any(|b| b.contains(&n)),
                "RenameKind:: が impl RenameTo・impl RenameParent の外（{} 行）",
                n + 1
            );
        }

        let mut held = 0;
        for (n, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // 作る形：`Held(…)`（ParentKind・RenameKind の振り分け・型の宣言を除く）、関数として渡す `(Held)`
            // （列挙の項目の宣言 `Dir(Held),` を除く）、`Held {`（impl・型の宣言を除く）。
            let call = line.contains("Held(")
                && !line.contains("ParentKind::Held(")
                && !line.contains("RenameKind::Held(")
                && !line.contains("struct Held(")
                && !trimmed.starts_with("Held(&");
            let as_fn = line.contains("(Held)") && !trimmed.starts_with("Dir(Held)");
            let literal = line.contains("Held {")
                && !line.contains("-> Held {")
                && !trimmed.starts_with("impl ")
                && !trimmed.contains("struct ")
                && !trimmed.contains("enum ");
            if !(call || as_fn || literal) {
                continue;
            }
            held += 1;
            let start = (0..n)
                .rev()
                .find(|&m| {
                    let t = lines[m].trim_start();
                    t.starts_with("fn ") || t.starts_with("pub(crate) fn ")
                })
                .unwrap_or_else(|| panic!("{} 行の Held( の関数がない", n + 1));
            assert!(
                lines[start..=n].iter().any(|l| l.contains("HOLD_DIR")),
                "HOLD_DIR で開かない関数で Held を作る（{} 行）",
                n + 1
            );
        }
        assert_eq!(
            held, 7,
            "Held を作る所の数が変わった（Root::open・open_place の2つ・open_child_dir・open_list_dir・InitScope の begin と prepare_root。Root::resolve はなくした）"
        );
    }

    /// `src` の下の本番のコード（各ファイルの tests の前まで）の、(ファイル, 行) の並び。
    fn source_files() -> Vec<(String, Vec<String>)> {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path)
                        .unwrap()
                        .replace("\r\n", "\n");
                    let production = text.split("\n#[cfg(test)]\nmod tests").next().unwrap();
                    let name = path
                        .strip_prefix(&src)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    files.push((name, production.lines().map(str::to_owned).collect()));
                }
            }
        }
        files
    }

    /// `lines` の中の、`signature` で始まる関数（`indent` の深さの `}` で閉じる）の行の範囲。
    fn fn_block(lines: &[String], signature: &str, indent: &str) -> std::ops::Range<usize> {
        let from = lines
            .iter()
            .position(|line| line.starts_with(signature))
            .unwrap_or_else(|| panic!("{signature} がない"));
        let close = format!("{indent}}}");
        let to = lines[from..]
            .iter()
            .position(|line| *line == close)
            .map(|n| from + n)
            .unwrap_or_else(|| panic!("{signature} の閉じがない"));
        from..to
    }

    /// TP2 の続き：`Root::open(` を呼ぶのは vault.rs の `fn open_checked`（open の本体）と `fn op_root` の中だけ。
    /// `Root::resolve` は InitScope に移してなくした（どこからも呼ばない）。guard の Root の縛りなので、この
    /// テストは guard に置く（src の全部のファイルを読む）。
    #[test]
    fn tp2_root_open_is_called_only_from_its_places() {
        let files = source_files();
        let (_, vault) = files
            .iter()
            .find(|(name, _)| name == "vault.rs")
            .expect("vault.rs がない");
        let open = fn_block(vault, "    pub(crate) fn open_checked(", "    ");
        let op_root = fn_block(vault, "    pub(crate) fn op_root(", "    ");
        let mut opens = 0;
        for (name, lines) in &files {
            for (n, line) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                assert!(
                    !line.contains("Root::resolve("),
                    "Root::resolve( を {name} の {} 行で呼ぶ",
                    n + 1
                );
                if line.contains("Root::open(") {
                    opens += 1;
                    assert!(
                        name == "vault.rs" && (open.contains(&n) || op_root.contains(&n)),
                        "Root::open( を {name} の {} 行で呼ぶ",
                        n + 1
                    );
                }
            }
        }
        assert_eq!(opens, 2, "呼ぶ所の数が変わった");
    }

    /// TP2 と同じ形：`TargetUse::Managed` を使うのは、trash_ops.rs の `move_orphan_tmp`
    /// （.vault/tmp に残った一時ファイル）の中だけ。
    #[test]
    fn managed_target_use_is_only_for_orphan_tmp() {
        let files = source_files();
        let (_, trash_ops) = files
            .iter()
            .find(|(name, _)| name == "trash_ops.rs")
            .expect("trash_ops.rs がない");
        let orphan = fn_block(trash_ops, "pub(crate) fn move_orphan_tmp(", "");
        let mut uses = 0;
        for (name, lines) in &files {
            for (n, line) in lines.iter().enumerate() {
                if line.contains("TargetUse::Managed") && !line.trim_start().starts_with("//") {
                    uses += 1;
                    assert!(
                        name == "trash_ops.rs" && orphan.contains(&n),
                        "TargetUse::Managed を {name} の {} 行で使う",
                        n + 1
                    );
                }
            }
        }
        assert_eq!(uses, 1);
    }

    /// init の片づけで消すもの（`Removable::TailDir(`・`InitDir(`・`InitFile(`・`VaultDir(`）を作るのは、InitScope の
    /// 中だけ（ほかは open_managed の振り分け）。
    #[test]
    fn init_removables_are_made_only_in_the_init_scope() {
        let lines = production_lines();
        let scope = block(&lines, "impl InitScope {");
        let open_managed = block(&lines, "fn open_managed(");
        let mut made = 0;
        for needle in [
            "Removable::TailDir(",
            "Removable::InitDir(",
            "Removable::InitFile(",
            "Removable::VaultDir(",
        ] {
            for n in lines_with(&lines, needle) {
                if open_managed.contains(&n) {
                    continue;
                }
                made += 1;
                assert!(
                    scope.contains(&n),
                    "{needle} を InitScope の外で作る（{} 行）",
                    n + 1
                );
            }
        }
        // どれも undo の中：InitFile の2つ（履歴のファイルと、ほかのファイル）・InitDir・VaultDir・TailDir
        assert_eq!(made, 5, "InitScope の中で作る所の数が変わった");
    }

    /// ファイルの ID を受け付けないことを示すエラーは、87・50・1 だけ。ほかのエラー（拒否・
    /// 使用中・ハンドルが正しくない）では、開くのをやめる。
    #[test]
    fn only_87_50_1_mean_the_id_is_unsupported() {
        for code in [87, 50, 1] {
            assert!(
                is_id_unsupported(&io::Error::from_raw_os_error(code)),
                "{code}"
            );
        }
        for code in [5, 32, 6, 2, 0] {
            assert!(
                !is_id_unsupported(&io::Error::from_raw_os_error(code)),
                "{code}"
            );
        }
        assert!(!is_id_unsupported(&io::Error::other("ほかのエラー")));
    }

    /// 消せるもの（`Deletable` と、その封じた trait）は、作ったもの（`Created<File>`・`Created<Dir>`）と
    /// 決まった名前の管理用のもの（`Managed`）の3つだけ。`os_sys::delete_by_handle` を呼ぶのは `delete` の中だけ。
    #[test]
    fn only_created_and_managed_items_are_deletable() {
        // テストのコード（この関数）の文字列を数えないように、本番のコードの部分だけを見る
        let source = include_str!("windows.rs")
            .split(
                "
#[cfg(test)]
mod tests",
            )
            .next()
            .unwrap();
        let lines = |needle: &str| -> Vec<String> {
            source
                .lines()
                .filter(|line| line.starts_with(needle))
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(
            lines(&["impl Deletable", " for "].concat()),
            [
                "impl Deletable for Created<File> {}",
                "impl Deletable for Created<Dir> {}",
                "impl Deletable for Managed {}"
            ]
        );
        assert_eq!(
            lines(&["impl deletable::Sealed", " for "].concat()),
            [
                "impl deletable::Sealed for Created<File> {",
                "impl deletable::Sealed for Created<Dir> {",
                "impl deletable::Sealed for Managed {"
            ]
        );
        let calls: Vec<&str> = source
            .lines()
            .filter(|line| line.contains(&["delete", "_by_handle("].concat()))
            .map(str::trim)
            .collect();
        assert_eq!(
            calls,
            [
                "deletable::Doomed::Handle(file) => os_sys::delete_by_handle(file),",
                "deletable::Doomed::CreatedDir(dir) => dir.delete_by_handle(),"
            ]
        );
    }

    #[test]
    fn final_paths_are_compared_by_components() {
        let root = Path::new(r"\\?\D:\v");
        let rel = RelPath::parse("a/B.txt").unwrap();
        assert_eq!(
            user_rel(root, Path::new(r"\\?\D:\v\A\b.TXT"), &rel).unwrap(),
            "A/b.TXT"
        );
        for bad in [
            r"\\?\D:\v2\a\b.txt",
            r"\\?\D:\v",
            r"\\?\D:\v\a",
            r"\\?\D:\v\a\b\c",
            r"\\?\D:\V\a\b.txt",
        ] {
            assert_eq!(
                user_rel(root, Path::new(bad), &rel).unwrap_err().code(),
                ErrorCode::OutsideVault,
                "{bad}"
            );
        }
        let reserved = RelPath::parse("a/b").unwrap();
        assert_eq!(
            user_rel(root, Path::new(r"\\?\D:\v\.VAULT\b"), &reserved)
                .unwrap_err()
                .code(),
            ErrorCode::ReservedPath
        );
        assert!(
            check_managed(
                root,
                Path::new(r"\\?\D:\v\.Vault\TMP\1-2.tmp"),
                &["tmp", "1-2.tmp"]
            )
            .is_ok()
        );
        assert!(
            check_managed(
                root,
                Path::new(r"\\?\D:\v\.vault\tmp2\1-2.tmp"),
                &["tmp", "1-2.tmp"]
            )
            .is_err()
        );
        assert!(check_managed(root, Path::new(r"\\?\D:\w\.vault\tmp"), &["tmp"]).is_err());
    }
}
