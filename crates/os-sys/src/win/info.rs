//! ハンドルでの rename（NtSetInformationFile）と、ハンドルで消す処理（SetFileInformationByHandle）。
//!
//! 渡すバッファ（FILE_RENAME_INFORMATION・FILE_DISPOSITION_INFO(_EX)）は、安全なコードでバイト列として
//! 組み立て、8 バイトにそろえた `Vec<u64>` に写してから OS に渡す。
//!
//! rename に kernel32 の SetFileInformationByHandle（FileRenameInfo・FileRenameInfoEx）を使わないのは、
//! RootDirectory を渡すと、どの形でも ERROR_INVALID_PARAMETER（87）になるため（2026-11-16 の R1）。
//! ntdll の NtSetInformationFile は、RootDirectory を名前を解く基準にする（R1）。完全なパス・`..`・ジャンクションの
//! 親は、OS が断る（R15〜R17）。

use std::cmp::max;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle};

use super::{Name, Parent, ffi, is_unsupported, status_error};

/// 置き換えない rename。`file` を、`parent` のフォルダの中の `name` へ移す（FileRenameInformation、
/// ReplaceIfExists は偽）。
///
/// - 同じ名前があれば失敗する（ERROR_ALREADY_EXISTS＝183）。どちらも変わらない。
/// - `file` は DELETE の権利で、同期の I/O で開いたハンドル（os-sys の open_child・create_* と、std の
///   OpenOptions で FILE_FLAG_OVERLAPPED を付けずに開いたもの）。
/// - `parent` は移す先の親。RootDirectory にいつも渡す（同じフォルダの中でも）。親として使えない開き方の
///   [`Dir`](crate::Dir) なら、何もせずに `InvalidInput`。
/// - 使用中（32・33）や拒否（5）の試し直しはしない（呼ぶ側が決める）。
pub fn rename_no_replace(file: &File, parent: &impl Parent, name: &Name) -> io::Result<()> {
    let root = parent.parent_file()?.as_handle();
    nt_rename(file, root, name.wide(), ffi::NT_CLASS_RENAME_INFORMATION, 0).map_err(status_error)
}

/// 置き換える rename（vault-core では、索引と vault.json だけに使う）。
///
/// FileRenameInformationEx（REPLACE_IF_EXISTS・POSIX_SEMANTICS。今の std::fs::rename と同じ置き換え方）で行い、
/// 受け付けない（0xC000000D・0xC00000BB・0xC0000003）ときだけ FileRenameInformation（ReplaceIfExists を真）で行う。
/// 読み取り専用の印の付いた移す先は置き換えない（IGNORE_READONLY_ATTRIBUTE を付けない）。
/// 引数の決まりは [`rename_no_replace`] と同じ。
pub fn rename_replace(file: &File, parent: &impl Parent, name: &Name) -> io::Result<()> {
    let root = parent.parent_file()?.as_handle();
    let flags = ffi::FILE_RENAME_FLAG_REPLACE_IF_EXISTS | ffi::FILE_RENAME_FLAG_POSIX_SEMANTICS;
    match nt_rename(
        file,
        root,
        name.wide(),
        ffi::NT_CLASS_RENAME_INFORMATION_EX,
        flags,
    ) {
        Err(
            ffi::STATUS_INVALID_PARAMETER
            | ffi::STATUS_NOT_SUPPORTED
            | ffi::STATUS_INVALID_INFO_CLASS,
        ) => nt_rename(
            file,
            root,
            name.wide(),
            // FileRenameInformation では、先頭は BOOLEAN の ReplaceIfExists（真＝1）
            ffi::NT_CLASS_RENAME_INFORMATION,
            1,
        ),
        other => other,
    }
    .map_err(status_error)
}

/// ハンドルで消す（vault-core では guard::delete からだけ呼ぶ）。
///
/// - FileDispositionInfoEx（DELETE・POSIX_SEMANTICS・IGNORE_READONLY_ATTRIBUTE）で、読み取り専用の印の
///   ものも消す。受け付けない（87・50・1）ときだけ FileDispositionInfo で行う。
/// - 名前が消えるのは、このハンドルを閉じたとき。`file` は DELETE の権利で開いたハンドル。
/// - 空でないフォルダは ERROR_DIR_NOT_EMPTY（145）で失敗し、そのまま残る。
pub fn delete_by_handle(file: &File) -> io::Result<()> {
    let flags = ffi::FILE_DISPOSITION_FLAG_DELETE
        | ffi::FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
        | ffi::FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE;
    match set_info_bytes(file, ffi::CLASS_DISPOSITION_INFO_EX, &flags.to_ne_bytes()) {
        // FILE_DISPOSITION_INFO は BOOLEAN の DeleteFile だけ（真＝1）
        Err(e) if is_unsupported(&e) => set_info_bytes(file, ffi::CLASS_DISPOSITION_INFO, &[1]),
        other => other,
    }
}

