//! ファイルの操作の共通の部品：使用中・拒否・ディスクがいっぱいの見分け（試し直しの回数と間隔）、種類と版。
//! 開く・作る・移す・消す処理は `guard`（ハンドルで確かめる形）にある。

use std::fs::Metadata;
use std::io;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, VaultError};
use crate::time;

/// 一時ファイルを置くフォルダ（`.vault/tmp`）。
pub(crate) const TMP_DIR: &str = "tmp";
/// 使用中で置き換えられないときに試す回数（100ms おきに3回まで）。
pub(crate) const BUSY_ATTEMPTS: u32 = 3;
/// 試す間隔。
pub(crate) const BUSY_INTERVAL: Duration = Duration::from_millis(100);

/// 一覧・索引・ゴミ箱での種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Link,
}

/// `symlink_metadata` から読んだ、種類・大きさ・更新時刻（ナノ秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileInfo {
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ns: i64,
}

impl FileInfo {
    /// リンクはたどらない（`symlink_metadata` の値をそのまま使う）。フォルダの大きさは 0。
    pub fn from_metadata(meta: &Metadata) -> Self {
        let kind = if meta.file_type().is_symlink() {
            EntryKind::Link
        } else if meta.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let size = if kind == EntryKind::Dir {
            0
        } else {
            meta.len()
        };
        let mtime_ns = meta.modified().map(unix_nanos).unwrap_or(0);
        Self {
            kind,
            size,
            mtime_ns,
        }
    }

    /// 版（`"<size>-<mtime_ns>"`）。
    pub fn version(&self) -> String {
        format!("{}-{}", self.size, self.mtime_ns)
    }

    /// 更新時刻（RFC 3339・UTC・ミリ秒）。年が 0000〜9999 の外なら空の文字。
    pub fn modified(&self) -> String {
        time::format_unix_millis(self.mtime_ns.div_euclid(1_000_000)).unwrap_or_default()
    }
}

fn unix_nanos(t: std::time::SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_nanos())
            .map(|v| -v)
            .unwrap_or(i64::MIN),
    }
}

/// 使用中のエラーか（Windows：共有違反 32・ロック違反 33）。
pub(crate) fn is_busy(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::ResourceBusy
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)))
}

/// アクセス拒否のエラーか（Windows：5。ほかのアプリが開いているファイルを置き換えるときにも出るので、何度か試す）。
pub(crate) fn is_access_denied(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied || (cfg!(windows) && e.raw_os_error() == Some(5))
}

/// `ACCESS_DENIED` のエラー。
pub(crate) fn access_denied(message: &str) -> VaultError {
    VaultError::new(ErrorCode::AccessDenied, message)
}

/// ディスクがいっぱいのエラーか（Windows：112・39 も）。
fn is_disk_full(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(39 | 112)))
}

pub(crate) fn io_or_disk_full(what: &str, e: &io::Error) -> VaultError {
    if is_disk_full(e) {
        VaultError::new(
            ErrorCode::DiskFull,
            "ディスクの空きが足りない（元のファイルには触れていない）",
        )
    } else {
        VaultError::io(what, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_modified_formats() {
        let info = FileInfo {
            kind: EntryKind::File,
            size: 12,
            mtime_ns: 1_000_000_000_123_456_789,
        };
        assert_eq!(info.version(), "12-1000000000123456789");
        assert_eq!(info.modified(), "2001-09-09T01:46:40.123Z");
    }

    #[test]
    fn busy_and_disk_full_errors_are_recognized() {
        assert!(is_disk_full(&io::Error::from(io::ErrorKind::StorageFull)));
        assert!(!is_disk_full(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(is_busy(&io::Error::from(io::ErrorKind::ResourceBusy)));
        assert!(is_access_denied(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        if cfg!(windows) {
            for code in [32, 33] {
                assert!(is_busy(&io::Error::from_raw_os_error(code)), "{code}");
            }
            assert!(!is_busy(&io::Error::from_raw_os_error(5)));
            assert!(is_access_denied(&io::Error::from_raw_os_error(5)));
            assert!(is_disk_full(&io::Error::from_raw_os_error(112)));
        }
        let e = io_or_disk_full("x", &io::Error::from(io::ErrorKind::StorageFull));
        assert_eq!(e.code(), ErrorCode::DiskFull);
    }

    #[test]
    fn busy_timing_matches_the_adr() {
        assert_eq!(BUSY_ATTEMPTS, 3);
        assert_eq!(BUSY_INTERVAL, Duration::from_millis(100));
    }

    #[test]
    fn entry_kinds_serialize_like_the_adr() {
        assert_eq!(serde_json::to_value(EntryKind::File).unwrap(), "file");
        assert_eq!(serde_json::to_value(EntryKind::Dir).unwrap(), "dir");
        assert_eq!(serde_json::to_value(EntryKind::Link).unwrap(), "link");
    }
}
