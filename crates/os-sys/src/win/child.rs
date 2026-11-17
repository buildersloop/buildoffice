//! 親のフォルダのハンドルを基準に、リンクをたどらずに開く・作る（W15。NtCreateFile）。
//!
//! 包み方
//! - 出力のハンドルは NULL で始める。状態の値が 0（STATUS_SUCCESS）で、ハンドルが NULL でも -1 でもないときだけ、
//!   OwnedHandle に包む（std の File にする）。
//! - 負の状態の値なら、ハンドルを包まず閉じず、RtlNtStatusToDosError で直したエラーを返す。
//! - 0 以外で負でない値が返ったら（起きないはず）、ハンドルが NULL・-1 でなければ包んで閉じてから、エラーにする。
//! - IO_STATUS_BLOCK の Information は、開くなら FILE_OPENED、作るなら FILE_CREATED であることを確かめる。
//!   違えば、包んで閉じてからエラーにする。
//! - CreateOptions は、このファイルの中で決める（呼ぶ側に渡させない）。親は封じた trait [`Parent`]（親として使える
//!   開き方の [`Dir`] と [`PassDir`]）だけを受ける。

use std::fs::File;
use std::io;
use std::os::windows::io::{
    AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle,
};
use std::ptr;

use super::ffi;
use super::types::holds_name;
use super::{
    Access, Created, Dir, Name, Opened, OpenedPass, Parent, PassDir, Share, Want, status_error,
};

/// OBJ_DONT_REPARSE を付けるか。R7① で、付けても開く・作るの結果が変わらないことを確かめた（2026-11-16、
/// この PC の Windows 11）。付けると、名前を解くあいだにリパースポイントに当たったときに、たどらずに
/// STATUS_REPARSE_POINT_ENCOUNTERED で断る（R7③）。
const USE_DONT_REPARSE: bool = true;

/// 親を基準に、リンクをたどらずに開く（FILE_OPEN・FILE_OPEN_REPARSE_POINT）。
///
/// - `name` がリンクなら、リンクそのものを開く（[`Opened::Link`]）。種類は開いたハンドルで確かめる。
/// - `Want::Dir` ならフォルダだけを開く（ファイルなら ERROR_DIRECTORY＝267）。
/// - ないときは ERROR_FILE_NOT_FOUND（2）。共有の違反は 32、拒否は 5。
pub fn open_child(
    parent: &impl Parent,
    name: &Name,
    want: Want,
    access: Access,
    share: Share,
) -> io::Result<Opened> {
    let mut options = ffi::FILE_OPEN_REPARSE_POINT | ffi::FILE_SYNCHRONOUS_IO_NONALERT;
    if want == Want::Dir {
        options |= ffi::FILE_DIRECTORY_FILE;
    }
    let (file, information) = open_relative(parent, name, access, share, ffi::FILE_OPEN, options)?;
    if information != ffi::FILE_OPENED {
        // 起きないはず。ハンドルは file を落とすときに閉じる
        return Err(io::Error::other(
            "開いたのに、OS が FILE_OPENED を返さなかった",
        ));
    }
    Opened::classify(file, holds_name(access, share))
}

/// 通り抜けるだけのフォルダの開き方（TRAVERSE。os-sys が READ_ATTRIBUTES と SYNCHRONIZE を足す）と共有。
const PASS_ACCESS: Access = Access::TRAVERSE;
const PASS_SHARE: Share = Share::READ.with(Share::WRITE);

/// 親を基準に、通り抜けるだけのフォルダとして開く（[`PassDir`]）。
///
/// 権利は TRAVERSE・READ_ATTRIBUTES・SYNCHRONIZE、共有は READ・WRITE で、os-sys が決める（呼ぶ側に渡させない）。
/// フォルダを求め（ファイルなら 267）、リンクはたどらない（リンクなら [`OpenedPass::Link`]）。
pub fn open_pass_dir(parent: &impl Parent, name: &Name) -> io::Result<OpenedPass> {
    match open_child(parent, name, Want::Dir, PASS_ACCESS, PASS_SHARE)? {
        Opened::Dir(dir) => Ok(OpenedPass::Dir(PassDir::new(dir.into_file()))),
        Opened::Link(file) => Ok(OpenedPass::Link(file)),
        Opened::File(_) => Err(io::Error::other(
            "フォルダを求めて開いたのに、フォルダでなかった",
        )),
    }
}

