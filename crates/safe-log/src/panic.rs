//! panic の知らせ（panic hook）の差し替え。panic の文は書かない（パスや中身が入ることがあるため）。

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::panic;

use crate::name::Name;

/// 場所のファイルの長さの上限（バイト）。
const MAX_PLACE_LEN: usize = 160;

/// プロセスの panic の知らせを、決まった1行だけを stderr に書くものに替える。
///
/// 書くのは `<source>: 内部のエラー（panic。詳しい文は出さない）` の1行。panic の文（`panic!` に渡した文・
/// `unwrap` の値）と、バックトレースは書かない。`location` が true なら、起きた場所（ファイルと行）を
/// ` location=<ファイル>:<行>` で足す。ファイルは、このリポジトリの相対の形（`crates/` で始まる）のときだけ書き、
/// ほか（依存のクレートの絶対パス・`/rustc/…`）は `?` にする。
///
/// プロセス全体の設定なので、バイナリ（vault-mcp・vaultd・mesh-server）の入口でだけ呼ぶ。ライブラリ
/// （vault-ffi など）は呼ばない。
pub fn set_quiet_panic_hook(source: Name, location: bool) {
    panic::set_hook(Box::new(move |info| {
        let place = info.location().map(|place| (place.file(), place.line()));
        let mut text = panic_line(source, place, location);
        text.push('\n');
        let _ = io::stderr().lock().write_all(text.as_bytes());
    }));
}

/// panic の知らせの1行（改行なし）。
fn panic_line(source: Name, place: Option<(&str, u32)>, location: bool) -> String {
    let mut text = format!(
        "{}: 内部のエラー（panic。詳しい文は出さない）",
        source.as_str()
    );
    if location {
        match place {
            Some((file, line)) if is_repo_file(file) => {
                let _ = write!(text, " location={file}:{line}");
            }
            _ => text.push_str(" location=?"),
        }
    }
    text
}

/// このリポジトリの相対の形のファイルか：`crates/`（か `crates\`）で始まり、文字は `[A-Za-z0-9_./\-]` と `\`、
/// `..` と `:` を含まない、160 バイトまで。
fn is_repo_file(file: &str) -> bool {
    (file.starts_with("crates/") || file.starts_with("crates\\"))
        && file.len() <= MAX_PLACE_LEN
        && !file.contains("..")
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'\\' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name;

    const FIXED: &str = "vault-mcp: 内部のエラー（panic。詳しい文は出さない）";

    #[test]
    fn without_location_only_the_fixed_line() {
        let place = Some(("crates/vault-mcp/src/host.rs", 12));
        assert_eq!(panic_line(name!("vault-mcp"), place, false), FIXED);
        assert_eq!(panic_line(name!("vault-mcp"), None, false), FIXED);
    }

    #[test]
    fn relative_places_are_written() {
        for file in [
            "crates/vault-core/src/ops.rs",
            r"crates\vault-core\src\guard\windows.rs",
            r"crates/vault-core/src\guard\windows.rs",
        ] {
            assert_eq!(
                panic_line(name!("vaultd"), Some((file, 7)), true),
                format!("vaultd: 内部のエラー（panic。詳しい文は出さない） location={file}:7")
            );
        }
    }

    #[test]
    fn other_places_are_hidden() {
        let long = format!("crates/{}.rs", "a".repeat(200));
        for file in [
            "/rustc/0ef1bc386abcdef/library/core/src/option.rs",
            r"C:\Profiles\SECRET-USER-q7\.cargo\registry\src\x\lib.rs",
            "/home/secret-user-q7/.cargo/registry/src/x/lib.rs",
            r"\\srv\share\crates\x.rs",
            "src/main.rs",
            "crates/../../secret-user-q7/x.rs",
            "crates/x y.rs",
            "crates/山田.rs",
            "crates/x:y.rs",
            long.as_str(),
            "",
        ] {
            let text = panic_line(name!("vaultd"), Some((file, 7)), true);
            assert_eq!(
                text, "vaultd: 内部のエラー（panic。詳しい文は出さない） location=?",
                "{file}"
            );
        }
        assert!(panic_line(name!("vaultd"), None, true).ends_with(" location=?"));
    }
}
