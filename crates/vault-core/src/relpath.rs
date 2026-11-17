//! 保管庫の中の相対パスの、文字列の規則。OS に関係なく同じ規則にする。
//!
//! 区切りは `/` だけで、根は `""` で表す。規則に合わなければ `INVALID_PATH`、
//! どこかの要素が `.vault`（大文字小文字を問わない）なら `RESERVED_PATH`。

use std::fmt;

use crate::error::{ErrorCode, Result, VaultError};

/// パス全体の長さの上限（バイト）。
pub const MAX_PATH_BYTES: usize = 4096;
/// 要素ごとの長さの上限（バイト）。
pub const MAX_COMPONENT_BYTES: usize = 255;
/// 管理用のフォルダの名前。
pub(crate) const RESERVED_NAME: &str = ".vault";

/// 文字列の規則を通った、保管庫の中の相対パス。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelPath {
    parts: Vec<String>,
}

impl RelPath {
    /// 文字列の規則で確かめて作る。
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() {
            return Ok(Self { parts: Vec::new() });
        }
        if input.len() > MAX_PATH_BYTES {
            return Err(invalid(format!(
                "パスが長すぎる（全体で {MAX_PATH_BYTES} バイトまで）"
            )));
        }
        if input.starts_with('/') {
            return Err(invalid(
                "先頭に / は使えない（保管庫の中の相対パスで、区切りは / で書く）",
            ));
        }
        let parts = input
            .split('/')
            .map(|part| check_component(part).map(|()| part.to_owned()))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { parts })
    }

    /// 保管庫の根（`""`）。
    pub fn root() -> Self {
        Self { parts: Vec::new() }
    }

    /// 根かどうか。
    pub fn is_root(&self) -> bool {
        self.parts.is_empty()
    }

    /// 要素の並び。
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().map(String::as_str)
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.parts.join("/"))
    }
}

/// 1つの要素（名前）が、文字列の規則に通るか（ツールで扱える名前か）。一覧と走査で、扱えない名前を除くのに使う。
pub(crate) fn is_valid_component(part: &str) -> bool {
    check_component(part).is_ok()
}

fn check_component(part: &str) -> Result<()> {
    if part.is_empty() {
        return Err(invalid("空の要素がある（// や、末尾の / は使えない）"));
    }
    if part.len() > MAX_COMPONENT_BYTES {
        return Err(invalid(format!(
            "要素が長すぎる（要素ごとに {MAX_COMPONENT_BYTES} バイトまで）"
        )));
    }
    if let Some(c) = part.chars().find(|&c| is_forbidden_char(c)) {
        return Err(invalid(format!(
            "使えない文字 U+{:04X} がある（\\ : < > \" | ? * と、制御文字・目に見えない文字・向きを変える文字は使えない）",
            u32::from(c)
        )));
    }
    if part == "." || part == ".." {
        return Err(invalid("要素に . と .. は使えない"));
    }
    if part.eq_ignore_ascii_case(RESERVED_NAME) {
        return Err(VaultError::new(
            ErrorCode::ReservedPath,
            ".vault は管理用の名前なので使えない",
        ));
    }
    if part.ends_with('.') || part.ends_with(' ') {
        return Err(invalid("末尾が . か空白の要素は使えない"));
    }
    if has_tilde_digit(part) {
        return Err(invalid(
            "~ のあとに数字が続く名前は使えない（Windows の短い名前と見分けられないため。例：file~1.txt）",
        ));
    }
    if is_windows_reserved_name(part) {
        return Err(invalid(
            "Windows の予約名（CON・PRN・AUX・NUL・COM0〜9・LPT0〜9 など）は、拡張子が付いていても使えない",
        ));
    }
    Ok(())
}

