//! OS の関数の宣言・定数・構造体（Windows・x64）。
//!
//! 宣言は自分で書いた。各項に、突き合わせた SDK のヘッダ
//! （Windows Kits 10.0.26100.0 の Include の下）のファイルと行を書く。ヘッダの文は写さない。
//!
//! 型の対応：HANDLE は `RawHandle`（`*mut c_void`）、BOOL・NTSTATUS・C の enum は `i32`、
//! DWORD・ULONG・ACCESS_MASK は `u32`、USHORT は `u16`、BOOLEAN・BYTE は `u8`、LARGE_INTEGER は `i64`、
//! ULONG_PTR は `usize`。edition 2024 なので `unsafe extern "system"` と書く。

use std::ffi::c_void;
use std::mem::offset_of;
use std::os::windows::io::RawHandle;

// ---------------------------------------------------------------------------
// 関数
// ---------------------------------------------------------------------------

// SAFETY: 宣言だけ。名前・引数の数と型・返り値・呼び出し規約（WINAPI＝"system"）を、下の各行の
// ヘッダと突き合わせた。呼ぶ所ごとに、引数の決まりを満たしていることを SAFETY に書く。
#[link(name = "kernel32")]
unsafe extern "system" {
    /// GetFinalPathNameByHandleW（um\fileapi.h:712）。DWORD (HANDLE, LPWSTR, DWORD, DWORD)。
    pub(crate) fn GetFinalPathNameByHandleW(
        file: RawHandle,
        path: *mut u16,
        cch: u32,
        flags: u32,
    ) -> u32;

    /// GetFileInformationByHandleEx（um\WinBase.h:9411）。BOOL (HANDLE, FILE_INFO_BY_HANDLE_CLASS, LPVOID, DWORD)。
    pub(crate) fn GetFileInformationByHandleEx(
        file: RawHandle,
        class: i32,
        info: *mut c_void,
        size: u32,
    ) -> i32;

    /// SetFileInformationByHandle（um\fileapi.h:1079）。BOOL (HANDLE, FILE_INFO_BY_HANDLE_CLASS, LPVOID, DWORD)。
    pub(crate) fn SetFileInformationByHandle(
        file: RawHandle,
        class: i32,
        info: *mut c_void,
        size: u32,
    ) -> i32;

    /// GetDiskFreeSpaceExW（um\fileapi.h:430）。BOOL (LPCWSTR, PULARGE_INTEGER, PULARGE_INTEGER, PULARGE_INTEGER)。
    /// ULARGE_INTEGER は 8 バイトの値なので `*mut u64` で受ける。
    pub(crate) fn GetDiskFreeSpaceExW(
        dir: *const u16,
        avail_to_caller: *mut u64,
        total: *mut u64,
        total_free: *mut u64,
    ) -> i32;

    /// GetCurrentProcess（um\processthreadsapi.h:148）。HANDLE (VOID)。今のプロセスの仮のハンドル（-1）を返す。
    /// 閉じない（OwnedHandle に入れない）。SHGetKnownFolderPath の hToken にそのまま渡さない（-1 は Default
    /// User の意味）。
    pub(crate) fn GetCurrentProcess() -> RawHandle;
}

// SAFETY: 宣言だけ（上と同じ）。advapi32.dll は OS の DLL。
#[link(name = "advapi32")]
unsafe extern "system" {
    /// OpenProcessToken（um\processthreadsapi.h:538〜542）。BOOL (HANDLE, DWORD, PHANDLE)。TokenHandle は _Outptr_。
    /// 開いたトークンは閉じる（OwnedHandle に入れる）。
    pub(crate) fn OpenProcessToken(process: RawHandle, access: u32, token: *mut RawHandle) -> i32;
}

// SAFETY: 宣言だけ（上と同じ）。
#[link(name = "bcrypt")]
unsafe extern "system" {
    /// BCryptGenRandom（shared\bcrypt.h:1663）。NTSTATUS (BCRYPT_ALG_HANDLE＝PVOID, PUCHAR, ULONG, ULONG)。
    pub(crate) fn BCryptGenRandom(alg: *mut c_void, buf: *mut u8, len: u32, flags: u32) -> i32;
}

