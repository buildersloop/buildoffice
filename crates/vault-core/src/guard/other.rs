//! Windows 以外の部品：Phase 0 のパスの処理のまま（CI（B13）が動いたら、ハンドルの形にする）。
//!
//! Windows の部品（`windows.rs`）と同じ形の型と関数を出すが、確かめは今までどおりパスで行う
//! （確かめてから操作するまでの隙は残る。守る範囲は Phase 0 と同じ）。空き容量は確かめない。

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;

use super::{ManagedName, Replaceable, TrashId};
use crate::error::{ErrorCode, Result, VaultError};
use crate::fsutil::{
    BUSY_ATTEMPTS, BUSY_INTERVAL, EntryKind, FileInfo, access_denied, io_or_disk_full,
    is_access_denied, is_busy,
};
use crate::relpath::{RESERVED_NAME, RelPath};
use crate::root::is_not_found;
use crate::trash::{DATA_NAME, ENTRY_FILE, MAX_ENTRY_BYTES};

fn not_a_vault(message: &str) -> VaultError {
    VaultError::new(ErrorCode::NotAVault, message)
}

/// 管理用のフォルダ（tmp・journal）がリンクかフォルダでないときの文（今までの ensure_management_dir と同じ）。
pub(crate) const MANAGEMENT_DIR_NOT_DIR: &str =
    "管理用のフォルダ（.vault の下）が、リンクかフォルダでないものになっている";

fn managed_not_regular() -> VaultError {
    not_a_vault("管理用のファイル・フォルダ（.vault の下）が、リンクかふつうでないものになっている")
}

fn created_changed(rel: &str) -> VaultError {
    VaultError::new(
        ErrorCode::Conflict,
        format!("作ったフォルダが、確かめたあとで、ほかのアプリに変えられた：{rel}。もう一度試す"),
    )
}

/// 親として渡すもの（パス）。
#[derive(Clone, Copy)]
pub(crate) struct ParentRef<'a>(&'a Path);

pub(crate) trait ParentDir {
    fn parent(&self) -> ParentRef<'_>;
}

pub(crate) trait RenameParent: ParentDir {}

#[derive(Debug)]
pub(crate) struct Held(PathBuf);

#[derive(Debug)]
pub(crate) struct Pass(PathBuf);

impl ParentDir for Held {
    fn parent(&self) -> ParentRef<'_> {
        ParentRef(&self.0)
    }
}

impl ParentDir for Pass {
    fn parent(&self) -> ParentRef<'_> {
        ParentRef(&self.0)
    }
}

impl RenameParent for Held {}
impl RenameParent for Pass {}

pub(crate) fn parent_of<'a>(root: &'a Root, chain: &'a [Pass]) -> ParentRef<'a> {
    match chain.last() {
        Some(p) => p.parent(),
        None => root.held.parent(),
    }
}

#[derive(Debug)]
pub(crate) struct Root {
    held: Held,
    path: PathBuf,
}

pub(crate) fn resolve_root_path(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

/// 根の ID（Windows 以外は持たない。パスだけで確かめる。強化は CI のあと）。
pub(crate) type RootId = ();

pub(crate) fn resolve_root(path: &Path) -> io::Result<(PathBuf, RootId)> {
    Ok((fs::canonicalize(path)?, ()))
}

/// 既知のフォルダは OS に問い合わせない（Windows 以外。強化は CI のあと）。
pub(crate) fn os_places() -> crate::root::OsPlaces {
    crate::root::OsPlaces::NotAsked
}

impl Root {
    pub(crate) fn open(expected: &Path, _id: &RootId) -> Result<Self> {
        test_hook!(Step::BeforeRoot);
        match fs::symlink_metadata(expected) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(Self {
                held: Held(expected.to_path_buf()),
                path: expected.to_path_buf(),
            }),
            Ok(_) => Err(not_a_vault(
                "保管庫のフォルダが、開いたときと違う。保管庫を開き直す",
            )),
            Err(e) if is_not_found(&e) => Err(not_a_vault(
                "保管庫のフォルダが、開いたときと違う。保管庫を開き直す",
            )),
            Err(e) => Err(VaultError::io("保管庫のフォルダを調べられない", &e)),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn open_vault_dir(&self) -> Result<Pass> {
        let dir = self.path.join(RESERVED_NAME);
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(Pass(dir)),
            Ok(_) => Err(managed_not_regular()),
            Err(e) if is_not_found(&e) => Err(not_a_vault("保管庫でない（.vault がない）")),
            Err(e) => Err(VaultError::io("管理用のフォルダを調べられない", &e)),
        }
    }
}

/// どの確かめか（Windows 以外では、読む対象と、すでにあるフォルダだけ）。
#[derive(Clone, Copy)]
pub(crate) enum At {
    Target,
}

/// 実際の名前（Phase 0 と同じく canonicalize から。読めなければ渡された名前）。
fn actual_rel(root: &Path, abs: &Path, rel: &RelPath) -> String {
    fs::canonicalize(abs)
        .ok()
        .and_then(|canon| {
            canon.strip_prefix(root).ok().map(|r| {
                r.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            })
        })
        .unwrap_or_else(|| rel.to_string())
}

pub(crate) fn check_user_target(
    root: &Root,
    _file: &File,
    rel: &RelPath,
    _at: At,
) -> Result<String> {
    Ok(actual_rel(&root.path, &rel_path(&root.path, rel), rel))
}

pub(crate) fn check_user_dir(root: &Root, dir: &Pass, rel: &RelPath, _at: At) -> Result<String> {
    Ok(actual_rel(&root.path, &dir.0, rel))
}

fn rel_path(root: &Path, rel: &RelPath) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in rel.components() {
        path.push(part);
    }
    path
}

