//! 名前（ログに書いてよい文字列）。コンパイルのときに決まる文字（ソースに書いた文字）からだけ作れる。

/// ログに書いてよい名前の長さの上限（バイト。名前は ASCII だけなので、文字の数と同じ）。
pub const MAX_NAME_LEN: usize = 64;
/// 名前の中で、英数字が続いてよい長さの上限（これより長く続けば、鍵や印の形とみなして断る）。
pub const MAX_ALNUM_RUN: usize = 16;

/// ログに書いてよい名前（出来事・キー・メソッド・ツール・エラーのコードなど、こちらが決めた名前）。
///
/// 作り方は [`name!`](crate::name) だけ。文字は定数（コンパイルのときに決まる値）でしか渡せないので、
/// 実行のときの文字列（読んだファイル・環境変数・引数・相手が送った文字・leak した `String`・
/// `LazyLock` の中身）からは作れない。名前の形（[`is_name`]）に合わなければ、コンパイルの誤りになる。
///
/// ```
/// let name = safe_log::name!("vault_write");
/// assert_eq!(name.as_str(), "vault_write");
/// ```
///
/// 名前の形に合わない文字は、コンパイルの誤り：
///
/// ```compile_fail
/// let name = safe_log::name!("C:\\Profiles\\alice");
/// ```
///
/// 中の文字を直に入れる口はない：
///
/// ```compile_fail
/// let name = safe_log::Name("vault_write");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Name(&'static str);

impl Name {
    /// 名前を作る（[`name!`](crate::name) が使う）。文字は `L::TEXT`（定数）。名前の形に合わなければ、
    /// コンパイルの誤りになる（`const` のブロックで確かめるので、実行のときまで残らない）。
    ///
    /// 形の誤りが出るのはビルド（`cargo build`・`cargo test`）のとき。関数の中で使った `name!` の誤りは、
    /// `cargo check`・`cargo clippy` では出ない（コードを作る段で確かめるため）。`const` の項目に置いたものは
    /// `cargo check` でも出る。
    ///
    /// ```
    /// struct Event;
    /// impl safe_log::Literal for Event {
    ///     const TEXT: &'static str = "io_error";
    /// }
    /// assert_eq!(safe_log::Name::of::<Event>().as_str(), "io_error");
    /// ```
    ///
    /// `name!` を通さずに作っても、形の確かめは同じ：
    ///
    /// ```compile_fail
    /// struct Path;
    /// impl safe_log::Literal for Path {
    ///     const TEXT: &'static str = "/home/alice";
    /// }
    /// let name = safe_log::Name::of::<Path>();
    /// ```
    #[must_use]
    pub const fn of<L: Literal>() -> Self {
        const {
            assert!(
                is_name(L::TEXT),
                "safe-log：名前の形でない（1〜64 字、[A-Za-z0-9_/-]、先頭は英字、英数字は 16 字まで続けてよい）"
            );
        }
        Self(L::TEXT)
    }