// SAFETY: 宣言だけ（上と同じ）。ntdll.dll はどのプロセスにも読み込まれている。
// NtCreateFile と RtlNtStatusToDosError は winternl.h と突き合わせた。NtSetInformationFile は手元の SDK に
// なく、形と番号は WDK の公開の文書と実機の R（R1〜R6f・R14c・R15〜R18）による。
#[link(name = "ntdll")]
unsafe extern "system" {
    /// NtCreateFile（um\winternl.h:475）。NTSTATUS (PHANDLE, ACCESS_MASK, POBJECT_ATTRIBUTES, PIO_STATUS_BLOCK,
    /// PLARGE_INTEGER, ULONG, ULONG, ULONG, ULONG, PVOID, ULONG)。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn NtCreateFile(
        handle: *mut RawHandle,
        access: u32,
        attributes: *mut ObjectAttributes,
        io_status: *mut IoStatusBlock,
        allocation_size: *mut i64,
        file_attributes: u32,
        share: u32,
        disposition: u32,
        options: u32,
        ea: *mut c_void,
        ea_length: u32,
    ) -> i32;

    /// RtlNtStatusToDosError（um\winternl.h:636）。ULONG (NTSTATUS)。
    pub(crate) fn RtlNtStatusToDosError(status: i32) -> u32;

    /// NtSetInformationFile。**手元の SDK（Include\10.0.26100.0）のヘッダにはない**。
    /// 形は WDK（ntifs.h）の公開の文書による：NTSTATUS (HANDLE FileHandle, PIO_STATUS_BLOCK IoStatusBlock,
    /// PVOID FileInformation, ULONG Length, FILE_INFORMATION_CLASS FileInformationClass)。ntdll.lib に
    /// `__imp_NtSetInformationFile` がある。実機の R1〜R6f・R14c・R15〜R18（2026-11-16）で確かめた。
    pub(crate) fn NtSetInformationFile(
        file: RawHandle,
        io_status: *mut IoStatusBlock,
        info: *mut c_void,
        length: u32,
        class: i32,
    ) -> i32;
}

// SAFETY: 宣言だけ（上と同じ）。shell32.dll は OS の DLL。STDAPI は
// EXTERN_C HRESULT STDAPICALLTYPE（shared\basetyps.h:44。x64 では呼び出し規約は1つ）。
#[link(name = "shell32")]
unsafe extern "system" {
    /// SHGetKnownFolderPath（um\ShlObj_core.h:1046〜1049）。HRESULT (REFKNOWNFOLDERID, DWORD, HANDLE, PWSTR*)。
    /// REFKNOWNFOLDERID は C では `const KNOWNFOLDERID *`（KNOWNFOLDERID は GUID）。hToken は _In_opt_。
    /// 返った文字列は CoTaskMemFree で放す（1049 行の注）。
    pub(crate) fn SHGetKnownFolderPath(
        rfid: *const Guid,
        flags: u32,
        token: RawHandle,
        path: *mut *mut u16,
    ) -> i32;
}

// SAFETY: 宣言だけ（上と同じ）。ole32.dll は OS の DLL。
#[link(name = "ole32")]
unsafe extern "system" {
    /// CoTaskMemFree（um\combaseapi.h:1408〜1411）。void (LPVOID)。_Frees_ptr_opt_ なので NULL も受ける。
    pub(crate) fn CoTaskMemFree(pv: *mut c_void);
}

// ---------------------------------------------------------------------------
// 情報の種類（um\minwinbase.h:178〜211 の FILE_INFO_BY_HANDLE_CLASS。並びの番号）
// ---------------------------------------------------------------------------

/// FileDispositionInfo（minwinbase.h:183）。
pub(crate) const CLASS_DISPOSITION_INFO: i32 = 4;
/// FileFullDirectoryInfo（minwinbase.h:193）。
pub(crate) const CLASS_FULL_DIRECTORY_INFO: i32 = 14;
/// FileFullDirectoryRestartInfo（minwinbase.h:194）。
pub(crate) const CLASS_FULL_DIRECTORY_RESTART_INFO: i32 = 15;
/// FileIdInfo（minwinbase.h:198）。
pub(crate) const CLASS_ID_INFO: i32 = 18;
/// FileIdExtdDirectoryInfo（minwinbase.h:199）。
pub(crate) const CLASS_ID_EXTD_DIRECTORY_INFO: i32 = 19;
/// FileIdExtdDirectoryRestartInfo（minwinbase.h:200）。
pub(crate) const CLASS_ID_EXTD_DIRECTORY_RESTART_INFO: i32 = 20;
/// FileDispositionInfoEx（minwinbase.h:203）。
pub(crate) const CLASS_DISPOSITION_INFO_EX: i32 = 21;