pub(crate) enum DirStep {
    Dir(Pass),
    Missing,
    NotDir,
    Link,
}

pub(crate) fn step_dir(parent: ParentRef<'_>, name: &str) -> Result<DirStep> {
    let path = parent.0.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(DirStep::Link),
        Ok(meta) if meta.is_dir() => Ok(DirStep::Dir(Pass(path))),
        Ok(_) => Ok(DirStep::NotDir),
        Err(e) if is_not_found(&e) => Ok(DirStep::Missing),
        Err(e) => Err(VaultError::io("フォルダを調べられない", &e)),
    }
}

/// `step_dir` の、試し直さない形（ここでは試し直さないので同じ）。
pub(crate) fn step_dir_once(parent: ParentRef<'_>, name: &str) -> Result<DirStep> {
    step_dir(parent, name)
}

pub(crate) enum LeafRead {
    File(File),
    Dir,
    Link,
    Missing,
}

pub(crate) fn open_leaf_read(parent: ParentRef<'_>, name: &str) -> Result<LeafRead> {
    let path = parent.0.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => return Ok(LeafRead::Link),
        Ok(meta) if meta.is_dir() => return Ok(LeafRead::Dir),
        Ok(_) => {}
        Err(e) if is_not_found(&e) => return Ok(LeafRead::Missing),
        Err(e) => return Err(VaultError::io("ファイルを調べられない", &e)),
    }
    match File::open(&path) {
        Ok(f) => Ok(LeafRead::File(f)),
        Err(e) if is_not_found(&e) => Ok(LeafRead::Missing),
        Err(e) => Err(VaultError::io("ファイルを開けない", &e)),
    }
}

pub(crate) enum LeafState {
    File(FileInfo),
    Dir,
    Link,
    Missing,
}

pub(crate) fn read_leaf_attrs(
    _root: &Root,
    parent: ParentRef<'_>,
    name: &str,
    _rel: &RelPath,
) -> Result<LeafState> {
    let path = parent.0.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(LeafState::Link),
        Ok(meta) if meta.is_dir() => Ok(LeafState::Dir),
        Ok(meta) => Ok(LeafState::File(FileInfo::from_metadata(&meta))),
        Err(e) if is_not_found(&e) => Ok(LeafState::Missing),
        Err(e) => Err(VaultError::io("ファイルを調べられない", &e)),
    }
}

pub(crate) struct NewDir {
    info: FileInfo,
    actual: String,
}

pub(crate) fn create_user_dir(
    root: &Root,
    parent: ParentRef<'_>,
    name: &str,
    rel: &RelPath,
) -> Result<NewDir> {
    let path = parent.0.join(name);
    if let Err(e) = fs::create_dir(&path) {
        return Err(if e.kind() == io::ErrorKind::AlreadyExists {
            VaultError::new(ErrorCode::AlreadyExists, format!("すでにある：{rel}"))
        } else {
            io_or_disk_full("フォルダを作れない", &e)
        });
    }
    let info = fs::symlink_metadata(&path)
        .map(|meta| FileInfo::from_metadata(&meta))
        .map_err(|e| VaultError::io("作ったフォルダを調べられない", &e))?;
    Ok(NewDir {
        info,
        actual: actual_rel(&root.path, &path, rel),
    })
}

impl NewDir {
    pub(crate) fn actual(&self) -> &str {
        &self.actual
    }

    pub(crate) fn info(&self) -> FileInfo {
        self.info
    }

    pub(crate) fn close(self) {}

    pub(crate) fn reopen(self, _root: &Root, parent: ParentRef<'_>, rel: &RelPath) -> Result<Pass> {
        test_hook!(Step::AfterCreatedClosed);
        let name = rel.components().last().unwrap_or_default();
        match step_dir(parent, name)? {
            DirStep::Dir(p) => Ok(p),
            DirStep::Link => Err(crate::ops::outside_link()),
            DirStep::Missing | DirStep::NotDir => Err(created_changed(&rel.to_string())),
        }
    }
}

/// Windows 以外では、空き容量を確かめない。
pub(crate) fn check_free_space(_root: &Root, _len: usize) -> Result<()> {
    Ok(())
}

pub(crate) fn open_place(
    _root: &Root,
    vault: &Pass,
    name: &ManagedName,
    create: bool,
    not_regular: &str,
) -> Result<Option<Held>> {
    let dir = vault.0.join(name.as_str());
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(Some(Held(dir))),
        Ok(_) => Err(not_a_vault(not_regular)),
        Err(e) if is_not_found(&e) => {
            if !create {
                return Ok(None);
            }
            match fs::create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(VaultError::io("管理用のフォルダを作れない", &e)),
            }
            test_hook!(Step::AfterCreatedClosed);
            match fs::symlink_metadata(&dir) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(Some(Held(dir))),
                _ => Err(not_a_vault(not_regular)),
            }
        }
        Err(e) => Err(VaultError::io("管理用のフォルダを調べられない", &e)),
    }
}