    /// 名前の文字。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// 名前の文字を、定数で渡す口（[`name!`](crate::name) の中で使う）。
///
/// `TEXT` は定数なので、実行のときに読んだ文字列は入れられない。ソースに書いた文字のほかに定数にできるもの
/// （`env!`・`option_env!`・`include_str!` など、ビルドする PC の値を取り込むマクロ）は、ソースの検査
/// （`crates/safe-log/tests/source_check.rs`）で止める。
pub trait Literal {
    /// 名前の文字。
    const TEXT: &'static str;
}

/// ログに書く名前を、ソースに書いた文字から作る。文字は文字列のリテラルだけ（変数・式・マクロの結果は受けない）。
///
/// ```
/// const KNOWN: &[safe_log::Name] = &[safe_log::name!("ping"), safe_log::name!("tools/call")];
/// assert_eq!(KNOWN[1].as_str(), "tools/call");
/// ```
///
/// 変数は渡せない：
///
/// ```compile_fail
/// let text = String::from("alice");
/// let name = safe_log::name!(text);
/// ```
///
/// マクロの結果（環境変数など）も渡せない：
///
/// ```compile_fail
/// let name = safe_log::name!(env!("CARGO_PKG_NAME"));
/// ```
#[macro_export]
macro_rules! name {
    ($text:literal) => {{
        struct SafeLogLiteral;
        impl $crate::Literal for SafeLogLiteral {
            const TEXT: &'static str = $text;
        }
        $crate::Name::of::<SafeLogLiteral>()
    }};
}

/// 名前の形か：1〜64 字、文字は `[A-Za-z0-9_/-]`、先頭は英字、英数字が 17 字以上続かない。
///
/// これは書式の守り（改行・空白・区切り・パス・IP・長い鍵の形を止める）。秘密を出さない保証は、名前を
/// 定数からしか作れないこと（[`Name`]）・相手の文字は一覧と照らすこと（[`pick`]）・ソースの検査で持つ
/// （`ABCD-EFGH`・`home/alice`・`alice` のような形は、ここでは止まらない）。
#[must_use]
pub const fn is_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_LEN || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    let mut run = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() {
            run += 1;
            if run > MAX_ALNUM_RUN {
                return false;
            }
        } else if matches!(b, b'_' | b'/' | b'-') {
            run = 0;
        } else {
            return false;
        }
        i += 1;
    }
    true
}

/// 相手が送った文字（`input`）が一覧（`allowed`）にあれば、一覧の側の名前を返す。なければ None。
/// 返すのは一覧の名前なので、相手の文字そのものはログに届かない。
#[must_use]
pub fn pick(input: &str, allowed: &[Name]) -> Option<Name> {
    allowed.iter().copied().find(|name| name.0 == input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_our_own_pass() {
        for text in [
            "tools/call",
            "notifications/initialized",
            "vault_write",
            "vault_trash_list",
            "NOT_A_VAULT",
            "INVALID_TRASH_ENTRY",
            "notification-ignored",
            "io_error",
            "ms",
            "a",
            "PermissionDenied",
        ] {
            assert!(is_name(text), "{text}");
        }
    }

    #[test]
    fn malformed_names_fail() {
        let long_hex = "deadbeef".repeat(8);
        let sixty_five = "m".repeat(65);
        for text in [
            "",
            " ",
            "a b",
            "a\nb",
            "a=b",
            "a.b",
            "a:b",
            r"a\b",
            "_a",
            "-a",
            "/a",
            "1a",
            "12345678",
            "192.168.77.88",
            "fe80::1",
            "山田太郎",
            "x山",
            "abcdefghijklmnopq",
            long_hex.as_str(),
            sixty_five.as_str(),
        ] {
            assert!(!is_name(text), "{text:?}");
        }
        // 16 字の英数字の連続は通り、17 字は通らない
        assert!(is_name("abcdefghijklmnop"));
        assert!(is_name("abcdefghijklmnop_abcdefghijklmnop"));
        // 64 字は通り、65 字は通らない
        assert!(is_name(&"ab_".repeat(21)[..63]));
        assert!(is_name(&format!("{}a", &"ab_".repeat(21)[..63])));
        assert!(!is_name(&format!("{}ab", &"ab_".repeat(21)[..63])));
    }

    #[test]
    fn pick_returns_the_listed_name() {
        const LIST: &[Name] = &[crate::name!("ping"), crate::name!("tools/call")];
        let input = String::from("tools/call");
        let picked = pick(&input, LIST).unwrap();
        assert_eq!(picked.as_str(), "tools/call");
        // 一覧の側の文字（定数の領域）で、相手の文字の写しではない
        assert!(std::ptr::eq(picked.as_str(), LIST[1].as_str()));
        assert_eq!(pick("TOOLS/CALL", LIST), None);
        assert_eq!(pick("tools/call ", LIST), None);
        assert_eq!(pick("", LIST), None);
        assert_eq!(pick("ping", &[]), None);
    }
}