// NtSetInformationFile の FILE_INFORMATION_CLASS（手元の SDK にない。WDK の wdm.h・ntifs.h の公開の文書による。
// 構造体 FILE_RENAME_INFORMATION の並びは、WinBase.h:9137 の FILE_RENAME_INFO と同じ）

/// FileRenameInformation（ReplaceIfExists は先頭の BOOLEAN）。
pub(crate) const NT_CLASS_RENAME_INFORMATION: i32 = 10;
/// FileRenameInformationEx（先頭は ULONG の Flags。FILE_RENAME_FLAG_* と同じ値）。
pub(crate) const NT_CLASS_RENAME_INFORMATION_EX: i32 = 65;

// ---------------------------------------------------------------------------
// 定数
// ---------------------------------------------------------------------------

/// KF_FLAG_DONT_VERIFY（um\ShlObj_core.h:1002）。フォルダがあるかを確かめない（ネットワークの場所で待たない。
/// まだないフォルダの場所も返す）。設計の決定。KF_FLAG_DEFAULT（948 行）は 0。
/// KF_FLAG_CREATE（994 行）は使わない（フォルダを作らせない）。
pub(crate) const KF_FLAG_DONT_VERIFY: u32 = 0x0000_4000;

/// FACILITY_WIN32（shared\winerror.h:66）。HRESULT_FROM_WIN32（30354 行）は、Win32 の番号を
/// `0x8000_0000 | (FACILITY_WIN32 << 16) | 番号` にする。
pub(crate) const FACILITY_WIN32: u32 = 7;

/// TOKEN_IMPERSONATE・TOKEN_QUERY（um\winnt.h:11990・11991）。SHGetKnownFolderPath に渡すトークンに要る権利
/// （公開の文書による）。
pub(crate) const TOKEN_IMPERSONATE: u32 = 0x0004;
pub(crate) const TOKEN_QUERY: u32 = 0x0008;

/// GetFinalPathNameByHandleW の flags：FILE_NAME_NORMALIZED（WinBase.h:748）と VOLUME_NAME_DOS（743）。どちらも 0。
pub(crate) const FINAL_PATH_FLAGS: u32 = 0;

/// FILE_FLAG_BACKUP_SEMANTICS（WinBase.h:145）。std でフォルダを開くときに要る。
pub(crate) const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// FILE_FLAG_OPEN_REPARSE_POINT（WinBase.h:148）。std で、リンクをたどらずに開く。
pub(crate) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// FILE_RENAME_FLAG_REPLACE_IF_EXISTS（WinBase.h:9129）。
pub(crate) const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x1;
/// FILE_RENAME_FLAG_POSIX_SEMANTICS（WinBase.h:9130）。
pub(crate) const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x2;
// 使わないもの：FILE_RENAME_FLAG_IGNORE_READONLY_ATTRIBUTE（読み取り専用の索引の置き換えを、今の std と同じく
// 失敗させるため。vault-core の write_safety.rs のテスト）。

/// FILE_DISPOSITION_FLAG_DELETE（WinBase.h:9187）。
pub(crate) const FILE_DISPOSITION_FLAG_DELETE: u32 = 0x1;
/// FILE_DISPOSITION_FLAG_POSIX_SEMANTICS（WinBase.h:9188）。
pub(crate) const FILE_DISPOSITION_FLAG_POSIX_SEMANTICS: u32 = 0x2;
/// FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE（WinBase.h:9192）。
pub(crate) const FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE: u32 = 0x10;