/// 親を基準に、新しいフォルダを作る（FILE_CREATE・FILE_DIRECTORY_FILE）。同じ名前があれば 183。
pub fn create_dir_child(
    parent: &impl Parent,
    name: &Name,
    access: Access,
    share: Share,
) -> io::Result<Created<Dir>> {
    let options =
        ffi::FILE_DIRECTORY_FILE | ffi::FILE_OPEN_REPARSE_POINT | ffi::FILE_SYNCHRONOUS_IO_NONALERT;
    let (file, information) =
        open_relative(parent, name, access, share, ffi::FILE_CREATE, options)?;
    if information != ffi::FILE_CREATED {
        return Err(io::Error::other(
            "作ったのに、OS が FILE_CREATED を返さなかった",
        ));
    }
    match Opened::classify(file, false)? {
        Opened::Dir(dir) => Ok(Created::new(dir)),
        _ => Err(io::Error::other("作ったものが、フォルダでなかった")),
    }
}

/// 親を基準に、新しいファイルを作る（FILE_CREATE・FILE_NON_DIRECTORY_FILE）。同じ名前があれば 183。
pub fn create_file_child(
    parent: &impl Parent,
    name: &Name,
    access: Access,
    share: Share,
) -> io::Result<Created<File>> {
    let options = ffi::FILE_NON_DIRECTORY_FILE
        | ffi::FILE_OPEN_REPARSE_POINT
        | ffi::FILE_SYNCHRONOUS_IO_NONALERT;
    let (file, information) =
        open_relative(parent, name, access, share, ffi::FILE_CREATE, options)?;
    if information != ffi::FILE_CREATED {
        return Err(io::Error::other(
            "作ったのに、OS が FILE_CREATED を返さなかった",
        ));
    }
    match Opened::classify(file, false)? {
        Opened::File(file) => Ok(Created::new(file)),
        _ => Err(io::Error::other("作ったものが、ふつうのファイルでなかった")),
    }
}

/// 親を基準に開く・作る。OBJ_DONT_REPARSE を付けて 87 が返ったら（受け付けない古い OS）、1回だけ外して開き直す。
fn open_relative(
    parent: &impl Parent,
    name: &Name,
    access: Access,
    share: Share,
    disposition: u32,
    options: u32,
) -> io::Result<(File, usize)> {
    let root = parent.parent_file()?.as_handle();
    let access = access.bits_for_open();
    let share = share.bits();
    let mut attributes = ffi::OBJ_CASE_INSENSITIVE;
    if USE_DONT_REPARSE {
        attributes |= ffi::OBJ_DONT_REPARSE;
    }
    let result = match nt_create(root, name, access, share, disposition, options, attributes) {
        Err(NtError::Status(status))
            if USE_DONT_REPARSE
                && status_error(status).raw_os_error() == Some(ffi::ERROR_INVALID_PARAMETER) =>
        {
            nt_create(
                root,
                name,
                access,
                share,
                disposition,
                options,
                ffi::OBJ_CASE_INSENSITIVE,
            )
        }
        other => other,
    };
    match result {
        Ok((handle, information)) => Ok((File::from(handle), information)),
        Err(e) => Err(e.into()),
    }
}

/// NtCreateFile が失敗したときの中身。
#[derive(Debug)]
pub(crate) enum NtError {
    /// 0 以外の状態の値。
    Status(i32),
    /// 状態の値は 0 なのに、ハンドルが NULL か -1（起きないはず）。
    BadHandle,
}

impl From<NtError> for io::Error {
    fn from(e: NtError) -> Self {
        match e {
            NtError::Status(status) => status_error(status),
            NtError::BadHandle => io::Error::other("OS が有効なハンドルを返さなかった"),
        }
    }
}