/// NtSetInformationFile で rename する中の関数（テストからは、Name の型を通らない名前や、Dir でない
/// RootDirectory でも呼ぶ＝R15〜R17）。成功は状態の値が 0 のときだけ。失敗は状態の値を返す。
pub(crate) fn nt_rename(
    file: &File,
    root: BorrowedHandle<'_>,
    name: &[u16],
    class: i32,
    flags: u32,
) -> Result<(), i32> {
    let bytes = rename_info_bytes(flags, root.as_raw_handle().addr(), name);
    let mut words = to_words(&bytes);
    let Ok(length) = u32::try_from(bytes.len()) else {
        return Err(ffi::STATUS_INVALID_PARAMETER);
    };
    let mut io_status = ffi::IoStatusBlock {
        status: 0,
        information: 0,
    };
    // SAFETY: NtSetInformationFile の形と FileRenameInformation(Ex)（10・65）の番号は、手元の SDK になく、
    // WDK の公開の文書による（ffi.rs）。実機の R1〜R6f・R14c・R15〜R18（2026-11-16）で確かめた。
    // - `file` と `root` は、呼ぶあいだ借りている有効なハンドル。
    // - `io_status` はこの関数のスタックの IO_STATUS_BLOCK（大きさはテストで確かめた）。`file` は同期の I/O の
    //   ハンドルなので、OS は戻る前に書き終える（STATUS_PENDING が返ったら、下で戻らずに、すぐ止める）。
    // - `words` は 8 バイトにそろった `length` バイト以上の領域で、FILE_RENAME_INFORMATION（WinBase.h:9137 の
    //   FILE_RENAME_INFO と同じ並び）に組み立てた。FileNameLength は名前のバイト数で、領域の中に収まる。
    let status = unsafe {
        ffi::NtSetInformationFile(
            file.as_raw_handle(),
            &mut io_status,
            words.as_mut_ptr().cast(),
            length,
            class,
        )
    };
    if status == ffi::STATUS_PENDING {
        // 同期の I/O のハンドルでは返らない。返ったら、OS があとで `io_status`（このスタック）に書くおそれが
        // あるので、戻らずに、すぐ止める（std の同期の読み書きは、完了を待ってから止める。こちらは待たない）。
        std::process::abort();
    }
    if status == 0 { Ok(()) } else { Err(status) }
}

/// FILE_RENAME_INFORMATION（FILE_RENAME_INFO と同じ並び）のバイト列を組み立てる（安全なコード）。
///
/// - 0〜3：Flags（FileRenameInformation では先頭の1バイトが ReplaceIfExists）。
/// - 8〜15：RootDirectory（親のハンドルの値）。
/// - 16〜19：FileNameLength（名前のバイト数。NUL を含めない）。
/// - 20〜：名前（UTF-16）と NUL。大きさは max(構造体の大きさ, 20＋名前のバイト数＋2)。
fn rename_info_bytes(flags: u32, root: usize, name: &[u16]) -> Vec<u8> {
    let name_bytes = name.len() * 2;
    let size = max(
        size_of::<ffi::FileRenameInfo>(),
        ffi::RENAME_NAME_AT + name_bytes + 2,
    );
    let mut bytes = vec![0u8; size];
    put(&mut bytes, ffi::RENAME_FLAGS_AT, &flags.to_ne_bytes());
    put(&mut bytes, ffi::RENAME_ROOT_AT, &root.to_ne_bytes());
    // Name の型なら 510 以下。テストの名前も短いので u32 に収まる（収まらなければ最大の値にし、OS が断る）
    let length = u32::try_from(name_bytes).unwrap_or(u32::MAX);
    put(
        &mut bytes,
        ffi::RENAME_NAME_LENGTH_AT,
        &length.to_ne_bytes(),
    );
    for (i, unit) in name.iter().enumerate() {
        put(&mut bytes, ffi::RENAME_NAME_AT + i * 2, &unit.to_ne_bytes());
    }
    bytes
}

fn put(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes[at..at + value.len()].copy_from_slice(value);
}

/// バイト列を、8 バイトにそろえた領域に写す。
fn to_words(bytes: &[u8]) -> Vec<u64> {
    let mut words = vec![0u64; bytes.len().div_ceil(8)];
    for (word, chunk) in words.iter_mut().zip(bytes.chunks(8)) {
        let mut b = [0u8; 8];
        b[..chunk.len()].copy_from_slice(chunk);
        *word = u64::from_ne_bytes(b);
    }
    words
}

