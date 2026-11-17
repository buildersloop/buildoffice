//! エラーの種類と、エラーの値。
//!
//! エラーの文には、絶対パスと OS のエラーの文を入れない。
//! OS のエラーは、種類（`ErrorKind`）とエラーの番号だけを入れる。

use std::fmt;
use std::io;

/// エラーの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    /// 保管庫の中のパスとして、文字列の規則に合わない。
    InvalidPath,
    /// 保管庫の外を指している（リンク・ジャンクションを含む）。
    OutsideVault,
    /// 管理用の名前（`.vault`）を指している。
    ReservedPath,
    /// 保管庫にしてはいけない場所、または保管庫の根として使えないパス。
    InvalidRoot,
    /// 保管庫でない、または保管庫の管理用のデータが壊れている。
    NotAVault,
    /// ほかの処理が書き込み中で、ロックを取れなかった（時間切れ）。
    Locked,
    /// 引数の誤り。
    InvalidArgument,
    /// OS の入出力のエラー。
    IoError,
    /// 内部のエラー。
    Internal,
    /// ない。
    NotFound,
    /// フォルダでない（フォルダが要るところ）。
    NotADirectory,
    /// ファイルでない（ファイルが要るところ）。
    NotAFile,
    /// UTF-8 の文字として読めない。
    NotUtf8,
    /// 大きすぎる。
    TooLarge,
    /// すでにある。
    AlreadyExists,
    /// 版（`expected_version`）が合わない。
    Conflict,
    /// ディスクがいっぱい。
    DiskFull,
    /// ファイルが使用中で、置き換えられない（何度か試したあと）。
    Busy,
    /// 読み取り専用で開いた保管庫に、書くツールを呼んだ。
    ReadOnly,
    /// 知らないツールの名前。
    UnknownTool,
    /// まだ作っていないツール（Phase 0 の作っている途中だけ）。
    NotImplemented,
    /// アクセスが拒否された（読み取り専用の印・権限。何度か試したあと）。
    AccessDenied,
    /// フォルダの中が空でない（recursive が要る）。
    DirNotEmpty,
    /// ゴミ箱の entry が正しくない（entry.json が読めない・偽の original_path・data がない・リンク など）。
    InvalidTrashEntry,
}

impl ErrorCode {
    /// エラーの種類の名前（例：`INVALID_PATH`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPath => "INVALID_PATH",
            Self::OutsideVault => "OUTSIDE_VAULT",
            Self::ReservedPath => "RESERVED_PATH",
            Self::InvalidRoot => "INVALID_ROOT",
            Self::NotAVault => "NOT_A_VAULT",
            Self::Locked => "LOCKED",
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::IoError => "IO_ERROR",
            Self::Internal => "INTERNAL",
            Self::NotFound => "NOT_FOUND",
            Self::NotADirectory => "NOT_A_DIRECTORY",
            Self::NotAFile => "NOT_A_FILE",
            Self::NotUtf8 => "NOT_UTF8",
            Self::TooLarge => "TOO_LARGE",
            Self::AlreadyExists => "ALREADY_EXISTS",
            Self::Conflict => "CONFLICT",
            Self::DiskFull => "DISK_FULL",
            Self::Busy => "BUSY",
            Self::ReadOnly => "READ_ONLY",
            Self::UnknownTool => "UNKNOWN_TOOL",
            Self::NotImplemented => "NOT_IMPLEMENTED",
            Self::AccessDenied => "ACCESS_DENIED",
            Self::DirNotEmpty => "DIR_NOT_EMPTY",
            Self::InvalidTrashEntry => "INVALID_TRASH_ENTRY",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// エラーの細かい種類：外に見えるエラーのコード（`ErrorCode`）は増やさずに、Rust から呼ぶ側が見分けるための内部の印。
/// MCP の返事・FFI の返事・表示（「CODE: 説明」）には出さない。文の書き出しで見分けないために使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorDetail {
    /// init で、渡したフォルダがすでに保管庫だった（`.vault` がある。コードは `InvalidRoot`）。
    AlreadyAVault,
}

/// vault-core のエラー。表示は「CODE: 説明」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultError {
    code: ErrorCode,
    message: String,
    detail: Option<ErrorDetail>,
}

