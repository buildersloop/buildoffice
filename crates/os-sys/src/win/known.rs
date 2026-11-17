//! 既知のフォルダ（W5。SHGetKnownFolderPath・CoTaskMemFree）。
//!
//! 守る場所（ホーム・AppData など）を、環境変数だけでなく OS の値でも決めるために使う（環境変数は、プロセスを
//! 起動する側がすべて差し替えられる）。flags は KF_FLAG_DONT_VERIFY（設計の決定：ネットワークの場所で待たない、
//! まだないフォルダの場所も返す）。
//!
//! hToken は、自分のプロセスのトークン（B1。OpenProcessToken）。NULL を渡すと、場所の値の中の `%USERPROFILE%`・
//! `%SystemDrive%` などをプロセスの環境変数で広げるので、子プロセスで環境変数を替えると AppData（Roaming・Local）
//! と ProgramData が変わり、SystemDrive を消すと ProgramData が絶対パスでなくなった（K1・K2・K3 の env_clear。
//! 2026-11-17）。自分のトークンを渡すと、トークンのユーザーの値で広げる。

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::PathBuf;
use std::ptr;

use super::ffi;

/// OS に問い合わせる既知のフォルダ（この8つだけ。設計の決定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownFolder {
    /// ユーザーのホーム（FOLDERID_Profile）。
    Profile,
    /// AppData\Roaming（FOLDERID_RoamingAppData）。
    RoamingAppData,
    /// AppData\Local（FOLDERID_LocalAppData）。
    LocalAppData,
    /// ドキュメント（FOLDERID_Documents）。ネットワークの場所へ移されていることがある。
    Documents,
    /// Windows のフォルダ（FOLDERID_Windows）。
    Windows,
    /// Program Files（FOLDERID_ProgramFiles。64 ビットのプロセスでは 64 ビットの場所）。
    ProgramFiles,
    /// Program Files (x86)（FOLDERID_ProgramFilesX86）。
    ProgramFilesX86,
    /// ProgramData（FOLDERID_ProgramData）。
    ProgramData,
}

impl KnownFolder {
    /// 8つすべて（決まった順）。
    pub const ALL: [Self; 8] = [
        Self::Profile,
        Self::RoamingAppData,
        Self::LocalAppData,
        Self::Documents,
        Self::Windows,
        Self::ProgramFiles,
        Self::ProgramFilesX86,
        Self::ProgramData,
    ];

    fn id(self) -> &'static ffi::Guid {
        match self {
            Self::Profile => &ffi::FOLDERID_PROFILE,
            Self::RoamingAppData => &ffi::FOLDERID_ROAMING_APP_DATA,
            Self::LocalAppData => &ffi::FOLDERID_LOCAL_APP_DATA,
            Self::Documents => &ffi::FOLDERID_DOCUMENTS,
            Self::Windows => &ffi::FOLDERID_WINDOWS,
            Self::ProgramFiles => &ffi::FOLDERID_PROGRAM_FILES,
            Self::ProgramFilesX86 => &ffi::FOLDERID_PROGRAM_FILES_X86,
            Self::ProgramData => &ffi::FOLDERID_PROGRAM_DATA,
        }
    }
}

/// 読む文字列の長さの上限（UTF-16 の単位。NUL を含めない）。これより先に NUL がなければ Err にする
/// （OS が返した文字列を、上限なく読み進めない）。
const MAX_UNITS: usize = 32_768;

/// 既知のフォルダの場所を OS に問い合わせる（W5）。
///
/// - flags は KF_FLAG_DONT_VERIFY、hToken は自分のプロセスのトークン（B1。開けなければ Err）。フォルダは
///   作らない（KF_FLAG_CREATE を付けない）。
/// - 成功は、HRESULT が 0 以上で、返った文字列が絶対パスのときだけ。HRESULT が FACILITY_WIN32 の失敗
///   （0x8007xxxx）なら Win32 の番号のエラー、ほかの失敗は値を文に入れたエラー。
/// - OS が返した文字列は、成功でも失敗でも1回だけ放す（CoTaskMemFree）。
/// - エラーの文にパスは入れない。
pub fn known_folder(folder: KnownFolder) -> io::Result<PathBuf> {
    let token = own_token()?;
    let mut raw: *mut u16 = ptr::null_mut();
    // SAFETY: rfid は 'static の GUID（ffi.rs の定数）を指す。flags は定数。hToken は、この関数が持っている
    // 自分のプロセスのトークン（TOKEN_QUERY・TOKEN_IMPERSONATE で開いた。呼ぶあいだ閉じない）。`raw` は書ける
    // `*mut u16` の置き場で、OS はそこへポインタを1つ書く（_Outptr_）。
    let hr = unsafe {
        ffi::SHGetKnownFolderPath(
            folder.id(),
            ffi::KF_FLAG_DONT_VERIFY,
            token.as_raw_handle(),
            &raw mut raw,
        )
    };
    // 失敗のときもポインタが返ることがあるので、先に持つ（Drop で放す）
    let returned = CoTaskString(raw);
    if hr < 0 {
        return Err(hresult_error(hr));
    }
    let units = returned.copy_units()?;
    let path = PathBuf::from(OsString::from_wide(&units));
    if !path.is_absolute() {
        return Err(io::Error::other(
            "OS が返した既知のフォルダの場所が、絶対パスでない",
        ));
    }
    Ok(path)
}

