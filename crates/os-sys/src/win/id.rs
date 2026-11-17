//! 開いたハンドルのファイルの ID（W1。GetFileInformationByHandleEx の FileIdInfo）。

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
