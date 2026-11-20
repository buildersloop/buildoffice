//! ログの1行（`source: k=v k=v`）。値は書いてよい型だけを受ける（文字列・バイト列・Display の値を受ける口はない）。

use std::fmt;
use std::io;
use std::time::Duration;

use crate::name::{Name, is_name, pick};

/// 数を書かないキーの名前に含まれる語（大文字と小文字を区別しない）。ポート・暗証の数字・秘密・鍵・トークン・
/// 招待・合言葉・アドレス（IP を数にしたもの）を、うっかり `count`・`int` で書かないため。当たれば値を `?` にする
/// （キーは書く）。`ip` は zip・skip に当たるので入れない（IP の型を受ける口は作らない。数として渡すことはレビューで止める）。
const SECRET_WORDS: [&str; 8] = [
    "port", "pin", "secret", "key", "token", "invite", "pass", "addr",
];

/// ログの1行。並びは足した順。表示（`Display`）は `source: k=v k=v`。
///
/// ```
/// use safe_log::{Line, name};
/// let line = Line::new(name!("vault-mcp"))
///     .name(name!("event"), name!("io_error"))
///     .io_kind(name!("kind"), std::io::ErrorKind::BrokenPipe)
///     .count(name!("lines"), 3);
/// assert_eq!(line.to_string(), "vault-mcp: event=io_error kind=broken_pipe lines=3");
/// ```
#[derive(Debug, Clone)]
pub struct Line {
    source: Name,
    fields: Vec<(Name, Field)>,
}

/// 1つの値。
#[derive(Debug, Clone, Copy)]
enum Field {
    Name(Name),
    /// ない（`-`）。
    Missing,
    /// 相手が送ったが、知っている名前でない（`?`）。
    Unknown,
    /// 書かない数（キーが秘密の数の名前。`?`）。
    Hidden,
    Count(u64),
    Int(i64),
    Ms(u128),
    Flag(bool),
    Kind(io::ErrorKind),
}

impl Line {
    /// 1行を始める。`source` は書き手の名前（例：`mcp`・`vault-mcp`）。
    #[must_use]
    pub fn new(source: Name) -> Self {
        Self {
            source,
            fields: Vec::new(),
        }
    }

    fn push(mut self, key: Name, field: Field) -> Self {
        self.fields.push((key, field));
        self
    }

    /// こちらが決めた名前を書く。
    ///
    /// ```
    /// use safe_log::{Line, name};
    /// let line = Line::new(name!("x")).name(name!("k"), name!("a"));
    /// assert_eq!(line.to_string(), "x: k=a");
    /// ```
    ///
    /// 文字列は受けない（`String` も、`&str` も）：
    ///
    /// ```compile_fail
    /// use safe_log::{Line, name};
    /// let line = Line::new(name!("x")).name(name!("k"), String::from("a"));
    /// ```
    ///
    /// ```compile_fail
    /// use safe_log::{Line, name};
    /// let line = Line::new(name!("x")).name(name!("k"), "a");
    /// ```
    #[must_use]
    pub fn name(self, key: Name, value: Name) -> Self {
        self.push(key, Field::Name(value))
    }

    /// 相手が送った文字が一覧にあれば、一覧の側の名前を書く。なければ `?`（相手の文字は書かない）。
    ///
    /// ```
    /// use safe_log::{Line, Name, name};
    /// const METHODS: &[Name] = &[name!("ping"), name!("tools/call")];
    /// let line = Line::new(name!("mcp"))
    ///     .one_of(name!("method"), "tools/call", METHODS)
    ///     .one_of(name!("tool"), r"C:\Profiles\alice\a.txt", METHODS);
    /// assert_eq!(line.to_string(), "mcp: method=tools/call tool=?");
    /// ```
    #[must_use]
    pub fn one_of(self, key: Name, input: &str, allowed: &[Name]) -> Self {
        let field = pick(input, allowed).map_or(Field::Unknown, Field::Name);
        self.push(key, field)
    }

    /// 値がないことを書く（`-`）。
    #[must_use]
    pub fn missing(self, key: Name) -> Self {
        self.push(key, Field::Missing)
    }

    /// 相手が送ったが知っている名前でないことを書く（`?`）。
    #[must_use]
    pub fn unknown(self, key: Name) -> Self {
        self.push(key, Field::Unknown)
    }

    /// 数（大きさ・個数）を書く。キーの名前に port・pin・secret・key・token・invite・pass・addr を含むときは `?`。
    ///
    /// ```
    /// use safe_log::{Line, name};
    /// let line = Line::new(name!("x")).count(name!("n"), 3).count(name!("port"), 443);
    /// assert_eq!(line.to_string(), "x: n=3 port=?");
    /// ```
    ///
    /// 秘密の包み（`Secret`）は受けない：
    ///
    /// ```compile_fail
    /// use safe_log::{Line, Secret, name};
    /// let line = Line::new(name!("x")).count(name!("n"), Secret::new(12345678u64));
    /// ```
    #[must_use]
    pub fn count(self, key: Name, value: u64) -> Self {
        let field = if secret_key(key) {
            Field::Hidden
        } else {
            Field::Count(value)
        };
        self.push(key, field)
    }

