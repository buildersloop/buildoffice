//! 開いたハンドルの最終のパス（W1。GetFinalPathNameByHandleW）。

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle};
use std::path::PathBuf;

use super::ffi;

/// はじめに用意する大きさ（UTF-16 の単位。NUL を含む）。
const INITIAL_UNITS: usize = 260;
/// 大きさが足りなかったときに取り直す回数の上限。
const RETRIES: usize = 3;

/// 開いたハンドルの最終のパス（flags 0＝FILE_NAME_NORMALIZED・VOLUME_NAME_DOS。`\\?\C:\…` の形）。
///
/// リンクをたどって開いたハンドルなら、たどった先のパスになる。リンクそのものを開いたハンドルなら、
/// リンクのパスになる。エラーの文にパスは入れない。
pub fn final_path(handle: &impl AsHandle) -> io::Result<PathBuf> {
    final_path_from(handle.as_handle(), INITIAL_UNITS)
}

/// はじめの大きさを変えて呼べる中の関数（R9 のテストで 0・1・ちょうど・十分を試す）。
fn final_path_from(handle: BorrowedHandle<'_>, initial: usize) -> io::Result<PathBuf> {
    let mut units = initial;
    for _ in 0..=RETRIES {
        let mut buf = vec![0u16; units];
        let cch = u32::try_from(buf.len()).map_err(|_| io::Error::other("最終のパスが長すぎる"))?;
        // SAFETY: `handle` は呼ぶあいだ借りている有効なハンドル。`buf` は `cch` 個の u16 を書ける領域で、
        // OS は `cch` 個を超えて書かない（_Out_writes_(cchFilePath)）。`cch` が 0 のときは何も書かない。
        let n = unsafe {
            ffi::GetFinalPathNameByHandleW(
                handle.as_raw_handle(),
                buf.as_mut_ptr(),
                cch,
                ffi::FINAL_PATH_FLAGS,
            )
        };
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        let n = usize::try_from(n).map_err(|_| io::Error::other("最終のパスが長すぎる"))?;
        if n < buf.len() {
            // 成功：NUL を含めない長さ
            buf.truncate(n);
            return Ok(PathBuf::from(OsString::from_wide(&buf)));
        }
        // 足りない：NUL を含めた要る大きさ（そのあいだに名前が変わって伸びることもあるので、回数に上限）
        units = n;
    }
    Err(io::Error::other(
        "最終のパスを読めない（読むたびに長さが変わった）",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    /// R9：はじめの大きさを 0・1・ちょうど（NUL の分が足りない・ちょうど）・十分にしても、同じ結果。
    #[test]
    fn r9_final_path_same_for_any_initial_size() {
        // 読むだけ。このクレートの Cargo.toml を開く（ファイルを作らない）
        let file = File::open(concat!(env!("CARGO_MANIFEST_DIR"), "\\Cargo.toml")).unwrap();
        let expected = final_path(&file).unwrap();
        // UTF-16 の単位の数（NUL を含めない）
        let len = std::os::windows::ffi::OsStrExt::encode_wide(expected.as_os_str()).count();
        assert!(expected.as_os_str().to_string_lossy().starts_with(r"\\?\"));
        assert!(expected.ends_with("Cargo.toml"));
        for initial in [0, 1, len - 1, len, len + 1, 4096] {
            let got = final_path_from(file.as_handle(), initial).unwrap();
            assert_eq!(got, expected, "はじめの大きさ {initial}");
        }
    }
}
