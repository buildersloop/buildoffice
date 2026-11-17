//! 権利・共有・種類の求め方と、開いたものの型。

use std::fs::{File, OpenOptions};
use std::io;
use std::ops::BitOr;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::{FileId, ffi, file_id, final_path, info};

/// 開くときに求める権利。下の定数からだけ組める（`|` か [`Access::with`]）。
///
/// MAXIMUM_ALLOWED・GENERIC_*・WRITE_DAC・WRITE_OWNER・ACCESS_SYSTEM_SECURITY は作れない。
/// SYNCHRONIZE と FILE_READ_ATTRIBUTES は、os-sys がいつも足す（同期の I/O と、ハンドルで種類を確かめるため）。
///
/// 外から値を作れないこと：
///
/// ```compile_fail,E0599
/// let _ = os_sys::Access::MAXIMUM_ALLOWED;
/// ```
///
/// ```compile_fail,E0423
/// let _ = os_sys::Access(0x0200_0000);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access(u32);

impl Access {
    /// DELETE（消す・名前を変える）。
    pub const DELETE: Self = Self(ffi::DELETE);
    /// FILE_READ_DATA（ファイルの中身を読む）。
    pub const READ_DATA: Self = Self(ffi::FILE_READ_DATA);
    /// FILE_LIST_DIRECTORY（フォルダの中を列挙する。READ_DATA と同じ値）。
    pub const LIST_DIRECTORY: Self = Self(ffi::FILE_READ_DATA);
    /// FILE_WRITE_DATA（ファイルに書く。フォルダではファイルを足す）。
    pub const WRITE_DATA: Self = Self(ffi::FILE_WRITE_DATA);
    /// FILE_APPEND_DATA（ファイルの末尾に足す）。
    pub const APPEND_DATA: Self = Self(ffi::FILE_APPEND_DATA);
    /// FILE_TRAVERSE（フォルダを通り抜ける）。
    pub const TRAVERSE: Self = Self(ffi::FILE_TRAVERSE);
    /// FILE_READ_ATTRIBUTES（属性を読む）。
    pub const READ_ATTRIBUTES: Self = Self(ffi::FILE_READ_ATTRIBUTES);
    /// FILE_WRITE_ATTRIBUTES（属性・時刻を書く）。
    pub const WRITE_ATTRIBUTES: Self = Self(ffi::FILE_WRITE_ATTRIBUTES);

    /// 2つを合わせる（const の中で使えるように、`|` と同じものを関数でも置く）。
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `other` の権利をすべて含むか。
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// OS に渡す値（SYNCHRONIZE と FILE_READ_ATTRIBUTES は含まない）。
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// OS に渡す値（os-sys がいつも足すものを含める）。
    pub(crate) const fn bits_for_open(self) -> u32 {
        self.0 | ffi::SYNCHRONIZE | ffi::FILE_READ_ATTRIBUTES
    }
}

impl BitOr for Access {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        self.with(other)
    }
}

/// ほかのハンドルに許す共有。下の定数からだけ組める。
///
/// ```compile_fail,E0423
/// let _ = os_sys::Share(0x8);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Share(u32);

impl Share {
    /// FILE_SHARE_READ。
    pub const READ: Self = Self(ffi::FILE_SHARE_READ);
    /// FILE_SHARE_WRITE。
    pub const WRITE: Self = Self(ffi::FILE_SHARE_WRITE);
    /// FILE_SHARE_DELETE（ほかのハンドルに、名前の変更と削除を許す）。
    pub const DELETE: Self = Self(ffi::FILE_SHARE_DELETE);

    /// 2つを合わせる。
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `other` をすべて含むか。
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// OS に渡す値。
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl BitOr for Share {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        self.with(other)
    }
}

/// [`open_child`](crate::open_child) で、種類をどう求めるか。CreateOptions は os-sys が決める。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// フォルダだけを開く（FILE_DIRECTORY_FILE。ファイルなら ERROR_DIRECTORY＝267）。
    Dir,
    /// 種類を求めない。種類は、開いたハンドルで確かめる（[`Opened`]）。
    Any,
}

/// [`open_path`] で、パスの中のリンクをたどるか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Links {
    /// たどる（ジャンクションの道からも開ける。保管庫を開くときの根）。
    Follow,
    /// 最後の要素がリンクなら、リンクそのものを開く（FILE_FLAG_OPEN_REPARSE_POINT）。
    DontFollow,
}

