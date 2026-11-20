//! MCP サーバー（stdio）と、保管庫を作るコマンド `vault-mcp init <絶対パス>`。
//!
//! - stdout は MCP のメッセージだけに使う。使い方・誤り・ログは stderr に出す。
//! - stderr には、絶対パスを出さない（MCP のクライアントは、サーバーの stderr をログに残すため）。
//! - panic の文は stderr に出さない（決まった1行だけ。ログはメソッド名・ツール名・コード・時間だけ）。
//!   panic の知らせとログの1行は safe-log で書く。使い方・誤り・init と open の結果は、コマンドの案内
//!   （画面の文）として、このファイルだけで書く（ソースの検査の許可リスト）。
//! - 終了コード：0 ＝ 正常（stdin の EOF・init の成功・--help）、1 ＝ 保管庫を開けない・作れない・入出力のエラー、
//!   2 ＝ 引数の誤り。
#![forbid(unsafe_code)]

pub mod args;
pub mod host;

use std::io;
use std::path::Path;
use std::process::ExitCode;

use mcp_proto::{Server, ServerInfo};
use safe_log::{Line, Name, name};
use vault_core::{Actor, ErrorCode, ErrorDetail, RootEnv, Vault, VaultError};

use crate::args::{Command, USAGE};
use crate::host::VaultHost;

/// サーバーの名前（serverInfo）。
const SERVER_NAME: &str = "vault-mcp";
/// ログと panic の知らせの書き手の名前。
const LOG_SOURCE: Name = name!("vault-mcp");
/// server/discover の instructions。
const INSTRUCTIONS: &str =
    "保管庫の中のファイルを相対パスで読み書きする。削除と上書きはゴミ箱から戻せる。";

/// プログラムの入口（`main.rs` から呼ぶ）。
pub fn run() -> ExitCode {
    // panic の文（場所・中身）は出さない。決まった1行（「vault-mcp: 内部のエラー（panic。詳しい文は出さない）」）だけ
    safe_log::set_quiet_panic_hook(LOG_SOURCE, false);
    match args::parse(std::env::args_os().skip(1).collect()) {
        Ok(Command::Help) => {
            eprintln!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::Init(path)) => init(&path),
        Ok(Command::Serve {
            vault,
            read_only,
            actor_id,
        }) => serve(&vault, read_only, actor_id),
        Err(problem) => {
            eprintln!("vault-mcp: 引数の誤り：{problem}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `vault-mcp init <絶対パス>`：保管庫を作って終わる（actor は `{user, cli}`）。
fn init(path: &Path) -> ExitCode {
    let result =
        RootEnv::from_system().and_then(|env| Vault::init(path, &env, &Actor::user("cli")));
    match result {
        Ok(()) => {
            eprintln!("vault-mcp: 保管庫を作った");
            ExitCode::SUCCESS
        }
        Err(e) => fail("保管庫を作れない", &e),
    }
}

/// `vault-mcp --vault <絶対パス>`：保管庫を開いて（作らない）、MCP のサーバーとして動く。stdin の EOF で終わる。
fn serve(path: &Path, read_only: bool, actor_id: Option<String>) -> ExitCode {
    let vault = match RootEnv::from_system().and_then(|env| Vault::open(path, read_only, &env)) {
        Ok(vault) => vault,
        Err(e) => return fail("保管庫を開けない", &e),
    };
    let mut server = Server::new(
        VaultHost::new(vault),
        ServerInfo {
            name: SERVER_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            instructions: INSTRUCTIONS.to_owned(),
        },
    )
    .with_actor_override(actor_id);
    match server.serve(io::stdin().lock(), io::stdout().lock(), io::stderr()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // エラーの文（io::Error の表示）は書かない。種類の名前だけ
            let line = Line::new(LOG_SOURCE)
                .name(name!("event"), name!("io_error"))
                .io_kind(name!("kind"), e.kind());
            eprintln!("{line}");
            ExitCode::from(1)
        }
    }
}

/// 失敗を stderr に出す（vault-core のエラーの文は「CODE: 説明」で、絶対パスを含まない）。
fn fail(what: &str, e: &VaultError) -> ExitCode {
    eprintln!("vault-mcp: {what}：{e}{}", hint(e));
    ExitCode::from(1)
}

/// 失敗の文に添えるヒント。すでに保管庫かどうかは、文の書き出しではなく、エラーの印（`ErrorDetail`）で見分ける。
fn hint(e: &VaultError) -> &'static str {
    match (e.code(), e.detail()) {
        (ErrorCode::NotAVault, _) => "（保管庫は、先に vault-mcp init <絶対パス> かアプリで作る）",
        // すでに保管庫のフォルダへの init は、文だけで足りる（場所の誤りではないので、場所のヒントを付けない）
        (ErrorCode::InvalidRoot, Some(ErrorDetail::AlreadyAVault)) => "",
        (ErrorCode::InvalidRoot, _) => "（絶対パスで、保管庫にできる場所を渡す。vault-mcp --help）",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vault_core::test_support::error;

    const PLACE_HINT: &str = "vault-mcp --help";

    /// すでに保管庫は印で見分ける：文の書き出しを変えても、場所のヒントを付けない。
    #[test]
    fn already_a_vault_is_told_by_its_detail_not_by_its_message() {
        let changed = error(
            ErrorCode::InvalidRoot,
            "書き出しを変えた文（.vault がある）",
            Some(ErrorDetail::AlreadyAVault),
        );
        assert_eq!(hint(&changed), "");
        let empty = error(ErrorCode::InvalidRoot, "", Some(ErrorDetail::AlreadyAVault));
        assert_eq!(hint(&empty), "");
    }

    /// 文が「すでに保管庫」で始まっても、印がなければ、ほかの INVALID_ROOT と同じく場所のヒントを付ける。
    #[test]
    fn a_message_alone_does_not_count_as_already_a_vault() {
        let lookalike = error(
            ErrorCode::InvalidRoot,
            "すでに保管庫になっている（.vault がある）。作り直さずに、そのまま開いて使う",
            None,
        );
        assert!(hint(&lookalike).contains(PLACE_HINT));
        let other = error(
            ErrorCode::InvalidRoot,
            "空でないフォルダは保管庫にできない",
            None,
        );
        assert!(hint(&other).contains(PLACE_HINT));
    }

    /// ほかのコードのヒントは今のまま。
    #[test]
    fn hints_for_other_codes_are_unchanged() {
        let not_a_vault = error(ErrorCode::NotAVault, "保管庫でない", None);
        assert!(hint(&not_a_vault).contains("vault-mcp init"));
        let locked = error(ErrorCode::Locked, "ロックを取れない", None);
        assert_eq!(hint(&locked), "");
    }
}