pub(crate) enum ManagedFile {
    File(File),
    NotFile,
    Missing,
}

#[derive(Clone, Copy)]
pub(crate) enum ManagedUse {
    Read,
    Attrs,
}

pub(crate) fn open_managed_file(
    parent: ParentRef<'_>,
    name: &ManagedName,
    _usage: ManagedUse,
    what: &str,
) -> Result<ManagedFile> {
    let path = parent.0.join(name.as_str());
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(ManagedFile::NotFile),
        Err(e) if is_not_found(&e) => return Ok(ManagedFile::Missing),
        Err(e) => return Err(VaultError::io(what, &e)),
    }
    match File::open(&path) {
        Ok(f) => Ok(ManagedFile::File(f)),
        Err(e) if is_not_found(&e) => Ok(ManagedFile::Missing),
        Err(e) => Err(VaultError::io(what, &e)),
    }
}

pub(crate) fn open_lock(_root: &Root, vault: &Pass) -> Result<File> {
    test_hook!(Step::Open(Place::Lock));
    let path = vault.0.join(ManagedName::Lock.as_str());
    for _ in 0..2 {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {
                return OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(|e| VaultError::io("ロックのファイルを開けない", &e));
            }
            Ok(_) => {
                return Err(not_a_vault(
                    "ロックのファイル（.vault/lock）が、ふつうのファイルでない",
                ));
            }
            Err(e) if is_not_found(&e) => {
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(file) => return Ok(file),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(VaultError::io("ロックのファイルを作れない", &e)),
                }
            }
            Err(e) => return Err(VaultError::io("ロックのファイルを調べられない", &e)),
        }
    }
    Err(VaultError::new(
        ErrorCode::IoError,
        "ロックのファイルを開けない（作るのと同時に、ほかの処理が置き換えた）",
    ))
}

pub(crate) fn open_journal_file(dir: &Held, name: &ManagedName) -> Result<File> {
    let path = dir.0.join(name.as_str());
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(not_a_vault("履歴のファイルが、ふつうのファイルでない")),
        Err(e) => return Err(VaultError::io("履歴のファイルを調べられない", &e)),
    }
    OpenOptions::new()
        .read(true)
        .append(true)
        .open(&path)
        .map_err(|e| VaultError::io("履歴のファイルを開けない", &e))
}

pub(crate) fn create_journal_file(
    _root: &Root,
    dir: &Held,
    name: &ManagedName,
) -> Result<(File, Made)> {
    let path = dir.0.join(name.as_str());
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| io_or_disk_full("履歴のファイルを作れない", &e))?;
    Ok((file, Made(path, false)))
}

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

pub(crate) fn remove_index(_root: &Root, vault: &Pass) {
    test_hook!(Step::Open(Place::Index));
    let _ = fs::remove_file(vault.0.join(ManagedName::IndexJson.as_str()));
}

pub(crate) struct TmpFile {
    path: PathBuf,
    file: File,
}

pub(crate) enum Placing {
    Placed(Placed),
    Collision(TmpFile),
    Failed(TmpFile, VaultError),
}

/// 書く先へ置いた一時ファイル（消す道は、.vault/tmp へ戻せたあとだけ）。
pub(crate) struct Placed {
    tmp: TmpFile,
    at: PathBuf,
}

pub(crate) enum Moved {
    Done,
    Collision,
}

pub(crate) fn create_tmp(root: &Root, vault: &Pass) -> Result<TmpFile> {
    test_hook!(Step::Open(Place::Tmp));
    let dir = open_place(root, vault, &ManagedName::Tmp, true, MANAGEMENT_DIR_NOT_DIR)?
        .ok_or_else(|| VaultError::new(ErrorCode::Internal, "一時ファイルのフォルダがない"))?;
    let path = dir.0.join(ManagedName::new_tmp_file().as_str());
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| io_or_disk_full("一時ファイルを作れない", &e))?;
    test_hook!(Step::AfterTmpCreated);
    Ok(TmpFile { path, file })
}

impl TmpFile {
    pub(crate) fn write_all(mut self, bytes: &[u8]) -> Result<Self> {
        if let Err(e) = self
            .file
            .write_all(bytes)
            .and_then(|()| self.file.sync_all())
        {
            self.discard();
            return Err(io_or_disk_full("一時ファイルに書けない", &e));
        }
        Ok(self)
    }

    pub(crate) fn discard(self) {
        drop(self.file);
        let _ = fs::remove_file(self.path);
    }

    /// 置き換えない rename（パスで、ないことを確かめてから。隙は残る）。
    pub(crate) fn place(self, parent: &dyn RenameParent, name: &str) -> Placing {
        let to = parent.parent().0.join(name);
        if fs::symlink_metadata(&to).is_ok() {
            return Placing::Collision(self);
        }
        match rename_retry(&self.path, &to, "書いたファイルを置き換えられない") {
            Ok(()) => Placing::Placed(Placed { tmp: self, at: to }),
            Err(e) => Placing::Failed(self, e),
        }
    }