/// SYNCHRONIZE（um\winnt.h:10224）。FILE_SYNCHRONOUS_IO_NONALERT に要る。os-sys がいつも付ける。
pub(crate) const SYNCHRONIZE: u32 = 0x0010_0000;
/// DELETE（winnt.h:10220）。
pub(crate) const DELETE: u32 = 0x0001_0000;
/// FILE_READ_DATA・FILE_LIST_DIRECTORY（winnt.h:16061・16062）。
pub(crate) const FILE_READ_DATA: u32 = 0x0001;
/// FILE_WRITE_DATA・FILE_ADD_FILE（winnt.h:16064・16065）。
pub(crate) const FILE_WRITE_DATA: u32 = 0x0002;
/// FILE_APPEND_DATA（winnt.h:16067）。
pub(crate) const FILE_APPEND_DATA: u32 = 0x0004;
/// FILE_TRAVERSE（winnt.h:16077）。
pub(crate) const FILE_TRAVERSE: u32 = 0x0020;
/// FILE_READ_ATTRIBUTES（winnt.h:16081）。ハンドルで種類を確かめるのに要るので、os-sys がいつも付ける。
pub(crate) const FILE_READ_ATTRIBUTES: u32 = 0x0080;
/// FILE_WRITE_ATTRIBUTES（winnt.h:16083）。
pub(crate) const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;

/// FILE_SHARE_READ・WRITE・DELETE（winnt.h:16107〜16109）。
pub(crate) const FILE_SHARE_READ: u32 = 0x1;
pub(crate) const FILE_SHARE_WRITE: u32 = 0x2;
pub(crate) const FILE_SHARE_DELETE: u32 = 0x4;

/// FILE_ATTRIBUTE_DIRECTORY（winnt.h:16113）。
pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// FILE_ATTRIBUTE_NORMAL（winnt.h:16116）。NtCreateFile の FileAttributes に渡す。
pub(crate) const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
/// FILE_ATTRIBUTE_REPARSE_POINT（winnt.h:16119）。
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// 名前の代理の印（winnt.h:16543 の IsReparseTagNameSurrogate が見るビット）。ジャンクション・シンボリックリンクに付く。
pub(crate) const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;

/// OBJECT_ATTRIBUTES の Attributes（um\winternl.h:973〜982）。
/// OBJ_INHERIT（973 行）は使わないことを確かめるためにだけ置く（子プロセスにハンドルを継がせない）。
#[cfg(test)]
pub(crate) const OBJ_INHERIT: u32 = 0x0000_0002;
/// OBJ_CASE_INSENSITIVE（winternl.h:976）。Win32 の CreateFileW と同じく、大文字小文字を区別しない。
pub(crate) const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
/// OBJ_DONT_REPARSE（winternl.h:982）。名前を解くあいだにリパースポイントに当たったら、たどらずに失敗する。
pub(crate) const OBJ_DONT_REPARSE: u32 = 0x0000_1000;

/// NtCreateFile の CreateDisposition（winternl.h:990・991）。
pub(crate) const FILE_OPEN: u32 = 0x1;
pub(crate) const FILE_CREATE: u32 = 0x2;

/// NtCreateFile の CreateOptions（winternl.h:1001〜1026）。
pub(crate) const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
pub(crate) const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
pub(crate) const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
pub(crate) const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
// 使わないもの（呼ぶ側にも渡させない）：FILE_DELETE_ON_CLOSE 0x1000・FILE_OPEN_BY_FILE_ID 0x2000・
// FILE_OPEN_FOR_BACKUP_INTENT 0x4000（winternl.h:1016〜1018）。テストで、組んだ値に入らないことを確かめる。
#[cfg(test)]
pub(crate) const FORBIDDEN_CREATE_OPTIONS: u32 = 0x1000 | 0x2000 | 0x4000;

/// NTSTATUS の値（shared\ntstatus.h の 295・2094・2184・3808 行）。
/// STATUS_PENDING：同期の I/O のハンドルでは返らない。返ったら、OS があとで IO_STATUS_BLOCK に書くおそれが
/// あるので、呼んだ関数から戻らずに、すぐ止める（info.rs。std は完了を待ってから止める）。
pub(crate) const STATUS_PENDING: i32 = 0x0000_0103;
/// FileRenameInformationEx を受け付けないときの値（STATUS_INVALID_PARAMETER・STATUS_NOT_SUPPORTED・
/// STATUS_INVALID_INFO_CLASS）。
pub(crate) const STATUS_INVALID_PARAMETER: i32 = 0xC000_000D_u32 as i32;
pub(crate) const STATUS_NOT_SUPPORTED: i32 = 0xC000_00BB_u32 as i32;
pub(crate) const STATUS_INVALID_INFO_CLASS: i32 = 0xC000_0003_u32 as i32;

