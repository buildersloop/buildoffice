//! Windows の実装。unsafe のブロックは ffi.rs の関数を呼ぶ所と、OS が返したハンドルを包む所だけ。

mod child;
mod dir;
mod disk;
mod ffi;
mod id;
mod info;
mod known;
mod name;
mod path;
mod random;
mod types;

#[cfg(test)]
mod testdir;

use std::io;

pub use child::{create_dir_child, create_file_child, open_child, open_pass_dir};
pub use dir::{DirEntry, EntryKind, read_dir_by_handle};
pub use disk::free_space;
pub use id::{FileId, file_id, link_count};
pub use info::{delete_by_handle, rename_no_replace, rename_replace};
pub use known::{KnownFolder, known_folder};
pub use name::{InvalidName, Name};
pub use path::final_path;
pub use random::os_random;
pub use types::{
    Access, Created, Dir, Links, Opened, OpenedPass, Parent, PassDir, Share, Want, open_path,
};

/// OBJ_DONT_REPARSE を付けて開いた・作ったときに、リパースポイントに当たって断ったエラーか
/// （STATUS_REPARSE_POINT_ENCOUNTERED を直した ERROR_REPARSE_POINT_ENCOUNTERED＝4395）。
///
/// vault-core は、これを IO_ERROR にせず、利用者のパスなら OUTSIDE_VAULT、管理用なら NOT_A_VAULT にする。
pub fn is_reparse_point_encountered(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ffi::ERROR_REPARSE_POINT_ENCOUNTERED)
}

/// 「この情報の種類を受け付けない」とみなすエラー（87・50・1）。古い OS・ファイルシステムへの切り替えに使う。
fn is_unsupported(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(ffi::ERROR_INVALID_PARAMETER | ffi::ERROR_NOT_SUPPORTED | ffi::ERROR_INVALID_FUNCTION)
    )
}

/// NTSTATUS を io::Error に直す（RtlNtStatusToDosError）。直せない値は、状態の値を文に入れる。
fn status_error(status: i32) -> io::Error {
    // SAFETY: RtlNtStatusToDosError は整数を受け取って整数を返すだけで、ポインタを受け取らない。
    // どの値を渡してもよい（知らない値には ERROR_MR_MID_NOT_FOUND などを返す）。
    let code = unsafe { ffi::RtlNtStatusToDosError(status) };
    match i32::try_from(code) {
        Ok(code) if code != 0 => io::Error::from_raw_os_error(code),
        _ => io::Error::other(format!("OS が状態の値 0x{status:08X} を返した")),
    }
}