    pub(crate) fn replace_managed(
        self,
        _root: &Root,
        vault: &Pass,
        name: Replaceable,
    ) -> Result<Written> {
        let name = name.name();
        test_hook!(Step::BeforeRename(Rename::ReplaceManaged));
        drop(self.file);
        rename_retry(
            &self.path,
            &vault.0.join(name.as_str()),
            "書いたファイルを置き換えられない",
        )
        .inspect_err(|_| {
            let _ = fs::remove_file(&self.path);
        })?;
        Ok(Written)
    }
}

impl Placed {
    pub(crate) fn check_renamed(&self, root: &Root, rel: &RelPath) -> Result<String> {
        Ok(actual_rel(&root.path, &self.at, rel))
    }

    pub(crate) fn move_back(self, e: VaultError, _rel: &RelPath) -> VaultError {
        test_hook!(Step::BeforeRenameBack);
        if fs::symlink_metadata(&self.tmp.path).is_err()
            && fs::rename(&self.at, &self.tmp.path).is_ok()
        {
            self.tmp.discard();
        }
        e
    }
}

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

// ---------------------------------------------------------------------------
// 移すもの・ゴミ箱（Phase 0 のパスの処理のまま。確かめてから操作するまでの隙は残る）
// ---------------------------------------------------------------------------

/// rename する。使用中・拒否のときは、100ms おきに3回まで試し、だめなら `BUSY` か `ACCESS_DENIED`。
/// 置き換えないかは、呼ぶ側が直前に確かめる（隙は残る）。
fn rename_retry(from: &Path, to: &Path, what: &str) -> Result<()> {
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if is_busy(&e) || is_access_denied(&e) => {
                if attempt >= BUSY_ATTEMPTS {
                    return Err(if is_access_denied(&e) {
                        access_denied(
                            "アクセスが拒否された（読み取り専用の印・権限・ほかのアプリが開いている など）",
                        )
                    } else {
                        VaultError::new(
                            ErrorCode::Busy,
                            "ファイルが使用中で、置き換えられない（ほかのアプリが開いている など）",
                        )
                    });
                }
                attempt += 1;
                thread::sleep(BUSY_INTERVAL);
            }
            Err(e) => return Err(io_or_disk_full(what, &e)),
        }
    }
}

/// 移す・ゴミ箱へ送るもの（パス）。
pub(crate) struct Target {
    path: RefCell<PathBuf>,
    info: FileInfo,
    read_only: bool,
}

impl Target {
    pub(crate) fn info(&self) -> FileInfo {
        self.info
    }

    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) fn actual(&self, root: &Root, rel: &RelPath) -> Result<String> {
        Ok(actual_rel(&root.path, &self.path.borrow(), rel))
    }

    pub(crate) fn check_managed(&self, _root: &Root, _name: &ManagedName) -> Result<()> {
        Ok(())
    }

    /// 置き換えない rename（パスで、ないことを確かめてから。隙は残る）。
    pub(crate) fn rename_into(
        &self,
        parent: &dyn RenameParent,
        name: &str,
        what: &str,
    ) -> Result<Moved> {
        let to = parent.parent().0.join(name);
        if fs::symlink_metadata(&to).is_ok() {
            return Ok(Moved::Collision);
        }
        let from = self.path.borrow().clone();
        rename_retry(&from, &to, what)?;
        *self.path.borrow_mut() = to;
        Ok(Moved::Done)
    }

    pub(crate) fn check_renamed(&self, root: &Root, rel: &RelPath) -> Result<String> {
        self.actual(root, rel)
    }

    pub(crate) fn move_back(&self, parent: &dyn RenameParent, name: &str) -> bool {
        test_hook!(Step::BeforeRenameBack);
        matches!(
            self.rename_into(parent, name, "元の場所へ戻せない"),
            Ok(Moved::Done)
        )
    }
}

/// 2つめの門で開くものの用途（Windows と同じ形。Windows 以外は ACL を確かめない＝WriteDenied は返さない）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetUse {
    User,
    Managed,
}

/// 2つめの門で開いた結果（Windows と同じ形）。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum TargetOpen {
    Target(Target),
    Missing,
    WriteDenied { info: FileInfo, read_only: bool },
}

pub(crate) fn open_target(
    parent: ParentRef<'_>,
    name: &str,
    _kind: EntryKind,
    _usage: TargetUse,
    _what: &str,
) -> Result<TargetOpen> {
    let path = parent.0.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) => Ok(TargetOpen::Target(Target {
            path: RefCell::new(path),
            info: FileInfo::from_metadata(&meta),
            read_only: meta.permissions().readonly(),
        })),
        Err(e) if is_not_found(&e) => Ok(TargetOpen::Missing),
        Err(e) => Err(VaultError::io("移すものを調べられない", &e)),
    }
}

pub(crate) fn rename_parent<'a>(root: &'a Root, chain: &'a [Pass]) -> &'a dyn RenameParent {
    match chain.last() {
        Some(p) => p,
        None => &root.held,
    }
}