/// IO_STATUS_BLOCK の Information（winternl.h:1040・1041）。
pub(crate) const FILE_OPENED: usize = 1;
pub(crate) const FILE_CREATED: usize = 2;

/// BCRYPT_USE_SYSTEM_PREFERRED_RNG（shared\bcrypt.h:1658）。
pub(crate) const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x2;

/// Win32 のエラーの番号（shared\winerror.h の 243・396・577・838・14189 行）。
pub(crate) const ERROR_INVALID_FUNCTION: i32 = 1;
pub(crate) const ERROR_NO_MORE_FILES: i32 = 18;
pub(crate) const ERROR_NOT_SUPPORTED: i32 = 50;
pub(crate) const ERROR_INVALID_PARAMETER: i32 = 87;
/// ERROR_REPARSE_POINT_ENCOUNTERED（STATUS_REPARSE_POINT_ENCOUNTERED を直した値）。
pub(crate) const ERROR_REPARSE_POINT_ENCOUNTERED: i32 = 4395;

// ---------------------------------------------------------------------------
// 構造体（#[repr(C)]。x64 の大きさと項目の位置を、下のテストで確かめる）
// ---------------------------------------------------------------------------

/// UNICODE_STRING（um\winternl.h:70）。Length と MaximumLength はバイト数（NUL を含めない）。
#[repr(C)]
pub(crate) struct UnicodeString {
    pub(crate) length: u16,
    pub(crate) maximum_length: u16,
    pub(crate) buffer: *mut u16,
}

/// OBJECT_ATTRIBUTES（um\winternl.h:213）。
#[repr(C)]
pub(crate) struct ObjectAttributes {
    pub(crate) length: u32,
    pub(crate) root_directory: RawHandle,
    pub(crate) object_name: *mut UnicodeString,
    pub(crate) attributes: u32,
    pub(crate) security_descriptor: *mut c_void,
    pub(crate) security_quality_of_service: *mut c_void,
}

/// OBJECT_ATTRIBUTES の Length に入れる値（x64 の size_of。テストで確かめる）。
pub(crate) const OBJECT_ATTRIBUTES_LENGTH: u32 = 48;

/// IO_STATUS_BLOCK（um\winternl.h:223）。先頭の union（NTSTATUS と PVOID）は、ポインタの大きさの `usize` で持つ。
#[repr(C)]
pub(crate) struct IoStatusBlock {
    pub(crate) status: usize,
    pub(crate) information: usize,
}

/// FILE_RENAME_INFO（um\WinBase.h:9137）。先頭の union（BOOLEAN ReplaceIfExists と DWORD Flags）は `u32` で持つ。
/// バイト列で組み立てる（info.rs）ので、この型は大きさと位置を確かめるためだけに使う。
#[repr(C)]
#[allow(dead_code)]
pub(crate) struct FileRenameInfo {
    pub(crate) flags: u32,
    pub(crate) root_directory: RawHandle,
    pub(crate) file_name_length: u32,
    pub(crate) file_name: [u16; 1],
}

pub(crate) const RENAME_FLAGS_AT: usize = offset_of!(FileRenameInfo, flags);
pub(crate) const RENAME_ROOT_AT: usize = offset_of!(FileRenameInfo, root_directory);
pub(crate) const RENAME_NAME_LENGTH_AT: usize = offset_of!(FileRenameInfo, file_name_length);
pub(crate) const RENAME_NAME_AT: usize = offset_of!(FileRenameInfo, file_name);

/// FILE_DISPOSITION_INFO（um\WinBase.h:9181）。BOOLEAN DeleteFile だけ。
#[repr(C)]
#[allow(dead_code)]
pub(crate) struct FileDispositionInfo {
    pub(crate) delete_file: u8,
}

/// FILE_DISPOSITION_INFO_EX（um\WinBase.h:9195）。DWORD Flags だけ。
#[repr(C)]
#[allow(dead_code)]
pub(crate) struct FileDispositionInfoEx {
    pub(crate) flags: u32,
}

/// FILE_ID_INFO（um\WinBase.h:9284）。FileId は FILE_ID_128（winnt.h:16185、BYTE の 16 個）。
#[repr(C)]
pub(crate) struct FileIdInfo {
    pub(crate) volume_serial_number: u64,
    pub(crate) file_id: [u8; 16],
}

