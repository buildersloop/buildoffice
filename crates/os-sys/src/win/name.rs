//! 単一の名前の型。
//!
//! FILE_RENAME_INFO や NtCreateFile に渡す名前が、`\` や `\??\` で始まる完全なパス・ドライブ文字のパス・
//! 別のデータの流れ（`名前:流れ`）だと、親のハンドル（RootDirectory）が無視されたり、別の物を指したりする。
//! この型は、それらを作れなくする。保管庫の名前の規則（予約名・末尾の点など）は、vault-core が別に確かめる。

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

/// 単一の名前（パスの1つの要素）。
///
/// 条件：空でない・UTF-16 で 255 単位以下・NUL と `\` と `/` と `:` を含まない・`.` と `..` でない。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Name(Vec<u16>);

/// [`Name::new`] で断った理由。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidName {
    /// 空の名前。
    Empty,
    /// UTF-16 で 255 単位を超える。
    TooLong,
    /// NUL を含む。
    Nul,
    /// `\`・`/`・`:` を含む（パスの区切り・ドライブ・別のデータの流れになる）。
    Separator,
    /// `.` か `..`。
    Dots,
}

impl fmt::Display for InvalidName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Empty => "名前が空",
            Self::TooLong => "名前が長すぎる（UTF-16 で 255 を超える）",
            Self::Nul => "名前に NUL がある",
            Self::Separator => "名前に \\ か / か : がある",
            Self::Dots => "名前が . か ..",
        };
        f.write_str(text)
    }
}

impl std::error::Error for InvalidName {}

impl Name {
    /// 名前の長さの上限（UTF-16 の単位の数。NTFS の1つの要素の上限）。
    pub const MAX_UNITS: usize = 255;

    /// 条件を確かめて作る。
    pub fn new(name: &OsStr) -> Result<Self, InvalidName> {
        let wide: Vec<u16> = name.encode_wide().collect();
        if wide.is_empty() {
            return Err(InvalidName::Empty);
        }
        if wide.len() > Self::MAX_UNITS {
            return Err(InvalidName::TooLong);
        }
        if wide.contains(&0) {
            return Err(InvalidName::Nul);
        }
        if wide
            .iter()
            .any(|&c| c == u16::from(b'\\') || c == u16::from(b'/') || c == u16::from(b':'))
        {
            return Err(InvalidName::Separator);
        }
        let dot = u16::from(b'.');
        if wide == [dot] || wide == [dot, dot] {
            return Err(InvalidName::Dots);
        }
        Ok(Self(wide))
    }

    /// 名前を OsString で返す。
    pub fn to_os_string(&self) -> OsString {
        OsString::from_wide(&self.0)
    }

    /// UTF-16 の並び（NUL なし。1〜255 単位）。
    pub(crate) fn wide(&self) -> &[u16] {
        &self.0
    }

    /// UTF-16 のバイト数（NUL なし。2〜510）。
    pub(crate) fn byte_len(&self) -> usize {
        // 255 単位以下なので、あふれない
        self.0.len() * 2
    }
}