const TRASH_NOT_DIR: &str =
    "ゴミ箱のフォルダ（.vault/trash）が、リンクかフォルダでないものになっている";

pub(crate) struct Trashed {
    id: TrashId,
    dir: PathBuf,
    data: Target,
}

impl Trashed {
    pub(crate) fn id(&self) -> &TrashId {
        &self.id
    }

    pub(crate) fn into_id(self) -> TrashId {
        self.id
    }
}

/// 後始末：entry.json と `<id>`（空のときだけ）を、パスで消す。
fn remove_entry_dir(dir: &Path) {
    let _ = fs::remove_file(dir.join(ENTRY_FILE));
    test_hook!(Step::BeforeTrashCleanup);
    let _ = fs::remove_dir(dir);
}

pub(crate) fn trash_target(
    root: &Root,
    vault: &Pass,
    target: Target,
    _origin: &dyn RenameParent,
    _origin_name: &str,
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
    let dir = trash.0.join(id.as_str());
    fs::create_dir(&dir).map_err(|e| VaultError::io("ゴミ箱のフォルダを作れない", &e))?;
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(ENTRY_FILE))
        .and_then(|mut f| f.write_all(entry).and_then(|()| f.sync_all()));
    if let Err(e) = written {
        remove_entry_dir(&dir);
        return Err(io_or_disk_full("entry.json を作れない", &e));
    }
    test_hook!(Step::BeforeRename(Rename::ToTrash));
    let failure = match target.rename_into(&Held(dir.clone()), DATA_NAME, "ゴミ箱へ移せない")
    {
        Ok(Moved::Done) => None,
        Ok(Moved::Collision) => Some(VaultError::new(
            ErrorCode::IoError,
            "ゴミ箱へ移せない（ゴミ箱の中に、同じ名前のものができていた）",
        )),
        Err(e) => Some(e),
    };
    if let Some(e) = failure {
        remove_entry_dir(&dir);
        return Err(e);
    }
    Ok(Trashed {
        id,
        dir,
        data: target,
    })
}

pub(crate) fn take_back(
    _root: &Root,
    trashed: Trashed,
    parent: &dyn RenameParent,
    name: &str,
    _rel: &RelPath,
) -> bool {
    test_hook!(Step::BeforeRename(Rename::TakeBack));
    if !matches!(
        trashed.data.rename_into(parent, name, "ゴミ箱から戻せない"),
        Ok(Moved::Done)
    ) {
        return false;
    }
    remove_entry_dir(&trashed.dir);
    true
}

pub(crate) fn open_trash(root: &Root, vault: &Pass) -> Result<Option<Held>> {
    test_hook!(Step::Open(Place::Trash));
    open_place(root, vault, &ManagedName::Trash, false, TRASH_NOT_DIR)
}

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

pub(crate) struct TrashItem<'a> {
    _trash: &'a Held,
    dir: PathBuf,
}

pub(crate) enum ItemState<'a> {
    Item(TrashItem<'a>),
    Missing,
    NotDir,
    #[allow(dead_code)]
    Busy,
}

pub(crate) fn open_trash_item<'a>(
    _root: &Root,
    trash: &'a Held,
    id: TrashId,
    _retry: bool,
) -> Result<ItemState<'a>> {
    let dir = trash.0.join(id.as_str());
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            Ok(ItemState::Item(TrashItem { _trash: trash, dir }))
        }
        Ok(_) => Ok(ItemState::NotDir),
        Err(e) if is_not_found(&e) => Ok(ItemState::Missing),
        Err(e) => Err(VaultError::io("ゴミ箱の entry を調べられない", &e)),
    }
}

pub(crate) enum DataState {
    Data(Target),
    Link,
    Missing,
    #[allow(dead_code)]
    Changed,
}

impl TrashItem<'_> {
    pub(crate) fn read_entry(&self) -> Option<Vec<u8>> {
        test_hook!(Step::Open(Place::Entry));
        let path = self.dir.join(ENTRY_FILE);
        let max = MAX_ENTRY_BYTES as u64;
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > max {
            return None;
        }
        let mut bytes = Vec::new();
        io::Read::read_to_end(
            &mut io::Read::take(File::open(&path).ok()?, max + 1),
            &mut bytes,
        )
        .ok()?;
        (bytes.len() as u64 <= max).then_some(bytes)
    }

    pub(crate) fn has_data(&self) -> bool {
        fs::symlink_metadata(self.dir.join(DATA_NAME)).is_ok()
    }

    pub(crate) fn open_data(&self) -> Result<DataState> {
        let path = self.dir.join(DATA_NAME);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => Ok(DataState::Link),
            Ok(meta) => Ok(DataState::Data(Target {
                path: RefCell::new(path),
                info: FileInfo::from_metadata(&meta),
                read_only: meta.permissions().readonly(),
            })),
            Err(e) if is_not_found(&e) => Ok(DataState::Missing),
            Err(e) => Err(VaultError::io("ゴミ箱の中身を調べられない", &e)),
        }
    }

    pub(crate) fn put_data_back(&self, data: &Target) -> bool {
        data.move_back(&Held(self.dir.clone()), DATA_NAME)
    }

    pub(crate) fn finish(self, _root: &Root) {
        let _ = fs::remove_file(self.dir.join(ENTRY_FILE));
        let _ = fs::remove_dir(&self.dir);
    }
}