/// NtCreateFile を1回呼ぶ（中の関数。テストからは、Dir でないハンドルを基準にも呼ぶ＝R7③）。
///
/// 返すのは、包んだハンドルと IO_STATUS_BLOCK の Information。
pub(crate) fn nt_create(
    root: BorrowedHandle<'_>,
    name: &Name,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
    attributes: u32,
) -> Result<(OwnedHandle, usize), NtError> {
    // 名前は 255 単位以下（Name の型）なので、バイト数は 510 以下で u16 に収まる
    let name_bytes = name.byte_len() as u16;
    // OS は名前を読むだけだが、UNICODE_STRING の Buffer は書ける形のポインタなので、写しを渡す
    let mut name_buf: Vec<u16> = name.wide().to_vec();
    let mut object_name = ffi::UnicodeString {
        length: name_bytes,
        maximum_length: name_bytes,
        buffer: name_buf.as_mut_ptr(),
    };
    let mut object_attributes = ffi::ObjectAttributes {
        length: ffi::OBJECT_ATTRIBUTES_LENGTH,
        root_directory: root.as_raw_handle(),
        object_name: &mut object_name,
        attributes,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = ffi::IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut handle: RawHandle = ptr::null_mut();
    // SAFETY: どのポインタも、この関数の中で生きている領域を指し、呼ぶあいだ動かない。
    // - `handle`・`io_status` は OS が書く出力（HANDLE 1つと IO_STATUS_BLOCK。大きさはテストで確かめた）。
    // - `object_attributes` は Length＝48、RootDirectory は呼ぶあいだ借りている有効なハンドル、ObjectName は
    //   `object_name`（Buffer は `name_buf` の `name_bytes` バイト）、SecurityDescriptor・QoS は NULL。
    // - AllocationSize・EaBuffer は NULL、EaLength は 0（どちらも省略してよい引数）。
    let status = unsafe {
        ffi::NtCreateFile(
            &mut handle,
            access,
            &mut object_attributes,
            &mut io_status,
            ptr::null_mut(),
            ffi::FILE_ATTRIBUTE_NORMAL,
            share,
            disposition,
            options,
            ptr::null_mut(),
            0,
        )
    };
    let valid = !handle.is_null() && handle.addr() != usize::MAX;
    if status == 0 && valid {
        // SAFETY: NtCreateFile が STATUS_SUCCESS で返した、NULL でも -1 でもないハンドル。
        // ほかのどこにも渡していないので、ここで持ち主になり、OwnedHandle が1回だけ閉じる。
        let owned = unsafe { OwnedHandle::from_raw_handle(handle) };
        return Ok((owned, io_status.information));
    }
    if status > 0 && valid {
        // 0 以外で負でない値（起きないはず）。ハンドルが返っていれば、包んで閉じる。
        // SAFETY: 上と同じく、OS が返したばかりの、ほかに渡していないハンドル。包んですぐ落とし、1回だけ閉じる。
        drop(unsafe { OwnedHandle::from_raw_handle(handle) });
    }
    // 負の値（失敗）のときは、ハンドルを包まず閉じない（OS はハンドルを返していない）。
    if status == 0 {
        Err(NtError::BadHandle)
    } else {
        Err(NtError::Status(status))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testdir::{UnitDir, junction};
    use super::*;
    use std::ffi::OsStr;
    use std::fs;

    const HOLD_DIR: Access = Access::LIST_DIRECTORY
        .with(Access::TRAVERSE)
        .with(Access::READ_ATTRIBUTES);
    const SHARE_RW: Share = Share::READ.with(Share::WRITE);

    fn name(s: &str) -> Name {
        Name::new(OsStr::new(s)).unwrap()
    }

    fn root(dir: &UnitDir) -> Dir {
        match super::super::open_path(
            dir.path(),
            super::super::Links::DontFollow,
            HOLD_DIR,
            SHARE_RW,
        )
        .unwrap()
        {
            Opened::Dir(d) => d,
            other => panic!("根がフォルダでない：{other:?}"),
        }
    }

    /// 状態の値と Information を、文字にして記録する（R7① の比べのため）。
    fn outcome(r: &Result<(OwnedHandle, usize), NtError>) -> String {
        match r {
            Ok((_, info)) => format!("ok(information={info})"),
            Err(NtError::Status(s)) => format!(
                "status=0x{:08X}(win32={:?})",
                s,
                status_error(*s).raw_os_error()
            ),
            Err(NtError::BadHandle) => "bad-handle".to_string(),
        }
    }

    fn run_r7_cases(parent: &Dir, attributes: u32, tag: &str) -> Vec<String> {
        let access_dir = HOLD_DIR.bits_for_open();
        let common = ffi::FILE_OPEN_REPARSE_POINT | ffi::FILE_SYNCHRONOUS_IO_NONALERT;
        let h = parent.as_file().as_handle();
        let mut out = Vec::new();
        // 1 作る（フォルダ・ファイル）
        out.push(outcome(&nt_create(
            h,
            &name(&format!("d-{tag}")),
            access_dir,
            SHARE_RW.bits(),
            ffi::FILE_CREATE,
            common | ffi::FILE_DIRECTORY_FILE,
            attributes,
        )));
        out.push(outcome(&nt_create(
            h,
            &name(&format!("f-{tag}.txt")),
            Access::WRITE_DATA.bits_for_open(),
            Share::READ.bits(),
            ffi::FILE_CREATE,
            common | ffi::FILE_NON_DIRECTORY_FILE,
            attributes,
        )));
        // 2 同じ名前がある
        out.push(outcome(&nt_create(
            h,
            &name(&format!("f-{tag}.txt")),
            Access::WRITE_DATA.bits_for_open(),
            Share::READ.bits(),
            ffi::FILE_CREATE,
            common | ffi::FILE_NON_DIRECTORY_FILE,
            attributes,
        )));
        // 3 ジャンクションを開くと、リンクそのもの
        let link = nt_create(
            h,
            &name("j"),
            access_dir,
            SHARE_RW.bits(),
            ffi::FILE_OPEN,
            common,
            attributes,
        );
        if let Ok((handle, _)) = &link {
            let file = File::from(handle.try_clone().unwrap());
            let attrs =
                std::os::windows::fs::MetadataExt::file_attributes(&file.metadata().unwrap());
            assert_ne!(
                attrs & ffi::FILE_ATTRIBUTE_REPARSE_POINT,
                0,
                "{tag}：リンクそのものでない"
            );
            assert!(matches!(
                Opened::classify(file, false).unwrap(),
                Opened::Link(_)
            ));
        }
        out.push(outcome(&link));
        // 4 フォルダを求めてファイルを開く
        out.push(outcome(&nt_create(
            h,
            &name(&format!("f-{tag}.txt")),
            access_dir,
            SHARE_RW.bits(),
            ffi::FILE_OPEN,
            common | ffi::FILE_DIRECTORY_FILE,
            attributes,
        )));
        out
    }

    /// R7①：OBJ_DONT_REPARSE を付けても、R7 の4つ（作る・同じ名前・リンクそのもの・フォルダを求めてファイル）が
    /// 同じ結果になる。返った状態の値を出力に記録する。
    #[test]
    fn r7_1_dont_reparse_gives_same_results() {
        let base = UnitDir::new("r7-1");
        let outside = UnitDir::new("r7-1-outside");
        fs::create_dir(base.join("p")).unwrap();
        if !junction(&base.join("p").join("j"), outside.path()) {
            return;
        }
        let top = root(&base);
        let parent = match open_child(&top, &name("p"), Want::Dir, HOLD_DIR, SHARE_RW).unwrap() {
            Opened::Dir(d) => d,
            other => panic!("{other:?}"),
        };
        let without = run_r7_cases(&parent, ffi::OBJ_CASE_INSENSITIVE, "plain");
        let with = run_r7_cases(
            &parent,
            ffi::OBJ_CASE_INSENSITIVE | ffi::OBJ_DONT_REPARSE,
            "dont",
        );
        println!("R7① OBJ_DONT_REPARSE なし：{without:?}");
        println!("R7① OBJ_DONT_REPARSE あり：{with:?}");
        assert_eq!(without, with);
        assert_eq!(
            with,
            vec![
                "ok(information=2)".to_string(),
                "ok(information=2)".to_string(),
                "status=0xC0000035(win32=Some(183))".to_string(),
                "ok(information=1)".to_string(),
                "status=0xC0000103(win32=Some(267))".to_string(),
            ]
        );
    }

    /// R7③（記録）：ジャンクション J（外のフォルダ O を指す）をリンクそのものとして開き、そのハンドルを基準に、
    /// O にある名前を開く・新しい名前を作る。O に届くかを、OBJ_DONT_REPARSE のあり・なしで記録する。
    #[test]
    fn r7_3_relative_to_opened_junction() {
        let base = UnitDir::new("r7-3");
        let outside = UnitDir::new("r7-3-outside");
        fs::write(outside.join("inside.txt"), b"outside").unwrap();
        if !junction(&base.join("j"), outside.path()) {
            return;
        }
        let top = root(&base);
        let link = match open_child(&top, &name("j"), Want::Any, HOLD_DIR, SHARE_RW).unwrap() {
            Opened::Link(f) => f,
            other => panic!("リンクとして開けない：{other:?}"),
        };
        let common = ffi::FILE_OPEN_REPARSE_POINT | ffi::FILE_SYNCHRONOUS_IO_NONALERT;
        let mut reached_outside = false;
        for (label, attributes) in [
            ("なし", ffi::OBJ_CASE_INSENSITIVE),
            ("あり", ffi::OBJ_CASE_INSENSITIVE | ffi::OBJ_DONT_REPARSE),
        ] {
            let opened = nt_create(
                link.as_handle(),
                &name("inside.txt"),
                Access::READ_DATA.bits_for_open(),
                SHARE_RW.bits(),
                ffi::FILE_OPEN,
                common,
                attributes,
            );
            let open_text = match &opened {
                Ok((h, info)) => {
                    let p = super::super::final_path(h).unwrap();
                    let outside_final = super::super::final_path(
                        &fs::File::open(outside.join("inside.txt")).unwrap(),
                    )
                    .unwrap();
                    if p == outside_final {
                        reached_outside = true;
                    }
                    format!(
                        "ok(information={info}, 外のファイル={})",
                        p == outside_final
                    )
                }
                Err(_) => outcome(&opened),
            };
            let new_name = format!(
                "new-{}.txt",
                if attributes & ffi::OBJ_DONT_REPARSE != 0 {
                    "dont"
                } else {
                    "plain"
                }
            );
            let created = nt_create(
                link.as_handle(),
                &name(&new_name),
                (Access::WRITE_DATA | Access::DELETE).bits_for_open(),
                Share::READ.bits(),
                ffi::FILE_CREATE,
                common | ffi::FILE_NON_DIRECTORY_FILE,
                attributes,
            );
            let in_outside = outside.join(&new_name).exists();
            if in_outside {
                reached_outside = true;
            }
            let create_text = match &created {
                Ok((h, info)) => {
                    // 作れてしまったら、作ったハンドルで消す（どこにできたかは、外にあるかで記録する）
                    let f = File::from(h.try_clone().unwrap());
                    super::super::delete_by_handle(&f).unwrap();
                    format!("ok(information={info}, 外にできた={in_outside})")
                }
                Err(_) => outcome(&created),
            };
            println!("R7③ OBJ_DONT_REPARSE {label}：開く={open_text}、作る={create_text}");
        }
        println!("R7③ 外へ届いたか：{reached_outside}");
    }

    /// 呼ぶ側に渡させない CreateOptions（DELETE_ON_CLOSE・OPEN_BY_FILE_ID・OPEN_FOR_BACKUP_INTENT）と、
    /// OBJ_INHERIT が、組む値に入らない。
    #[test]
    fn options_never_include_forbidden_flags() {
        let all = ffi::FILE_DIRECTORY_FILE
            | ffi::FILE_NON_DIRECTORY_FILE
            | ffi::FILE_OPEN_REPARSE_POINT
            | ffi::FILE_SYNCHRONOUS_IO_NONALERT;
        assert_eq!(all & ffi::FORBIDDEN_CREATE_OPTIONS, 0);
        let attributes = ffi::OBJ_CASE_INSENSITIVE | ffi::OBJ_DONT_REPARSE;
        assert_eq!(attributes & ffi::OBJ_INHERIT, 0);
    }
}