/// FILE_FULL_DIR_INFO（um\WinBase.h:9218）。列挙のバッファは安全なコードでバイト列として読む（dir.rs）。
#[repr(C)]
#[allow(dead_code)]
pub(crate) struct FileFullDirInfo {
    pub(crate) next_entry_offset: u32,
    pub(crate) file_index: u32,
    pub(crate) creation_time: i64,
    pub(crate) last_access_time: i64,
    pub(crate) last_write_time: i64,
    pub(crate) change_time: i64,
    pub(crate) end_of_file: i64,
    pub(crate) allocation_size: i64,
    pub(crate) file_attributes: u32,
    pub(crate) file_name_length: u32,
    pub(crate) ea_size: u32,
    pub(crate) file_name: [u16; 1],
}

/// FILE_ID_EXTD_DIR_INFO（um\WinBase.h:9292）。FileId は FILE_ID_128（winnt.h:16185、BYTE の 16 個）。
#[repr(C)]
#[allow(dead_code)]
pub(crate) struct FileIdExtdDirInfo {
    pub(crate) next_entry_offset: u32,
    pub(crate) file_index: u32,
    pub(crate) creation_time: i64,
    pub(crate) last_access_time: i64,
    pub(crate) last_write_time: i64,
    pub(crate) change_time: i64,
    pub(crate) end_of_file: i64,
    pub(crate) allocation_size: i64,
    pub(crate) file_attributes: u32,
    pub(crate) file_name_length: u32,
    pub(crate) ea_size: u32,
    pub(crate) reparse_point_tag: u32,
    pub(crate) file_id: [u8; 16],
    pub(crate) file_name: [u16; 1],
}

// 列挙の項目を読む位置（2つの構造体で、FileName の前までの共通の部分は同じ位置。テストで確かめる）。
pub(crate) const DIR_NEXT_AT: usize = offset_of!(FileFullDirInfo, next_entry_offset);
pub(crate) const DIR_LAST_WRITE_AT: usize = offset_of!(FileFullDirInfo, last_write_time);
pub(crate) const DIR_END_OF_FILE_AT: usize = offset_of!(FileFullDirInfo, end_of_file);
pub(crate) const DIR_ATTRIBUTES_AT: usize = offset_of!(FileFullDirInfo, file_attributes);
pub(crate) const DIR_NAME_LENGTH_AT: usize = offset_of!(FileFullDirInfo, file_name_length);
pub(crate) const FULL_EA_SIZE_AT: usize = offset_of!(FileFullDirInfo, ea_size);
pub(crate) const FULL_NAME_AT: usize = offset_of!(FileFullDirInfo, file_name);
pub(crate) const ID_EXTD_TAG_AT: usize = offset_of!(FileIdExtdDirInfo, reparse_point_tag);
pub(crate) const ID_EXTD_FILE_ID_AT: usize = offset_of!(FileIdExtdDirInfo, file_id);
pub(crate) const ID_EXTD_NAME_AT: usize = offset_of!(FileIdExtdDirInfo, file_name);

// ---------------------------------------------------------------------------
// GUID と既知のフォルダの ID（W5）
// ---------------------------------------------------------------------------

/// GUID（shared\guiddef.h:22〜26）。unsigned long は Windows では 32 ビット。大きさ 16、位置 0・4・6・8。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Guid {
    pub(crate) data1: u32,
    pub(crate) data2: u16,
    pub(crate) data3: u16,
    pub(crate) data4: [u8; 8],
}

impl Guid {
    const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }
}