// ---------------------------------------------------------------------------
// 列挙（Phase 0 のパスの処理のまま）
// ---------------------------------------------------------------------------

pub(crate) struct Listed {
    pub name: OsString,
    pub info: FileInfo,
}

/// パスの read_dir で列挙する（値は、列挙の値＝リンクをたどらないもの）。
fn list_path(dir: &Path, what: &str) -> Result<Vec<Listed>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| VaultError::io(what, &e))? {
        let entry = entry.map_err(|e| VaultError::io(what, &e))?;
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        out.push(Listed {
            name: entry.file_name(),
            info: FileInfo::from_metadata(&meta),
        });
    }
    Ok(out)
}

pub(crate) enum ChildInfo {
    Info(FileInfo),
    Missing,
    Unreadable,
}

fn path_info(path: &Path) -> ChildInfo {
    match fs::symlink_metadata(path) {
        Ok(meta) => ChildInfo::Info(FileInfo::from_metadata(&meta)),
        Err(e) if is_not_found(&e) => ChildInfo::Missing,
        Err(_) => ChildInfo::Unreadable,
    }
}

pub(crate) enum ChildDir {
    Dir(Held),
    Other(FileInfo),
    Missing,
    Failed(VaultError),
}

fn path_child_dir(path: PathBuf) -> ChildDir {
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => ChildDir::Dir(Held(path)),
        Ok(meta) => ChildDir::Other(FileInfo::from_metadata(&meta)),
        Err(e) if is_not_found(&e) => ChildDir::Missing,
        Err(e) => ChildDir::Failed(VaultError::io("フォルダを調べられない", &e)),
    }
}

pub(crate) enum ChildActual {
    Found(FileInfo, String),
    Missing,
    Unreadable,
}

pub(crate) fn child_actual(
    root: &Root,
    parent: ParentRef<'_>,
    name: &str,
    rel: &RelPath,
) -> ChildActual {
    let path = parent.0.join(name);
    // リンクはたどらない：値はリンクそのもの。実際の名前は、親（フォルダ。step_dir でリンクでないことを確かめた）
    // の実際の場所に、葉の名前をそのまま付ける（葉を canonicalize すると、リンクの先の名前になる）
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if is_not_found(&e) => return ChildActual::Missing,
        Err(_) => return ChildActual::Unreadable,
    };
    let parent_rel = match rel.to_string().rsplit_once('/') {
        Some((parent_rel, _)) => {
            let Ok(parent_rel) = RelPath::parse(parent_rel) else {
                return ChildActual::Unreadable;
            };
            let Ok(canon) = fs::canonicalize(parent.0) else {
                return ChildActual::Unreadable;
            };
            let Ok(inside) = canon.strip_prefix(&root.path) else {
                return ChildActual::Unreadable;
            };
            let parts: Vec<String> = inside
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if parts.len() != parent_rel.components().count() {
                return ChildActual::Unreadable;
            }
            Some(parts.join("/"))
        }
        None => None,
    };
    let actual = match parent_rel {
        Some(parent_rel) => format!("{parent_rel}/{name}"),
        None => name.to_owned(),
    };
    ChildActual::Found(FileInfo::from_metadata(&meta), actual)
}

pub(crate) fn open_child_dir(parent: ParentRef<'_>, name: &OsStr, _retry: bool) -> ChildDir {
    path_child_dir(parent.0.join(name))
}

impl Held {
    pub(crate) fn entries(&mut self, what: &str) -> Result<Vec<Listed>> {
        list_path(&self.0, what)
    }

    pub(crate) fn child_info(&self, name: &OsStr) -> ChildInfo {
        path_info(&self.0.join(name))
    }

    pub(crate) fn open_child_dir(&self, name: &OsStr, _retry: bool) -> ChildDir {
        path_child_dir(self.0.join(name))
    }
}

impl Root {
    pub(crate) fn into_held(self) -> Held {
        self.held
    }
}

pub(crate) enum ListDir {
    Dir(Held),
    Missing,
    NotDir,
    Link,
}

pub(crate) fn open_list_dir(parent: ParentRef<'_>, name: &str) -> Result<ListDir> {
    let path = parent.0.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(ListDir::Link),
        Ok(meta) if meta.is_dir() => Ok(ListDir::Dir(Held(path))),
        Ok(_) => Ok(ListDir::NotDir),
        Err(e) if is_not_found(&e) => Ok(ListDir::Missing),
        Err(e) => Err(VaultError::io("フォルダを調べられない", &e)),
    }
}

pub(crate) fn check_held_dir(root: &Root, dir: &Held, rel: &RelPath, _at: At) -> Result<String> {
    Ok(actual_rel(&root.path, &dir.0, rel))
}