/// フォルダで、リンク（ジャンクション・シンボリックリンク）でないと、ハンドルで確かめたもの。
///
/// 作れるのは [`open_path`]・[`open_child`](crate::open_child) だけ（[`create_dir_child`](crate::create_dir_child)
/// が返す [`Created<Dir>`] は、Dir を貸さない）。
///
/// 開いたときの権利と共有を覚えている。親（[`Parent`]）として使えるのは、データの権利（LIST_DIRECTORY か
/// TRAVERSE）を持ち、FILE_SHARE_DELETE なしで開いたものだけ（[`Dir::can_be_parent`]）。そうでない Dir を親に
/// 渡すと、親を受け取る関数は、何もせずに `InvalidInput` で断る。OS は RootDirectory のハンドルの権利を問わない
/// ので、「持っているあいだ、親の名前を変えられない」（R6a・R6c）ことを、ここで保つ。
#[derive(Debug)]
pub struct Dir {
    file: File,
    /// データの権利を持ち、DELETE を共有していない（親として使える）。
    holds_name: bool,
}

/// 通り抜けるだけのフォルダ（[`open_pass_dir`](crate::open_pass_dir) で、TRAVERSE・READ_ATTRIBUTES・SYNCHRONIZE、
/// 共有は READ・WRITE で開いたもの。フォルダで、リンクでない）。
///
/// 親（[`Parent`]）として渡せる。データの権利（TRAVERSE）を持ち、DELETE を共有しないので、持っているあいだ、
/// ほかのプロセスは名前を変えられない（R6c・R18）。一覧を拒否したフォルダも開ける（R2d）。
/// 列挙と空き容量には渡せない（LIST_DIRECTORY の権利がないため）。
///
/// ```compile_fail,E0308
/// fn f(p: &mut os_sys::PassDir) {
///     let _ = os_sys::read_dir_by_handle(p);
/// }
/// ```
///
/// ```compile_fail,E0308
/// fn f(p: &os_sys::PassDir) {
///     let _ = os_sys::free_space(p);
/// }
/// ```
#[derive(Debug)]
pub struct PassDir(File);

impl PassDir {
    pub(crate) fn new(file: File) -> Self {
        Self(file)
    }

    /// 中の File を借りる（最終のパス・ファイルの ID を読むため）。
    pub fn as_file(&self) -> &File {
        &self.0
    }
}

/// [`open_pass_dir`](crate::open_pass_dir) で開いたもの。
#[derive(Debug)]
pub enum OpenedPass {
    /// フォルダ（リンクでない）。
    Dir(PassDir),
    /// 名前の代理のリパースポイント（ジャンクションなど）。リンクそのものを開いている。
    Link(File),
}

/// 封じた trait の置き場（このクレートの外からは名前を書けないので、外では [`Parent`] を実装できない）。
pub(crate) mod sealed {
    use std::fs::File;
    use std::io;

    pub trait Sealed {
        /// 親のハンドル（RootDirectory に渡す）。親として使えない開き方なら Err。
        fn parent_file(&self) -> io::Result<&File>;
    }
}

/// 親として渡せるハンドル（[`open_child`](crate::open_child)・[`open_pass_dir`](crate::open_pass_dir)・
/// [`create_dir_child`](crate::create_dir_child)・[`create_file_child`](crate::create_file_child)・
/// [`rename_no_replace`](crate::rename_no_replace)・[`rename_replace`](crate::rename_replace) の親）。
///
/// 封じた trait で、このクレートの外では実装できない。実装するのは [`Dir`]（[`Dir::can_be_parent`] が真の
/// ものだけが通る）と [`PassDir`]。
///
/// [`Created<Dir>`]（DELETE の権利で持っている、作ったばかりのフォルダ）は実装しない。作ったフォルダは、
/// 確かめたら閉じて、DELETE なしで開き直してから親にする決まり（DELETE つきのフォルダの
/// ハンドルを、開いた関数の外へ出さない）を、型で保つため。なお、R2f の記録では、NTFS で DELETE つきで持つ
/// 親への rename は共有の違反にならなかった。
///
/// ```compile_fail,E0277
/// struct Mine;
/// impl os_sys::Parent for Mine {}
/// ```
///
/// ```compile_fail,E0277
/// fn f(c: &os_sys::Created<os_sys::Dir>, n: &os_sys::Name) {
///     let _ = os_sys::create_dir_child(c, n, os_sys::Access::TRAVERSE, os_sys::Share::READ);
/// }
/// ```
pub trait Parent: sealed::Sealed {}