/// 自分のプロセスのトークンを、TOKEN_QUERY・TOKEN_IMPERSONATE で開く（B1）。閉じるのは OwnedHandle。
fn own_token() -> io::Result<OwnedHandle> {
    // SAFETY: 引数を受け取らず、今のプロセスの仮のハンドル（-1）を返すだけ。仮のハンドルは閉じなくてよく、
    // 閉じない（OwnedHandle に入れない）。hToken にはこれを渡さない（-1 は Default User の意味）。
    let process = unsafe { ffi::GetCurrentProcess() };
    let mut token: RawHandle = ptr::null_mut();
    // SAFETY: `process` は今のプロセスの仮のハンドル（いつでも有効）。access は定数。`token` は書ける置き場で、
    // OS は成功したときだけ、そこへハンドルを1つ書く（_Outptr_）。
    let ok = unsafe {
        ffi::OpenProcessToken(
            process,
            ffi::TOKEN_QUERY | ffi::TOKEN_IMPERSONATE,
            &raw mut token,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 成功したので、`token` はこの呼び出しで開いた、ほかに持ち主のないハンドル。OwnedHandle が1回だけ
    // 閉じる。
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

/// OS が返した文字列（CoTaskMemAlloc の領域）。Drop で1回だけ放す。
///
/// 作るのは `known_folder` の中だけで、外へ出さない。Clone・Copy はしないので、放すのは Drop の1回だけ
/// （成功でも、エラーで戻っても、panic でも）。
struct CoTaskString(*mut u16);

impl CoTaskString {
    /// NUL の前までの文字を写す。`MAX_UNITS` 個までに NUL がなければ Err。NULL なら Err。
    fn copy_units(&self) -> io::Result<Vec<u16>> {
        if self.0.is_null() {
            return Err(io::Error::other("OS が既知のフォルダの場所を返さなかった"));
        }
        let mut units = Vec::new();
        while units.len() < MAX_UNITS {
            // 位置を求めるだけ（読まない）なので、安全な wrapping_add で足す
            let at = self.0.wrapping_add(units.len());
            // SAFETY: `self.0` は、OS が返した NUL で終わる文字列の先頭（_Outptr_ PWSTR）で、この値が生きている
            // あいだは放さない。ここまでに読んだ `units.len()` 個の中に NUL はなかったので、`at`（`units.len()`
            // 番目）はまだ文字列（か、終わりの NUL）の中にある。領域の揃い方に頼らないように read_unaligned で読む。
            let unit = unsafe { at.read_unaligned() };
            if unit == 0 {
                return Ok(units);
            }
            units.push(unit);
        }
        Err(io::Error::other(
            "OS が返した既知のフォルダの場所が長すぎる",
        ))
    }
}

impl Drop for CoTaskString {
    fn drop(&mut self) {
        // SAFETY: `self.0` は SHGetKnownFolderPath が返したポインタ（NULL のこともある。CoTaskMemFree は NULL も
        // 受ける＝_Frees_ptr_opt_）。この型は複製できず、放すのはこの Drop の1回だけ。放したあとは使わない。
        unsafe { ffi::CoTaskMemFree(self.0.cast()) }
    }
}

/// HRESULT（負の値＝失敗）を io::Error に直す。FACILITY_WIN32 の失敗（0x8007xxxx。HRESULT_FROM_WIN32 で
/// 作った値）は Win32 の番号 xxxx に、ほかは値を文に入れる。
fn hresult_error(hr: i32) -> io::Error {
    let bits = hr.cast_unsigned();
    if bits & 0xFFFF_0000 == 0x8000_0000 | (ffi::FACILITY_WIN32 << 16) {
        // 下の 16 ビット（0〜0xFFFF）は、i32 にそのまま収まる
        io::Error::from_raw_os_error((bits & 0xFFFF) as i32)
    } else {
        io::Error::other(format!("OS が HRESULT 0x{bits:08X} を返した"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HRESULT の直し方：0x8007xxxx は Win32 の番号に、ほかは値を文に入れる。
    #[test]
    fn hresult_errors() {
        // HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)
        let e = hresult_error(0x8007_0002_u32.cast_signed());
        assert_eq!(e.raw_os_error(), Some(2));
        // HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)
        let e = hresult_error(0x8007_0005_u32.cast_signed());
        assert_eq!(e.raw_os_error(), Some(5));
        // E_FAIL（FACILITY_NULL）
        let e = hresult_error(0x8000_4005_u32.cast_signed());
        assert_eq!(e.raw_os_error(), None);
        assert!(e.to_string().contains("0x80004005"), "{e}");
        // E_INVALIDARG（0x80070057。HRESULT_FROM_WIN32(ERROR_INVALID_PARAMETER) と同じ値）
        let e = hresult_error(0x8007_0057_u32.cast_signed());
        assert_eq!(e.raw_os_error(), Some(87));
        // 0x8008xxxx（ほかの FACILITY）は番号に直さない
        let e = hresult_error(0x8008_0002_u32.cast_signed());
        assert_eq!(e.raw_os_error(), None);
    }

    /// 8つの ID はどれも違う（取り違えて同じ ID を2つの名前に付けていない）。
    #[test]
    fn every_folder_has_its_own_id() {
        for (i, a) in KnownFolder::ALL.iter().enumerate() {
            for b in &KnownFolder::ALL[i + 1..] {
                assert_ne!(a.id(), b.id(), "{a:?} と {b:?}");
            }
        }
    }
}