pub(crate) fn managed_info(parent: ParentRef<'_>, name: &ManagedName) -> Result<Option<FileInfo>> {
    match path_info(&parent.0.join(name.as_str())) {
        ChildInfo::Info(info) => Ok(Some(info)),
        ChildInfo::Missing => Ok(None),
        ChildInfo::Unreadable => Err(VaultError::new(
            ErrorCode::IoError,
            "管理用のファイルを調べられない",
        )),
    }
}

pub(crate) struct Found<'a> {
    parent: &'a Path,
    pub parent_rel: &'a str,
    pub name: &'a OsStr,
    pub depth: usize,
    pub listed: FileInfo,
}

impl Found<'_> {
    pub(crate) fn current(&self) -> Option<FileInfo> {
        match path_info(&self.parent.join(self.name)) {
            ChildInfo::Info(info) => Some(info),
            ChildInfo::Missing => None,
            ChildInfo::Unreadable => Some(self.listed),
        }
    }
}

/// パスで木を歩く（再帰でなく繰り返しで。リンクはたどらない）。
fn walk_path(
    start: &Path,
    strict: bool,
    visit: &mut dyn FnMut(&Found<'_>) -> Result<bool>,
) -> Result<()> {
    let mut stack = vec![(start.to_path_buf(), String::new(), 1usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let entries = match list_path(&dir, "フォルダを読めない") {
            Ok(entries) => entries,
            Err(e) if strict => return Err(e),
            Err(_) => continue,
        };
        for item in entries {
            let found = Found {
                parent: &dir,
                parent_rel: &rel,
                name: &item.name,
                depth,
                listed: item.info,
            };
            if !visit(&found)? || item.info.kind != EntryKind::Dir {
                continue;
            }
            let name = item.name.to_string_lossy();
            let child_rel = if rel.is_empty() {
                name.into_owned()
            } else {
                format!("{rel}/{name}")
            };
            test_hook!(Step::BeforeChildOpen);
            stack.push((dir.join(&item.name), child_rel, depth + 1));
        }
    }
    Ok(())
}

impl Target {
    pub(crate) fn walk(
        &mut self,
        strict: bool,
        visit: &mut dyn FnMut(&Found<'_>) -> Result<bool>,
    ) -> Result<()> {
        if self.info.kind != EntryKind::Dir {
            return Ok(());
        }
        let path = self.path.borrow().clone();
        walk_path(&path, strict, visit)
    }

    pub(crate) fn has_entries(&mut self) -> Result<bool> {
        if self.info.kind != EntryKind::Dir {
            return Ok(false);
        }
        let path = self.path.borrow().clone();
        Ok(!list_path(&path, "フォルダを読めない")?.is_empty())
    }

    pub(crate) fn current_info(&self) -> Result<FileInfo> {
        fs::symlink_metadata(&*self.path.borrow())
            .map(|meta| FileInfo::from_metadata(&meta))
            .map_err(|e| VaultError::io("移したものを調べられない", &e))
    }
}

// ---------------------------------------------------------------------------
// init の作る処理と片づけ（Windows 以外。Phase 0 のパスの処理を、vault.rs からそのまま移した）
// ---------------------------------------------------------------------------

/// init が作ったものの記録（Windows 以外：パスと、フォルダか）。
#[derive(Debug)]
pub(crate) struct Made(PathBuf, bool);

/// 管理用のファイルを置き換えた結果（Windows 以外は ID を持たない）。
#[must_use]
pub(crate) struct Written;

/// init の作る処理と片づけ（Windows 以外。パスで作り、記録したものをパスで消す。フォルダは空のときだけ消える）。
pub(crate) struct InitScope {
    target: PathBuf,
    root: Option<Root>,
    vault: Option<Pass>,
    made: RefCell<Vec<(PathBuf, bool)>>,
}

impl InitScope {
    pub(crate) fn begin(path: &Path) -> Result<Self> {
        Ok(Self {
            target: canonical_with_missing_tail(path)?,
            root: None,
            vault: None,
            made: RefCell::new(Vec::new()),
        })
    }

    pub(crate) fn target(&self) -> &Path {
        &self.target
    }

    pub(crate) fn prepare_root(&mut self) -> Result<()> {
        match fs::symlink_metadata(&self.target) {
            Ok(meta) if meta.is_dir() => {
                if fs::symlink_metadata(self.target.join(RESERVED_NAME)).is_ok() {
                    return Err(crate::vault::already_a_vault());
                }
                let mut entries = fs::read_dir(&self.target)
                    .map_err(|e| VaultError::io("フォルダの中を調べられない", &e))?;
                if entries.next().is_some() {
                    return Err(VaultError::new(
                        ErrorCode::InvalidRoot,
                        "空でないフォルダは保管庫にできない（隠しファイルも数える）",
                    ));
                }
            }
            Ok(_) => {
                return Err(VaultError::new(
                    ErrorCode::InvalidRoot,
                    "フォルダでないものは保管庫にできない",
                ));
            }
            Err(e) if is_not_found(&e) => {
                create_missing_dirs(&self.target, &mut self.made.borrow_mut())?;
            }
            Err(e) => return Err(VaultError::io("保管庫のフォルダを調べられない", &e)),
        }
        let path = fs::canonicalize(&self.target)
            .map_err(|e| VaultError::io("保管庫のフォルダを調べられない", &e))?;
        self.root = Some(Root {
            held: Held(path.clone()),
            path,
        });
        Ok(())
    }

    pub(crate) fn root(&self) -> Result<&Root> {
        self.root
            .as_ref()
            .ok_or_else(|| VaultError::new(ErrorCode::Internal, "init の順番が正しくない"))
    }

    pub(crate) fn vault(&self) -> Result<&Pass> {
        self.vault
            .as_ref()
            .ok_or_else(|| VaultError::new(ErrorCode::Internal, "init の順番が正しくない"))
    }

    pub(crate) fn create_management(&mut self) -> Result<()> {
        let root = self.root()?;
        let vault_dir = root.path.join(RESERVED_NAME);
        // create_dir_all でなく create_dir にして、同じフォルダを同時に作ろうとした2つめを止める
        fs::create_dir(&vault_dir).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                crate::vault::already_a_vault()
            } else {
                VaultError::io("管理用のフォルダを作れない", &e)
            }
        })?;
        self.made.borrow_mut().push((vault_dir.clone(), true));
        for sub in [
            crate::trash::TRASH_DIR,
            crate::journal::JOURNAL_DIR,
            crate::fsutil::TMP_DIR,
        ] {
            let dir = vault_dir.join(sub);
            fs::create_dir(&dir).map_err(|e| VaultError::io("管理用のフォルダを作れない", &e))?;
            self.made.borrow_mut().push((dir, true));
        }
        let lock = vault_dir.join(crate::vault::LOCK_FILE);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(|e| VaultError::io("ロックのファイルを作れない", &e))?;
        self.made.borrow_mut().push((lock, false));
        let vault = root.open_vault_dir()?;
        self.vault = Some(vault);
        Ok(())
    }

    pub(crate) fn record(&self, made: Made) {
        self.made.borrow_mut().push((made.0, made.1));
    }

    pub(crate) fn record_written(&self, name: ManagedName, _written: &Written) {
        if let Some(root) = &self.root {
            let mut path = root.path.join(RESERVED_NAME);
            for part in name.below_vault() {
                path.push(part);
            }
            self.made.borrow_mut().push((path, false));
        }
    }

    pub(crate) fn undo(self, e: VaultError) -> VaultError {
        test_hook!(Step::BeforeInitUndo);
        let Self {
            root, vault, made, ..
        } = self;
        drop((root, vault));
        undo_items(made.into_inner());
        e
    }
}