impl sealed::Sealed for Dir {
    fn parent_file(&self) -> io::Result<&File> {
        if self.holds_name {
            Ok(&self.file)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "親として使えない開き方のフォルダ（データの権利がないか、DELETE を共有している）",
            ))
        }
    }
}

impl Parent for Dir {}

impl sealed::Sealed for PassDir {
    fn parent_file(&self) -> io::Result<&File> {
        Ok(&self.0)
    }
}

impl Parent for PassDir {}

/// 親として使える開き方か：データの権利（LIST_DIRECTORY か TRAVERSE）を持ち、FILE_SHARE_DELETE を含めない。
pub(crate) fn holds_name(access: Access, share: Share) -> bool {
    (access.contains(Access::LIST_DIRECTORY) || access.contains(Access::TRAVERSE))
        && !share.contains(Share::DELETE)
}

impl Dir {
    /// 中の File を借りる（最終のパス・std の metadata・rename で移すものに使う）。
    pub fn as_file(&self) -> &File {
        &self.file
    }

    /// 親（[`Parent`]）として使えるか（データの権利を持ち、FILE_SHARE_DELETE なしで開いたか）。
    pub fn can_be_parent(&self) -> bool {
        self.holds_name
    }

    pub(crate) fn into_file(self) -> File {
        self.file
    }
}

/// 開いたもの。種類は、開いたハンドル（std の metadata。リパースのタグも見る）で決める。
///
/// - `Link`：名前の代理のリパースポイント（ジャンクション・シンボリックリンク）。リンクそのものを開いている。
/// - `Dir`・`File`：それ以外。名前の代理でないリパースポイント（クラウドの置き場など）は、ここに入る
///   （(g) より前に扱いを直す）。
#[derive(Debug)]
pub enum Opened {
    Dir(Dir),
    File(File),
    Link(File),
}

impl Opened {
    /// 開いたハンドルの種類を確かめて分ける（std と同じ決め方）。`holds_name` は、開いたときの権利と共有で
    /// 決めた「親として使えるか」（[`holds_name`]）。
    pub(crate) fn classify(file: File, holds_name: bool) -> io::Result<Self> {
        let file_type = file.metadata()?.file_type();
        Ok(if file_type.is_symlink() {
            Self::Link(file)
        } else if file_type.is_dir() {
            Self::Dir(Dir { file, holds_name })
        } else {
            Self::File(file)
        })
    }
}

/// FILE_CREATE で新しく作ったもの（OS が FILE_CREATED を返したと確かめたもの）。
///
/// 作れるのは [`create_dir_child`](crate::create_dir_child)・[`create_file_child`](crate::create_file_child)
/// だけ。「この呼び出しで作ったものだから、作ったハンドルで消してよい」の根拠を型にする。
///
/// - `Created<File>`：中の File を借りて、中身を書く・rename で移す（一時ファイル・entry.json）。
/// - `Created<Dir>`：中の Dir を貸さない。できるのは、最終のパス・ID・metadata を読む・ハンドルで消す・閉じる（落とす）
///   だけ。このあと親として使うなら、閉じてから、親を基準に DELETE なしで開き直す。
///
/// ```compile_fail,E0599
/// fn f(c: os_sys::Created<os_sys::Dir>) {
///     let _ = c.get();
/// }
/// ```
#[derive(Debug)]
pub struct Created<T>(T);

impl<T> Created<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self(inner)
    }
}

impl Created<File> {
    /// 中の File を借りる（中身を書く・最終のパス・rename で移すものに使う）。
    pub fn get(&self) -> &File {
        &self.0
    }

    /// 中の File を取り出す（「作ったもの」の印は外れる）。
    pub fn into_inner(self) -> File {
        self.0
    }
}

impl Created<Dir> {
    /// 作ったフォルダの最終のパス。
    pub fn final_path(&self) -> io::Result<PathBuf> {
        final_path(&self.0.file)
    }

    /// 作ったフォルダのファイルの ID（閉じてから開き直したときに照らす）。
    pub fn file_id(&self) -> io::Result<FileId> {
        file_id(&self.0.file)
    }