/// バイト列を、8 バイトにそろえた領域に写して SetFileInformationByHandle に渡す（消す処理だけ）。
fn set_info_bytes(file: &File, class: i32, bytes: &[u8]) -> io::Result<()> {
    let mut words = to_words(bytes);
    let size = u32::try_from(bytes.len()).map_err(|_| io::Error::other("渡す情報が大きすぎる"))?;
    // SAFETY: `file` は呼ぶあいだ借りている有効なハンドル。`words` は 8 バイトにそろった領域で、
    // `size`（＝bytes の長さ）バイト以上ある。OS はこの領域を読むだけ（_In_reads_bytes_）。
    // 中身は `class`（FileDispositionInfo・FileDispositionInfoEx）の構造体の形に組み立てた。
    let ok = unsafe {
        ffi::SetFileInformationByHandle(
            file.as_raw_handle(),
            class,
            words.as_mut_ptr().cast(),
            size,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::testdir::{UnitDir, junction};
    use super::super::{
        Access, Dir, Links, Opened, Share, Want, final_path, open_child, open_path,
    };
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// FILE_RENAME_INFORMATION の FileNameLength は名前のバイト数（NUL を含めない）。
    /// 8〜15 バイトが親のハンドルの値、20 バイトから名前、最後に NUL。
    #[test]
    fn rename_info_layout() {
        let one = Name::new(OsStr::new("a")).unwrap();
        let bytes = rename_info_bytes(0, 0x1234_5678_9ABC, one.wide());
        assert_eq!(bytes.len(), 24);
        assert_eq!(u32_at(&bytes, 0), 0);
        assert_eq!(
            usize::from_ne_bytes(bytes[8..16].try_into().unwrap()),
            0x1234_5678_9ABC
        );
        assert_eq!(u32_at(&bytes, 16), 2);
        assert_eq!(&bytes[20..22], &u16::from(b'a').to_ne_bytes());
        assert_eq!(&bytes[22..24], &[0, 0]);

        let long = Name::new(OsStr::new(&"b".repeat(255))).unwrap();
        let bytes = rename_info_bytes(3, 7, long.wide());
        assert_eq!(bytes.len(), 20 + 510 + 2);
        assert_eq!(u32_at(&bytes, 0), 3);
        assert_eq!(usize::from_ne_bytes(bytes[8..16].try_into().unwrap()), 7);
        assert_eq!(u32_at(&bytes, 16), 510);
        assert_eq!(&bytes[20..22], &u16::from(b'b').to_ne_bytes());
        assert_eq!(&bytes[528..530], &u16::from(b'b').to_ne_bytes());
        assert_eq!(&bytes[530..532], &[0, 0]);

        // FileRenameInformation の ReplaceIfExists（BOOLEAN）は先頭の1バイト
        let replace = rename_info_bytes(1, 0, one.wide());
        assert_eq!(replace[0], 1);
        assert_eq!(&replace[1..4], &[0, 0, 0]);
    }

    const HOLD_DIR: Access = Access::LIST_DIRECTORY
        .with(Access::TRAVERSE)
        .with(Access::READ_ATTRIBUTES);
    const SHARE_RW: Share = Share::READ.with(Share::WRITE);

    fn held(path: &Path) -> Dir {
        match open_path(path, Links::DontFollow, HOLD_DIR, SHARE_RW).unwrap() {
            Opened::Dir(d) => d,
            other => panic!("{other:?}"),
        }
    }

    fn target_file(parent: &Dir, n: &str) -> File {
        match open_child(
            parent,
            &Name::new(OsStr::new(n)).unwrap(),
            Want::Any,
            Access::DELETE | Access::READ_ATTRIBUTES,
            SHARE_RW,
        )
        .unwrap()
        {
            Opened::File(f) => f,
            other => panic!("{other:?}"),
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().collect()
    }

    fn status_text(r: Result<(), i32>) -> String {
        match r {
            Ok(()) => "ok".to_string(),
            Err(s) => format!(
                "status=0x{:08X}(win32={:?})",
                s,
                status_error(s).raw_os_error()
            ),
        }
    }

    /// R15：RootDirectory と、先頭が `\??\` か `\` の名前（完全なパス）を、型を通さずに直に渡す →
    /// 失敗し、何も動かない（RootDirectory が無視されて、完全なパスの先へ移ることはない）。
    #[test]
    fn r15_absolute_names_with_root_are_rejected() {
        let t = UnitDir::new("r15");
        fs::create_dir(t.join("d1")).unwrap();
        fs::create_dir(t.join("d2")).unwrap();
        fs::write(t.join("d1").join("a.txt"), b"a").unwrap();
        let d1 = held(&t.join("d1"));
        let d2 = held(&t.join("d2"));
        let a = target_file(&d1, "a.txt");
        // \\?\D:\…\d2 の形
        let d2_text = final_path(d2.as_file())
            .unwrap()
            .as_os_str()
            .to_string_lossy()
            .into_owned();
        assert!(d2_text.starts_with(r"\\?\") && d2_text.as_bytes()[5] == b':');
        let unique = format!("os-sys-r15-{}.txt", std::process::id());
        let nt_full = format!(r"\??\{}\{unique}", &d2_text[4..]);
        let rooted = format!(r"{}\{unique}", &d2_text[6..]);
        let top = format!(r"\{unique}");
        for (label, name) in [
            (r"\??\ の完全なパス", &nt_full),
            (r"先頭が \ のパス（ドライブなし）", &rooted),
            (r"\<名前>", &top),
        ] {
            let r = nt_rename(
                &a,
                d2.as_file().as_handle(),
                &wide(name),
                ffi::NT_CLASS_RENAME_INFORMATION,
                0,
            );
            println!("R15 {label}：{}", status_text(r));
            assert!(r.is_err(), "{label}");
        }
        drop(a);
        assert!(t.join("d1").join("a.txt").is_file());
        assert!(!t.join("d2").join(&unique).exists());
        assert!(!Path::new(&unique).exists());
        assert!(!Path::new(&top).exists());
    }

    /// R16：ジャンクション（外のフォルダを指す）をリンクそのものとして開いたハンドルを RootDirectory に渡す →
    /// 失敗し、外に届かない。
    #[test]
    fn r16_junction_handle_as_root_is_rejected() {
        let t = UnitDir::new("r16");
        let outside = UnitDir::new("r16-outside");
        fs::create_dir(t.join("d1")).unwrap();
        fs::write(t.join("d1").join("a.txt"), b"a").unwrap();
        if !junction(&t.join("j"), outside.path()) {
            return;
        }
        let top = held(t.path());
        let link = match open_child(
            &top,
            &Name::new(OsStr::new("j")).unwrap(),
            Want::Any,
            HOLD_DIR,
            SHARE_RW,
        )
        .unwrap()
        {
            Opened::Link(l) => l,
            other => panic!("{other:?}"),
        };
        let d1 = held(&t.join("d1"));
        let a = target_file(&d1, "a.txt");
        let r = nt_rename(
            &a,
            link.as_handle(),
            &wide("moved.txt"),
            ffi::NT_CLASS_RENAME_INFORMATION,
            0,
        );
        println!(
            "R16 ジャンクションのハンドルを RootDirectory に：{}",
            status_text(r)
        );
        assert!(r.is_err());
        drop(a);
        assert!(!outside.join("moved.txt").exists());
        assert!(t.join("d1").join("a.txt").is_file());
    }

    /// R17：名前に `..` → 失敗し、外（親の隣）に届かない。
    #[test]
    fn r17_dotdot_in_name_is_rejected() {
        let t = UnitDir::new("r17");
        for d in ["d1", "d2", "outside"] {
            fs::create_dir(t.join(d)).unwrap();
        }
        fs::write(t.join("d1").join("a.txt"), b"a").unwrap();
        let d1 = held(&t.join("d1"));
        let d2 = held(&t.join("d2"));
        let a = target_file(&d1, "a.txt");
        for name in [r"..\outside\i.txt", r"..\i.txt", ".."] {
            let r = nt_rename(
                &a,
                d2.as_file().as_handle(),
                &wide(name),
                ffi::NT_CLASS_RENAME_INFORMATION,
                0,
            );
            println!("R17 名前 {name}：{}", status_text(r));
            assert!(r.is_err(), "{name}");
        }
        drop(a);
        assert!(t.join("d1").join("a.txt").is_file());
        assert!(!t.join("outside").join("i.txt").exists());
        assert!(!t.join("i.txt").exists());
    }

    /// FileRenameInformationEx に Flags 0 を渡すと、置き換えずに移る（同じ名前があれば 183）。
    #[test]
    fn rename_information_ex_without_flags_does_not_replace() {
        let t = UnitDir::new("rename-ex-flags0");
        fs::create_dir(t.join("d")).unwrap();
        fs::write(t.join("d").join("a.txt"), b"a").unwrap();
        fs::write(t.join("d").join("b.txt"), b"b").unwrap();
        let d = held(&t.join("d"));
        let a = target_file(&d, "a.txt");
        let onto = nt_rename(
            &a,
            d.as_file().as_handle(),
            &wide("b.txt"),
            ffi::NT_CLASS_RENAME_INFORMATION_EX,
            0,
        );
        assert_eq!(
            onto.map_err(|s| status_error(s).raw_os_error()),
            Err(Some(183))
        );
        nt_rename(
            &a,
            d.as_file().as_handle(),
            &wide("a2.txt"),
            ffi::NT_CLASS_RENAME_INFORMATION_EX,
            0,
        )
        .unwrap();
        drop(a);
        assert_eq!(fs::read_to_string(t.join("d").join("b.txt")).unwrap(), "b");
        assert!(t.join("d").join("a2.txt").is_file());
    }
}