// FOLDERID の値（um\KnownFolders.h の DEFINE_KNOWN_FOLDER。行は各項に書く）。
/// FOLDERID_Profile（KnownFolders.h:150）。
pub(crate) const FOLDERID_PROFILE: Guid = Guid::new(
    0x5E6C_858F,
    0x0E22,
    0x4760,
    [0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73],
);
/// FOLDERID_RoamingAppData（KnownFolders.h:123）。
pub(crate) const FOLDERID_ROAMING_APP_DATA: Guid = Guid::new(
    0x3EB6_85DB,
    0x65F9,
    0x4CF6,
    [0xA0, 0x3A, 0xE3, 0xEF, 0x65, 0x72, 0x9F, 0x3D],
);
/// FOLDERID_LocalAppData（KnownFolders.h:126）。
pub(crate) const FOLDERID_LOCAL_APP_DATA: Guid = Guid::new(
    0xF1B3_2785,
    0x6FBA,
    0x4FCF,
    [0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91],
);
/// FOLDERID_Documents（KnownFolders.h:87）。
pub(crate) const FOLDERID_DOCUMENTS: Guid = Guid::new(
    0xFDD3_9AD0,
    0x238F,
    0x46AF,
    [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7],
);
/// FOLDERID_Windows（KnownFolders.h:147）。
pub(crate) const FOLDERID_WINDOWS: Guid = Guid::new(
    0xF38B_F404,
    0x1D43,
    0x42F2,
    [0x93, 0x05, 0x67, 0xDE, 0x0B, 0x28, 0xFC, 0x23],
);
/// FOLDERID_ProgramFiles（KnownFolders.h:168）。
pub(crate) const FOLDERID_PROGRAM_FILES: Guid = Guid::new(
    0x905E_63B6,
    0xC1BF,
    0x494E,
    [0xB2, 0x9C, 0x65, 0xB7, 0x32, 0xD3, 0xD2, 0x1A],
);
/// FOLDERID_ProgramFilesX86（KnownFolders.h:156）。
pub(crate) const FOLDERID_PROGRAM_FILES_X86: Guid = Guid::new(
    0x7C5A_40EF,
    0xA0FB,
    0x4BFC,
    [0x87, 0x4A, 0xC0, 0xF2, 0xE0, 0xB9, 0xFA, 0x8E],
);
/// FOLDERID_ProgramData（KnownFolders.h:114）。
pub(crate) const FOLDERID_PROGRAM_DATA: Guid = Guid::new(
    0x62AB_5D82,
    0xFDC1,
    0x4DC3,
    [0xA9, 0xDD, 0x07, 0x0D, 0x1D, 0x49, 0x5D, 0x97],
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    /// x64 の大きさと項目の位置（表。ヘッダの `#if` で形が変わる構造体があるため）。
    #[test]
    fn struct_sizes_and_offsets() {
        assert_eq!(size_of::<FileRenameInfo>(), 24);
        assert_eq!(RENAME_FLAGS_AT, 0);
        assert_eq!(RENAME_ROOT_AT, 8);
        assert_eq!(RENAME_NAME_LENGTH_AT, 16);
        assert_eq!(RENAME_NAME_AT, 20);

        assert_eq!(size_of::<FileDispositionInfo>(), 1);
        assert_eq!(offset_of!(FileDispositionInfo, delete_file), 0);
        assert_eq!(size_of::<FileDispositionInfoEx>(), 4);
        assert_eq!(offset_of!(FileDispositionInfoEx, flags), 0);

        assert_eq!(size_of::<FileIdInfo>(), 24);
        assert_eq!(offset_of!(FileIdInfo, volume_serial_number), 0);
        assert_eq!(offset_of!(FileIdInfo, file_id), 8);

        assert_eq!(size_of::<FileFullDirInfo>(), 72);
        assert_eq!(DIR_NEXT_AT, 0);
        assert_eq!(DIR_LAST_WRITE_AT, 24);
        assert_eq!(DIR_END_OF_FILE_AT, 40);
        assert_eq!(DIR_ATTRIBUTES_AT, 56);
        assert_eq!(DIR_NAME_LENGTH_AT, 60);
        assert_eq!(FULL_EA_SIZE_AT, 64);
        assert_eq!(FULL_NAME_AT, 68);

        assert_eq!(size_of::<FileIdExtdDirInfo>(), 96);
        assert_eq!(
            offset_of!(FileIdExtdDirInfo, next_entry_offset),
            DIR_NEXT_AT
        );
        assert_eq!(
            offset_of!(FileIdExtdDirInfo, last_write_time),
            DIR_LAST_WRITE_AT
        );
        assert_eq!(
            offset_of!(FileIdExtdDirInfo, end_of_file),
            DIR_END_OF_FILE_AT
        );
        assert_eq!(
            offset_of!(FileIdExtdDirInfo, file_attributes),
            DIR_ATTRIBUTES_AT
        );
        assert_eq!(
            offset_of!(FileIdExtdDirInfo, file_name_length),
            DIR_NAME_LENGTH_AT
        );
        assert_eq!(offset_of!(FileIdExtdDirInfo, ea_size), FULL_EA_SIZE_AT);
        assert_eq!(ID_EXTD_TAG_AT, 68);
        assert_eq!(ID_EXTD_FILE_ID_AT, 72);
        assert_eq!(ID_EXTD_NAME_AT, 88);

        assert_eq!(size_of::<ObjectAttributes>(), 48);
        assert_eq!(
            size_of::<ObjectAttributes>(),
            OBJECT_ATTRIBUTES_LENGTH as usize
        );
        assert_eq!(offset_of!(ObjectAttributes, length), 0);
        assert_eq!(offset_of!(ObjectAttributes, root_directory), 8);
        assert_eq!(offset_of!(ObjectAttributes, object_name), 16);
        assert_eq!(offset_of!(ObjectAttributes, attributes), 24);
        assert_eq!(offset_of!(ObjectAttributes, security_descriptor), 32);
        assert_eq!(
            offset_of!(ObjectAttributes, security_quality_of_service),
            40
        );

        assert_eq!(size_of::<UnicodeString>(), 16);
        assert_eq!(offset_of!(UnicodeString, length), 0);
        assert_eq!(offset_of!(UnicodeString, maximum_length), 2);
        assert_eq!(offset_of!(UnicodeString, buffer), 8);

        assert_eq!(size_of::<IoStatusBlock>(), 16);
        assert_eq!(offset_of!(IoStatusBlock, status), 0);
        assert_eq!(offset_of!(IoStatusBlock, information), 8);
    }

    /// GUID の大きさと項目の位置（guiddef.h:22〜26。x64）。
    #[test]
    fn guid_size_and_offsets() {
        assert_eq!(size_of::<Guid>(), 16);
        assert_eq!(offset_of!(Guid, data1), 0);
        assert_eq!(offset_of!(Guid, data2), 4);
        assert_eq!(offset_of!(Guid, data3), 6);
        assert_eq!(offset_of!(Guid, data4), 8);
    }

    /// GUID を、レジストリなどで使う形（`{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`）に書く。
    fn guid_text(g: &Guid) -> String {
        let d = g.data4;
        format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        )
    }

    /// FOLDERID の値を書いた形が、KnownFolders.h の値（KNOWNFOLDERID の文字列の形）と同じ。
    #[test]
    fn folder_ids_match_the_header() {
        let cases = [
            (&FOLDERID_PROFILE, "{5E6C858F-0E22-4760-9AFE-EA3317B67173}"),
            (
                &FOLDERID_ROAMING_APP_DATA,
                "{3EB685DB-65F9-4CF6-A03A-E3EF65729F3D}",
            ),
            (
                &FOLDERID_LOCAL_APP_DATA,
                "{F1B32785-6FBA-4FCF-9D55-7B8E7F157091}",
            ),
            (
                &FOLDERID_DOCUMENTS,
                "{FDD39AD0-238F-46AF-ADB4-6C85480369C7}",
            ),
            (&FOLDERID_WINDOWS, "{F38BF404-1D43-42F2-9305-67DE0B28FC23}"),
            (
                &FOLDERID_PROGRAM_FILES,
                "{905E63B6-C1BF-494E-B29C-65B732D3D21A}",
            ),
            (
                &FOLDERID_PROGRAM_FILES_X86,
                "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}",
            ),
            (
                &FOLDERID_PROGRAM_DATA,
                "{62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}",
            ),
        ];
        for (id, text) in cases {
            assert_eq!(guid_text(id), text);
        }
        // 8つとも違う値（取り違えて同じ値を2回書いていない）
        let mut all: Vec<String> = cases.iter().map(|(id, _)| guid_text(id)).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 8);
    }

    /// 使わない値が、使う値と重ならない（OBJ_INHERIT を付けない・危ない CreateOptions を付けない）。
    #[test]
    fn unused_flags_do_not_overlap() {
        assert_eq!(OBJ_INHERIT & (OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE), 0);
        let used = FILE_DIRECTORY_FILE
            | FILE_SYNCHRONOUS_IO_NONALERT
            | FILE_NON_DIRECTORY_FILE
            | FILE_OPEN_REPARSE_POINT;
        assert_eq!(used & FORBIDDEN_CREATE_OPTIONS, 0);
    }
}