    /// 作ったフォルダの属性・時刻（std の metadata。索引の値に使う）。
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.0.file.metadata()
    }

    /// 作ったハンドルで消す（[`delete_by_handle`](crate::delete_by_handle) と同じ。消えるのは閉じたとき。
    /// 空でなければ 145 で残る）。
    pub fn delete_by_handle(&self) -> io::Result<()> {
        info::delete_by_handle(&self.0.file)
    }
}

/// std でパスを開く（根を開くため）。権利・共有はこのクレートの型で渡す。
///
/// フォルダも開ける（FILE_FLAG_BACKUP_SEMANTICS）。`Links::DontFollow` なら、最後の要素がリンクのとき、
/// リンクそのものを開く（途中の要素のリンクは、OS がたどる）。種類は [`Opened`] で返す。std の OpenOptions は
/// FILE_FLAG_OVERLAPPED を付けないので、同期の I/O のハンドルになる。
pub fn open_path(path: &Path, links: Links, access: Access, share: Share) -> io::Result<Opened> {
    let mut flags = ffi::FILE_FLAG_BACKUP_SEMANTICS;
    if links == Links::DontFollow {
        flags |= ffi::FILE_FLAG_OPEN_REPARSE_POINT;
    }
    let file = OpenOptions::new()
        .access_mode(access.bits_for_open())
        .share_mode(share.bits())
        .custom_flags(flags)
        .open(path)?;
    Opened::classify(file, holds_name(access, share))
}

#[cfg(test)]
mod tests {
    use super::super::testdir::UnitDir;
    use super::super::{Name, Want, open_child, open_pass_dir, rename_no_replace};
    use super::*;
    use std::ffi::OsStr;
    use std::fs;

    fn dir_with(path: &Path, access: Access, share: Share) -> Dir {
        match open_path(path, Links::DontFollow, access, share).unwrap() {
            Opened::Dir(d) => d,
            other => panic!("{other:?}"),
        }
    }

    /// 親として使えるのは、データの権利（LIST か TRAVERSE）を持ち、DELETE を共有しない Dir だけ。
    /// そうでない Dir を親に渡すと、何もせずに InvalidInput。PassDir はいつも親として使える。
    #[test]
    fn only_name_holding_dirs_can_be_parents() {
        let t = UnitDir::new("parent-rights");
        fs::write(t.join("f.txt"), b"f").unwrap();
        fs::create_dir(t.join("dest")).unwrap();
        let rw = Share::READ | Share::WRITE;
        let hold = Access::LIST_DIRECTORY | Access::TRAVERSE | Access::READ_ATTRIBUTES;
        let cases = [
            (hold, rw, true),
            (Access::TRAVERSE | Access::READ_ATTRIBUTES, rw, true),
            (Access::LIST_DIRECTORY | Access::READ_ATTRIBUTES, rw, true),
            (hold | Access::DELETE, rw, true),
            (Access::READ_ATTRIBUTES, rw, false),
            (Access::DELETE | Access::READ_ATTRIBUTES, rw, false),
            (hold, rw | Share::DELETE, false),
        ];
        let name = Name::new(OsStr::new("f.txt")).unwrap();
        for (access, share, ok) in cases {
            let d = dir_with(t.path(), access, share);
            assert_eq!(d.can_be_parent(), ok, "{access:?} {share:?}");
            let opened = open_child(&d, &name, Want::Any, Access::READ_ATTRIBUTES, rw);
            assert_eq!(opened.is_ok(), ok, "{access:?} {share:?}");
            if !ok {
                assert_eq!(opened.unwrap_err().kind(), io::ErrorKind::InvalidInput);
            }
        }

        // 親に使えない Dir へは rename もしない（何も動かない）
        let top = dir_with(t.path(), Access::LIST_DIRECTORY | Access::TRAVERSE, rw);
        let f = match open_child(&top, &name, Want::Any, Access::DELETE, rw).unwrap() {
            Opened::File(f) => f,
            other => panic!("{other:?}"),
        };
        let weak = dir_with(&t.join("dest"), Access::READ_ATTRIBUTES, rw);
        let err = rename_no_replace(&f, &weak, &name).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(t.join("f.txt").is_file());
        assert!(!t.join("dest").join("f.txt").exists());

        // PassDir は親として使える
        let dest = Name::new(OsStr::new("dest")).unwrap();
        match open_pass_dir(&top, &dest).unwrap() {
            OpenedPass::Dir(p) => rename_no_replace(&f, &p, &name).unwrap(),
            other => panic!("{other:?}"),
        }
        drop(f);
        assert!(t.join("dest").join("f.txt").is_file());
    }
}