impl VaultError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    /// 内部の印を付ける（コードと文は変えない）。
    pub(crate) fn with_detail(mut self, detail: ErrorDetail) -> Self {
        self.detail = Some(detail);
        self
    }

    /// OS の入出力のエラーから作る。OS のエラーの文は入れず、種類と番号だけを入れる。
    pub(crate) fn io(what: &str, err: &io::Error) -> Self {
        let detail = match err.raw_os_error() {
            Some(n) => format!("{:?}、OS のエラー番号 {n}", err.kind()),
            None => format!("{:?}", err.kind()),
        };
        Self::new(ErrorCode::IoError, format!("{what}（{detail}）"))
    }

    /// 説明の文のあとに、書き足す（絶対パスを入れないこと）。
    pub(crate) fn with_note(mut self, note: &str) -> Self {
        self.message.push('。');
        self.message.push_str(note);
        self
    }

    /// エラーの種類。
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// 説明の文（絶対パスを含まない）。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 内部の印（あれば）。見分けるときは、文ではなくこれを使う。
    pub fn detail(&self) -> Option<ErrorDetail> {
        self.detail
    }
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VaultError {}

/// vault-core の `Result`。
pub type Result<T> = std::result::Result<T, VaultError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_code_and_message() {
        let e = VaultError::new(ErrorCode::InvalidPath, "説明");
        assert_eq!(e.to_string(), "INVALID_PATH: 説明");
        assert_eq!(e.code(), ErrorCode::InvalidPath);
        assert_eq!(e.message(), "説明");
    }

    #[test]
    fn io_error_has_kind_and_number_but_not_the_os_text() {
        let os = io::Error::from_raw_os_error(2);
        let e = VaultError::io("読めない", &os);
        assert_eq!(e.code(), ErrorCode::IoError);
        assert!(e.message().contains("OS のエラー番号 2"));
        // OS のエラーの文（パスを含むことがある）は入れない
        assert!(!e.message().contains(&os.to_string()));

        let custom = io::Error::other(r"C:\secret\path");
        let e = VaultError::io("読めない", &custom);
        assert!(!e.message().contains("secret"));
    }

    #[test]
    fn notes_are_appended_to_the_message() {
        let e = VaultError::new(ErrorCode::IoError, "書けない").with_note("あとの説明");
        assert_eq!(e.message(), "書けない。あとの説明");
        assert_eq!(e.code(), ErrorCode::IoError);
    }

    #[test]
    fn code_names_match_the_adr() {
        let all = [
            (ErrorCode::InvalidPath, "INVALID_PATH"),
            (ErrorCode::OutsideVault, "OUTSIDE_VAULT"),
            (ErrorCode::ReservedPath, "RESERVED_PATH"),
            (ErrorCode::InvalidRoot, "INVALID_ROOT"),
            (ErrorCode::NotAVault, "NOT_A_VAULT"),
            (ErrorCode::Locked, "LOCKED"),
            (ErrorCode::InvalidArgument, "INVALID_ARGUMENT"),
            (ErrorCode::IoError, "IO_ERROR"),
            (ErrorCode::Internal, "INTERNAL"),
            (ErrorCode::NotFound, "NOT_FOUND"),
            (ErrorCode::NotADirectory, "NOT_A_DIRECTORY"),
            (ErrorCode::NotAFile, "NOT_A_FILE"),
            (ErrorCode::NotUtf8, "NOT_UTF8"),
            (ErrorCode::TooLarge, "TOO_LARGE"),
            (ErrorCode::AlreadyExists, "ALREADY_EXISTS"),
            (ErrorCode::Conflict, "CONFLICT"),
            (ErrorCode::DiskFull, "DISK_FULL"),
            (ErrorCode::Busy, "BUSY"),
            (ErrorCode::ReadOnly, "READ_ONLY"),
            (ErrorCode::UnknownTool, "UNKNOWN_TOOL"),
            (ErrorCode::NotImplemented, "NOT_IMPLEMENTED"),
            (ErrorCode::AccessDenied, "ACCESS_DENIED"),
            (ErrorCode::DirNotEmpty, "DIR_NOT_EMPTY"),
            (ErrorCode::InvalidTrashEntry, "INVALID_TRASH_ENTRY"),
        ];
        for (code, name) in all {
            assert_eq!(code.as_str(), name);
        }
    }
}
