//! コマンドラインの引数。
//!
//! - `vault-mcp --vault <絶対パス> [--read-only] [--actor-id <名前>]`：MCP のサーバーとして動く
//! - `vault-mcp init <絶対パス>`：保管庫を作って終わる（MCP のツールではない手続き）
//! - `vault-mcp --help`：使い方を stderr に出して終わる
//!
//! 誤りの文には、渡された値（パスなど）を入れない（stderr にも絶対パスを出さない）。
//! `--vault` と `--actor-id` の値が `--` で始まるときは、引数の誤りにする（`--actor-id --read-only` のように、
//! 次の引数を値として取り込んで、読み取り専用が黙って外れることがないように）。

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// 使い方（stderr に出す。stdout は MCP のメッセージだけに使う）。
pub const USAGE: &str = "使い方：
  vault-mcp --vault <保管庫の絶対パス> [--read-only] [--actor-id <名前>]
      MCP のサーバーとして動く（stdin と stdout で、改行区切りの JSON-RPC 2.0）。
      保管庫は、先に vault-mcp init かアプリで作っておく。
      --read-only    書くツール（write・mkdir・move・delete・restore）を出さず、呼ばれたら READ_ONLY にする
      --actor-id     履歴に残す名前（mcp:<名前>）を、クライアントの名乗る名前の代わりに使う
  vault-mcp init <保管庫にするフォルダの絶対パス>
      保管庫を作って終わる（MCP のサーバーとしては動かない）。空のフォルダか、まだないフォルダを渡す。
  vault-mcp --help
      この使い方を出す。";

/// 何をするか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// MCP のサーバーとして動く。
    Serve {
        vault: PathBuf,
        read_only: bool,
        actor_id: Option<String>,
    },
    /// 保管庫を作って終わる。
    Init(PathBuf),
    /// 使い方を出して終わる。
    Help,
}

/// 引数（プログラムの名前を除いたもの）を読む。誤りなら、その説明（値は入れない）。
pub fn parse(args: Vec<OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Err("引数がない".to_owned());
    };
    if first == "init" {
        let path = args
            .next()
            .ok_or("init には、保管庫にするフォルダの絶対パスを渡す")?;
        if args.next().is_some() {
            return Err("init に渡すのは、絶対パス1つだけ".to_owned());
        }
        return Ok(Command::Init(PathBuf::from(path)));
    }
    if first == "--help" || first == "-h" {
        return Ok(Command::Help);
    }

    let mut vault = None;
    let mut read_only = false;
    let mut actor_id = None;
    let mut next = Some(first);
    while let Some(arg) = next.take().or_else(|| args.next()) {
        if arg == "--vault" {
            let value = value_of(args.next())
                .ok_or("--vault のあとに、保管庫の絶対パスを渡す（-- で始まる値は使えない）")?;
            if vault.replace(PathBuf::from(value)).is_some() {
                return Err("--vault は1回だけ".to_owned());
            }
        } else if arg == "--read-only" {
            if read_only {
                return Err("--read-only は1回だけ".to_owned());
            }
            read_only = true;
        } else if arg == "--actor-id" {
            let value = value_of(args.next())
                .ok_or("--actor-id のあとに、名前を渡す（-- で始まる値は使えない）")?;
            let value = value
                .into_string()
                .map_err(|_| "--actor-id の名前は、文字として読めるものにする".to_owned())?;
            if actor_id.replace(value).is_some() {
                return Err("--actor-id は1回だけ".to_owned());
            }
        } else {
            return Err("知らない引数がある（値は表示しない）".to_owned());
        }
    }
    let vault = vault.ok_or("--vault（保管庫の絶対パス）がない")?;
    Ok(Command::Serve {
        vault,
        read_only,
        actor_id,
    })
}

/// 値として使える引数か（なければ None。`--` で始まるもの（ほかの引数）も None）。
fn value_of(arg: Option<OsString>) -> Option<OsString> {
    arg.filter(|value| !starts_with_dashes(value))
}

fn starts_with_dashes(value: &OsStr) -> bool {
    value.to_string_lossy().starts_with("--")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from).collect())
    }

    #[test]
    fn serve_init_and_help() {
        assert_eq!(
            parse_strs(&["--vault", r"D:\v"]),
            Ok(Command::Serve {
                vault: PathBuf::from(r"D:\v"),
                read_only: false,
                actor_id: None,
            })
        );
        assert_eq!(
            parse_strs(&["--read-only", "--actor-id", "me", "--vault", r"D:\v"]),
            Ok(Command::Serve {
                vault: PathBuf::from(r"D:\v"),
                read_only: true,
                actor_id: Some("me".to_owned()),
            })
        );
        assert_eq!(
            parse_strs(&["init", r"D:\v"]),
            Ok(Command::Init(PathBuf::from(r"D:\v")))
        );
        assert_eq!(parse_strs(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["-h"]), Ok(Command::Help));
    }

    #[test]
    fn mistakes_are_reported_without_echoing_values() {
        for args in [
            &[][..],
            &["init"],
            &["init", "a", "b"],
            &["--vault"],
            &["--vault", "a", "--vault", "b"],
            &["--read-only"],
            &["--read-only", "--read-only", "--vault", "a"],
            &["--vault", "a", "--actor-id"],
            &["--vault", "a", "--secret-flag"],
            &["--vault=a"],
            &["C:\\Profiles\\someone"],
            // 値が -- で始まる：次の引数を値として取り込まない
            &["--vault", "a", "--actor-id", "--read-only"],
            &["--vault", "--read-only"],
            &["--vault", "--", "--read-only"],
            &["--actor-id", "--vault", "a"],
        ] {
            let err = parse_strs(args).unwrap_err();
            assert!(!err.contains("secret") && !err.contains("someone"), "{err}");
        }
    }
}
