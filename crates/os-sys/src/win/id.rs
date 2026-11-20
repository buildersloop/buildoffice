//! 開いたハンドルのファイルの ID とリンクの数（GetFileInformationByHandleEx の FileIdInfo と FileStandardInfo）。

use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsHandle, AsRawHandle};

use super::ffi;

/// ファイルの ID（ボリュームの番号と、128 ビットの ID）。同じボリュームの中で名前を変えても変わらない。
///
/// 作ったフォルダを閉じてから開き直したときに、同じものかを照らすのに使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId {
    /// ボリュームの番号（VolumeSerialNumber）。
    pub volume: u64,
    /// ボリュームの中の ID（FILE_ID_128）。
    pub id: [u8; 16],
}

/// 開いたハンドルのファイルの ID を読む。読めないファイルシステム（FAT 系の見込み）では Err。
pub fn file_id(handle: &impl AsHandle) -> io::Result<FileId> {
    let mut info = ffi::FileIdInfo {
        volume_serial_number: 0,
        file_id: [0; 16],
    };
    // FILE_ID_INFO の大きさ（24。テストで確かめた）
    let size = size_of::<ffi::FileIdInfo>() as u32;
    // SAFETY: ハンドルは呼ぶあいだ借りている有効なもの。`info` は FILE_ID_INFO と同じ形（#[repr(C)]、
    // 大きさと位置をテストで確かめた）の、`size` バイトの書ける領域で、OS は `size` バイトを超えて書かない。
    // どのバイトの並びも u64 と [u8; 16] として正しい値になる。
    let ok = unsafe {
        ffi::GetFileInformationByHandleEx(
            handle.as_handle().as_raw_handle(),
            ffi::CLASS_ID_INFO,
            (&raw mut info).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileId {
        volume: info.volume_serial_number,
        id: info.file_id,
    })
}

/// 開いたハンドルのファイルのリンクの数（NumberOfLinks）を読む。ハードリンクでほかの名前があれば2以上。
///
/// 属性だけ（READ_ATTRIBUTES）のハンドルでも読める（R21）。読めなければ Err（呼ぶ側が、読めないときの扱いを決める）。
/// `file_id` とまとめない：ID を読めないファイルシステム（FAT 系の見込み）でも、リンクの数は読めるため。
pub fn link_count(handle: &impl AsHandle) -> io::Result<u32> {
    let mut info = ffi::FileStandardInfo {
        allocation_size: 0,
        end_of_file: 0,
        number_of_links: 0,
        delete_pending: 0,
        directory: 0,
    };
    // FILE_STANDARD_INFO の大きさ（24。テストで確かめた）
    let size = size_of::<ffi::FileStandardInfo>() as u32;
    // SAFETY: ハンドルは呼ぶあいだ借りている有効なもの。`info` は FILE_STANDARD_INFO と同じ形（#[repr(C)]、
    // 大きさと位置をテストで確かめた）の、すべての項目を 0 で初めた `size` バイトの書ける領域で、OS は `size` バイトを
    // 超えて書かない。どのバイトの並びも i64・u32・u8 として正しい値になる（BOOLEAN を `bool` にしていないので、
    // OS が 0・1 以外を書いても正しい値のまま）。
    let ok = unsafe {
        ffi::GetFileInformationByHandleEx(
            handle.as_handle().as_raw_handle(),
            ffi::CLASS_STANDARD_INFO,
            (&raw mut info).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.number_of_links)
}
