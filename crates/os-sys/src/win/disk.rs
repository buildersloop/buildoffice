//! 空き容量（W3。GetDiskFreeSpaceExW）。

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr;

use super::ffi;
use super::{Dir, final_path};

/// 呼ぶユーザーが使える空き容量（バイト。クォータを含む lpFreeBytesAvailableToCaller）。
///
/// パスで問い合わせると、途中のジャンクションで別のボリュームを測る道があるので、`dir` のハンドルから読んだ
/// 最終のパス（`\\?\C:\…`）に `\` を付けて問い合わせる。
pub fn free_space(dir: &Dir) -> io::Result<u64> {
    let path = final_path(dir.as_file())?;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::other("最終のパスに NUL がある"));
    }
    if wide.last() != Some(&u16::from(b'\\')) {
        wide.push(u16::from(b'\\'));
    }
    wide.push(0);
    let mut available = 0u64;
    // SAFETY: `wide` は NUL で終わる UTF-16 の文字列で、呼ぶあいだ生きている。`available` は 8 バイトの
    // 出力（ULARGE_INTEGER）。ほかの2つの出力は NULL（省略してよい引数）。
    let ok = unsafe {
        ffi::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(available)
    }
}