fn is_forbidden_char(c: char) -> bool {
    matches!(
        c,
        '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
            // 制御文字（C0・DEL・C1）
            | '\u{0}'..='\u{1F}'
            | '\u{7F}'..='\u{9F}'
            // 目に見えない文字・向きを変える文字
            | '\u{AD}'
            | '\u{61C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

fn has_tilde_digit(part: &str) -> bool {
    part.as_bytes()
        .windows(2)
        .any(|w| w[0] == b'~' && w[1].is_ascii_digit())
}

/// Windows の予約名か（大文字小文字を問わず、拡張子が付いていても）。
/// 最初の `.` より前を名前とし、末尾の空白は除いて比べる（`CON .txt` も予約名として扱う）。
pub(crate) fn is_windows_reserved_name(part: &str) -> bool {
    let base = part.split('.').next().unwrap_or(part).trim_end_matches(' ');
    let upper = base.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) {
        return true;
    }
    let Some(rest) = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut chars = rest.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('0'..='9' | '¹' | '²' | '³'), None)
    )
}

fn invalid(message: impl Into<String>) -> VaultError {
    VaultError::new(ErrorCode::InvalidPath, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// パスの表のうち、拒否されるもの。
    const ADR_REJECTED: [&str; 22] = [
        "..",
        "a/../b",
        "/x",
        "C:/x",
        "C:x",
        r"\\s\x",
        r"a\b",
        "a:b",
        "CON",
        "con.txt",
        "CONIN$",
        "a.",
        "a ",
        "a//b",
        "a\u{0}b",    // NUL
        "a\u{202E}b", // 向きを変える文字
        "a\u{FEFF}b", // 目に見えない文字
        ".VAULT/x",
        "x/.vault/y",
        "VAULT~1/x",
        "vault~1/journal/00000000000000000001.jsonl",
        "abc~2.txt",
    ];

    /// パスの表のうち、通るもの。
    const ADR_ACCEPTED: [&str; 3] = ["日本語のメモ.md", "my notes/a b.txt", "a/b/c.md"];

    #[test]
    fn adr_table_rejected() {
        for input in ADR_REJECTED {
            let err = RelPath::parse(input).unwrap_err();
            let expected = if input.to_ascii_lowercase().contains(".vault/") {
                ErrorCode::ReservedPath
            } else {
                ErrorCode::InvalidPath
            };
            assert_eq!(err.code(), expected, "{input:?}: {err}");
        }
    }

    #[test]
    fn adr_table_accepted() {
        for input in ADR_ACCEPTED {
            let p = RelPath::parse(input).unwrap_or_else(|e| panic!("{input:?}: {e}"));
            assert_eq!(p.to_string(), input);
        }
    }

    #[test]
    fn root_is_the_empty_string() {
        let p = RelPath::parse("").unwrap();
        assert!(p.is_root());
        assert_eq!(p, RelPath::root());
        assert_eq!(p.to_string(), "");
        assert_eq!(p.components().count(), 0);
    }

    #[test]
    fn components_are_split_by_slash() {
        let p = RelPath::parse("a/b/c.md").unwrap();
        assert_eq!(p.components().collect::<Vec<_>>(), ["a", "b", "c.md"]);
        assert!(!p.is_root());
    }

    #[test]
    fn length_limits() {
        let comp255 = "a".repeat(MAX_COMPONENT_BYTES);
        assert!(RelPath::parse(&comp255).is_ok());
        assert_eq!(
            RelPath::parse(&"a".repeat(MAX_COMPONENT_BYTES + 1))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidPath
        );
        // 日本語は1文字3バイトなので、85字まで
        assert!(RelPath::parse(&"あ".repeat(85)).is_ok());
        assert!(RelPath::parse(&"あ".repeat(86)).is_err());

        // 全体で 4096 バイトちょうどは通り、4097 バイトは拒否
        let comp = "a".repeat(240);
        let mut path = vec![comp.as_str(); 17].join("/"); // 240*17 + 16 = 4096
        assert_eq!(path.len(), MAX_PATH_BYTES);
        assert!(RelPath::parse(&path).is_ok());
        path.push('a');
        assert_eq!(
            RelPath::parse(&path).unwrap_err().code(),
            ErrorCode::InvalidPath
        );
    }

    #[test]
    fn every_forbidden_character_is_rejected() {
        let mut chars: Vec<char> = r#"\:<>"|?*"#.chars().collect();
        chars.extend((0u32..=0x1F).filter_map(char::from_u32));
        chars.extend((0x7F..=0x9F).filter_map(char::from_u32));
        chars.extend(['\u{AD}', '\u{61C}', '\u{180E}', '\u{3164}', '\u{FEFF}']);
        chars.extend((0x200B..=0x200F).filter_map(char::from_u32));
        chars.extend((0x2028..=0x2029).filter_map(char::from_u32));
        chars.extend((0x202A..=0x202E).filter_map(char::from_u32));
        chars.extend((0x2060..=0x206F).filter_map(char::from_u32));
        chars.extend((0xFE00..=0xFE0F).filter_map(char::from_u32));
        chars.extend((0xE0000..=0xE007F).filter_map(char::from_u32));
        assert_eq!(chars.len(), 8 + 32 + 33 + 5 + 5 + 2 + 5 + 16 + 16 + 128);
        for c in chars {
            let input = format!("a{c}b");
            assert_eq!(
                RelPath::parse(&input).unwrap_err().code(),
                ErrorCode::InvalidPath,
                "U+{:04X}",
                u32::from(c)
            );
        }
        // 近くの文字は通る
        for ok in [
            "a\u{200A}b",
            "a\u{2010}b",
            "a\u{2027}b",
            "a\u{202F}b",
            "a\u{2070}b",
            "a\u{A0}b",
            "a\u{AC}b",
            "a\u{AE}b",
            "a\u{61B}b",
            "a\u{180D}b",
            "a\u{3165}b",
            "a\u{FDFF}b",
            "a\u{FE10}b",
            "a\u{E0080}b",
            "a\u{DFFFF}b",
        ] {
            assert!(RelPath::parse(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn dots_spaces_and_empty_components() {
        for bad in [
            ".", "a/.", "./a", "a/..", "a/", "a//", "b/a.", "b/a ", "...",
        ] {
            assert_eq!(
                RelPath::parse(bad).unwrap_err().code(),
                ErrorCode::InvalidPath,
                "{bad:?}"
            );
        }
        for ok in [".a", "a.b", " a", "a b", ".config", "..a", "a..b"] {
            assert!(RelPath::parse(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn tilde_followed_by_digit() {
        for bad in ["a~0", "LONGFI~1.TXT", "x/y~9z", "~1"] {
            assert!(RelPath::parse(bad).is_err(), "{bad:?}");
        }
        for ok in ["a~b", "~", "a~", "~a", "a~~b", "a~１"] {
            assert!(RelPath::parse(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn windows_reserved_names() {
        let mut bad = vec![
            "CON",
            "prn",
            "Aux",
            "nul",
            "CONIN$",
            "conout$",
            "CLOCK$",
            "con.txt",
            "NUL.tar.gz",
            "CON .txt",
            "a/LPT1",
            "clock$.log",
        ];
        let numbered: Vec<String> = ["COM", "LPT", "com", "lpt"]
            .iter()
            .flat_map(|p| {
                ["0", "1", "5", "9", "¹", "²", "³"]
                    .iter()
                    .map(move |n| format!("{p}{n}"))
            })
            .collect();
        bad.extend(numbered.iter().map(String::as_str));
        for name in bad {
            assert_eq!(
                RelPath::parse(name).unwrap_err().code(),
                ErrorCode::InvalidPath,
                "{name:?}"
            );
        }
        for ok in [
            "CONSOLE",
            "COM10",
            "LPT",
            "COM",
            "AUXILIARY.txt",
            "a.con",
            "my-con",
            "COM⁴",
            "nulls",
        ] {
            assert!(RelPath::parse(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn reserved_vault_name_anywhere() {
        for bad in [".vault", ".VAULT", ".Vault/x", "a/.vault", "a/.vAuLt/b"] {
            assert_eq!(
                RelPath::parse(bad).unwrap_err().code(),
                ErrorCode::ReservedPath,
                "{bad:?}"
            );
        }
        for ok in [".vaultx", "x.vault", ".vault.txt", "vault"] {
            assert!(RelPath::parse(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn error_messages_do_not_echo_the_input() {
        let err = RelPath::parse("secret-name/..").unwrap_err();
        assert!(!err.message().contains("secret-name"));
    }
}