/// まだないフォルダを、根に近いほうから1つずつ作る（作ったものを `made` に書く）。
fn create_missing_dirs(target: &Path, made: &mut Vec<(PathBuf, bool)>) -> Result<()> {
    let mut missing = Vec::new();
    let mut cur = Some(target);
    while let Some(dir) = cur {
        match fs::symlink_metadata(dir) {
            Ok(_) => break,
            Err(e) if is_not_found(&e) => {
                missing.push(dir.to_path_buf());
                cur = dir.parent();
            }
            Err(e) => return Err(VaultError::io("保管庫のフォルダを調べられない", &e)),
        }
    }
    for dir in missing.iter().rev() {
        fs::create_dir(dir).map_err(|e| VaultError::io("保管庫のフォルダを作れない", &e))?;
        made.push((dir.clone(), true));
    }
    Ok(())
}

/// 記録したもの（`(パス, フォルダか)` を作った順）を、逆の順で片づける。フォルダは空のときだけ消える。
pub(crate) fn undo_items(items: Vec<(PathBuf, bool)>) {
    for (path, is_dir) in items.into_iter().rev() {
        // 片づけられなくても、もとのエラーを返す
        let _ = if is_dir {
            if path.file_name().is_some_and(|n| n == RESERVED_NAME) {
                test_hook!(Step::BeforeInitRemove(Removing::VaultDir));
            } else if !path.components().any(|c| c.as_os_str() == RESERVED_NAME) {
                test_hook!(Step::BeforeInitRemove(Removing::CreatedDir));
            }
            fs::remove_dir(&path)
        } else {
            fs::remove_file(&path)
        };
    }
}

/// まだないパスは、いちばん深い存在する祖先を canonicalize し、残りの要素を足した形にする。
fn canonical_with_missing_tail(path: &Path) -> Result<PathBuf> {
    for ancestor in path.ancestors() {
        match fs::canonicalize(ancestor) {
            Ok(mut out) => {
                let rest = path
                    .strip_prefix(ancestor)
                    .map_err(|_| VaultError::new(ErrorCode::Internal, "パスを分けられない"))?;
                for c in rest.components() {
                    match c {
                        std::path::Component::Normal(name) => out.push(name),
                        _ => {
                            return Err(VaultError::new(
                                ErrorCode::InvalidRoot,
                                "パスの形が正しくない",
                            ));
                        }
                    }
                }
                return Ok(out);
            }
            Err(e) if is_not_found(&e) => {}
            Err(e) => return Err(VaultError::io("保管庫のフォルダの場所を調べられない", &e)),
        }
    }
    Err(VaultError::new(
        ErrorCode::InvalidRoot,
        "保管庫のフォルダの場所が見つからない（ドライブがない など）",
    ))
}