    /// 符号つきの数（JSON-RPC のエラーの番号など）を書く。キーの決まりは [`Line::count`] と同じ。
    #[must_use]
    pub fn int(self, key: Name, value: i64) -> Self {
        let field = if secret_key(key) {
            Field::Hidden
        } else {
            Field::Int(value)
        };
        self.push(key, field)
    }

    /// かかった時間をミリ秒で書く。
    #[must_use]
    pub fn ms(self, key: Name, elapsed: Duration) -> Self {
        self.push(key, Field::Ms(elapsed.as_millis()))
    }

    /// はい・いいえを書く（`true`・`false`）。
    #[must_use]
    pub fn flag(self, key: Name, value: bool) -> Self {
        self.push(key, Field::Flag(value))
    }

    /// 入出力のエラーの種類の名前を書く（例：`not_found`）。エラーの文（`io::Error` の表示）は書かない。
    #[must_use]
    pub fn io_kind(self, key: Name, kind: io::ErrorKind) -> Self {
        self.push(key, Field::Kind(kind))
    }
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.source.as_str())?;
        for (key, field) in &self.fields {
            write!(f, " {}=", key.as_str())?;
            match field {
                Field::Name(name) => f.write_str(name.as_str())?,
                Field::Missing => f.write_str("-")?,
                Field::Unknown | Field::Hidden => f.write_str("?")?,
                Field::Count(n) => write!(f, "{n}")?,
                Field::Int(n) => write!(f, "{n}")?,
                Field::Ms(n) => write!(f, "{n}")?,
                Field::Flag(b) => write!(f, "{b}")?,
                Field::Kind(kind) => f.write_str(&kind_name(*kind))?,
            }
        }
        Ok(())
    }
}

/// キーの名前が、秘密の数の名前か。
fn secret_key(key: Name) -> bool {
    let lower = key.as_str().to_ascii_lowercase();
    SECRET_WORDS.iter().any(|word| lower.contains(word))
}

/// 入出力のエラーの種類の名前（`NotFound` → `not_found`）。種類の名前は std の列挙の名前だけなので、
/// 秘密は入らない。それでも名前の形に合わなければ `?`。
fn kind_name(kind: io::ErrorKind) -> String {
    let raw = format!("{kind:?}");
    let mut snake = String::with_capacity(raw.len() + 4);
    for (i, c) in raw.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                snake.push('_');
            }
            snake.push(c.to_ascii_lowercase());
        } else {
            snake.push(c);
        }
    }
    if is_name(&snake) {
        snake
    } else {
        "?".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name;

    #[test]
    fn fields_are_written_in_order() {
        let line = Line::new(name!("mcp"))
            .name(name!("method"), name!("tools/call"))
            .missing(name!("tool"))
            .int(name!("result"), -32601)
            .ms(name!("ms"), Duration::from_micros(2_500))
            .flag(name!("read_only"), true)
            .unknown(name!("client"));
        assert_eq!(
            line.to_string(),
            "mcp: method=tools/call tool=- result=-32601 ms=2 read_only=true client=?"
        );
        assert_eq!(Line::new(name!("x")).to_string(), "x:");
    }

    #[test]
    fn numbers_under_secret_keys_are_hidden() {
        let line = Line::new(name!("x"))
            .count(name!("listen_port"), 443)
            .count(name!("PIN"), 1234)
            .int(name!("secret_len"), 5)
            .count(name!("keys"), 2)
            .count(name!("token_count"), 7)
            .int(name!("invite_no"), 12_345_678)
            .count(name!("passcode"), 9)
            .count(name!("peer_addr"), 3_232_271_704)
            .count(name!("files"), 4)
            .count(name!("zip_entries"), 2);
        assert_eq!(
            line.to_string(),
            "x: listen_port=? PIN=? secret_len=? keys=? token_count=? invite_no=? passcode=? peer_addr=? files=4 zip_entries=2"
        );
    }

    #[test]
    fn io_kinds_are_written_as_snake_case_names() {
        for (kind, text) in [
            (io::ErrorKind::NotFound, "not_found"),
            (io::ErrorKind::PermissionDenied, "permission_denied"),
            (io::ErrorKind::ConnectionRefused, "connection_refused"),
            (io::ErrorKind::BrokenPipe, "broken_pipe"),
            (io::ErrorKind::UnexpectedEof, "unexpected_eof"),
            (io::ErrorKind::Other, "other"),
        ] {
            assert_eq!(kind_name(kind), text);
        }
        // 文のある io::Error でも、書くのは種類の名前だけ
        let error = io::Error::new(io::ErrorKind::NotFound, r"C:\Profiles\SECRET-USER-q7\a.txt");
        let line = Line::new(name!("x")).io_kind(name!("kind"), error.kind());
        assert_eq!(line.to_string(), "x: kind=not_found");
    }
}
