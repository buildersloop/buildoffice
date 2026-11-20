//! ツールの受付口（mcp-proto の `ToolHost`）を、vault-core の `tool_specs()` と `api::call` でつなぐ。
//!
//! - actor は `Actor::ai(actor_id)`：MCP の通り道から来た操作なので、kind は常に `ai`。
//! - 引数（`&Value`）は、写さずに `api::call` に渡す。
//! - 読み取り専用のときは、書くツール（write・mkdir・move・delete・restore）を tools/list に出さない。
//!   呼ばれたら、vault-core が `READ_ONLY` を返す。
//! - ツールの処理が途中で止まった（panic）ら、このプロセスでは、それ以降の書くツールを止める（`INTERNAL`）。
//!   読むツールと vault_changes は続けて使える。
//! - ログに書いてよい名前（`log_names`）は、10 個のツールの名前と 24 個のエラーのコード。ソースに書いた一覧で、
//!   `tool_specs()` の名前とコードの名前に合うことをテストで確かめる。

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use mcp_proto::{CallError, ToolDef, ToolHost};
use safe_log::{Name, name};
use serde_json::Value;
use vault_core::api::{self, WRITE_TOOLS};
use vault_core::{Actor, ErrorCode, Vault};

/// テストだけ：feature `test-panic` の debug ビルドのとき、この環境変数にツールの名前を入れると、そのツールの呼び出しで
/// panic する（panic の文が stderr に出ないこと・panic のあとに書き込みを止めることを、本物のバイナリで確かめるため）。
/// release のビルドには、どんなビルドのしかたでも（`--all-targets`・`cargo test --release` でも）、入口も環境変数の名前も入らない。
#[cfg(all(feature = "test-panic", debug_assertions))]
pub const TEST_PANIC_ENV: &str = "VAULT_MCP_TEST_PANIC_TOOL";

/// ログに書いてよい名前：ツールの名前（`api::tool_specs()` と同じ 10 個）と、エラーのコード（`ErrorCode` の 24 個）。
/// ここにない名前は、ログで `?` になる（返事は変わらない）。ツールかコードを足したら、ここにも足す（テストで気づく）。
pub const LOG_NAMES: &[Name] = &[
    name!("vault_info"),
    name!("vault_list"),
    name!("vault_read"),
    name!("vault_write"),
    name!("vault_mkdir"),
    name!("vault_move"),
    name!("vault_delete"),
    name!("vault_trash_list"),
    name!("vault_restore"),
    name!("vault_changes"),
    name!("INVALID_PATH"),
    name!("OUTSIDE_VAULT"),
    name!("RESERVED_PATH"),
    name!("INVALID_ROOT"),
    name!("NOT_A_VAULT"),
    name!("LOCKED"),
    name!("INVALID_ARGUMENT"),
    name!("IO_ERROR"),
    name!("INTERNAL"),
    name!("NOT_FOUND"),
    name!("NOT_A_DIRECTORY"),
    name!("NOT_A_FILE"),
    name!("NOT_UTF8"),
    name!("TOO_LARGE"),
    name!("ALREADY_EXISTS"),
    name!("CONFLICT"),
    name!("DISK_FULL"),
    name!("BUSY"),
    name!("READ_ONLY"),
    name!("UNKNOWN_TOOL"),
    name!("NOT_IMPLEMENTED"),
    name!("ACCESS_DENIED"),
    name!("DIR_NOT_EMPTY"),
    name!("INVALID_TRASH_ENTRY"),
];

/// 保管庫につないだ受付口。
pub struct VaultHost {
    vault: Vault,
    /// 前の呼び出しが途中で止まった（panic）か。止まったあとは、書くツールを止める。
    writes_stopped: bool,
}

impl VaultHost {
    pub fn new(vault: Vault) -> Self {
        Self {
            vault,
            writes_stopped: false,
        }
    }
}

impl ToolHost for VaultHost {
    fn tools(&self) -> Vec<ToolDef> {
        let read_only = self.vault.is_read_only();
        api::tool_specs()
            .into_iter()
            .filter(|spec| !(read_only && spec.writes))
            .map(|spec| ToolDef {
                name: spec.name.to_owned(),
                description: spec.description.to_owned(),
                input_schema: spec.input_schema,
                read_only_hint: spec.read_only_hint,
                destructive_hint: spec.destructive_hint,
            })
            .collect()
    }

    fn call(&mut self, name: &str, arguments: &Value, actor_id: &str) -> Result<Value, CallError> {
        if self.writes_stopped && WRITE_TOOLS.contains(&name) {
            return Err(CallError::Tool {
                code: ErrorCode::Internal.as_str().to_owned(),
                message: "前の呼び出しで内部のエラーが起きたので、このサーバーでは書き込みを止めた。MCP のサーバーを起動し直す".to_owned(),
            });
        }
        let actor = Actor::ai(actor_id);
        let vault = &self.vault;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            #[cfg(all(feature = "test-panic", debug_assertions))]
            if std::env::var_os(TEST_PANIC_ENV).is_some_and(|tool| tool == name) {
                panic!("テスト用の panic：C:\\Profiles\\secret-user\\PANIC-SECRET.txt");
            }
            api::call(vault, name, arguments, &actor)
        }));
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) if e.code() == ErrorCode::UnknownTool => Err(CallError::UnknownTool),
            Ok(Err(e)) => Err(CallError::Tool {
                code: e.code().as_str().to_owned(),
                message: e.message().to_owned(),
            }),
            Err(payload) => {
                // 途中で止まった：保管庫の中が、履歴と索引に合っていないかもしれないので、書き込みを止める
                self.writes_stopped = true;
                resume_unwind(payload)
            }
        }
    }

    fn log_names(&self) -> &[Name] {
        LOG_NAMES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一覧のツールの名前は `tool_specs()` の名前とちょうど同じ（順番も）。
    #[test]
    fn log_names_cover_every_tool() {
        let tools: Vec<&str> = api::tool_specs().iter().map(|spec| spec.name).collect();
        let listed: Vec<&str> = LOG_NAMES[..tools.len()]
            .iter()
            .map(|name| name.as_str())
            .collect();
        assert_eq!(listed, tools);
        for tool in &tools {
            assert!(safe_log::pick(tool, LOG_NAMES).is_some(), "{tool}");
        }
    }

    /// 一覧のコードは。
    #[test]
    fn log_names_cover_every_error_code() {
        let codes = [
            ErrorCode::InvalidPath,
            ErrorCode::OutsideVault,
            ErrorCode::ReservedPath,
            ErrorCode::InvalidRoot,
            ErrorCode::NotAVault,
            ErrorCode::Locked,
            ErrorCode::InvalidArgument,
            ErrorCode::IoError,
            ErrorCode::Internal,
            ErrorCode::NotFound,
            ErrorCode::NotADirectory,
            ErrorCode::NotAFile,
            ErrorCode::NotUtf8,
            ErrorCode::TooLarge,
            ErrorCode::AlreadyExists,
            ErrorCode::Conflict,
            ErrorCode::DiskFull,
            ErrorCode::Busy,
            ErrorCode::ReadOnly,
            ErrorCode::UnknownTool,
            ErrorCode::NotImplemented,
            ErrorCode::AccessDenied,
            ErrorCode::DirNotEmpty,
            ErrorCode::InvalidTrashEntry,
        ];
        let tools = api::tool_specs().len();
        let listed: Vec<&str> = LOG_NAMES[tools..]
            .iter()
            .map(|name| name.as_str())
            .collect();
        let names: Vec<&str> = codes.iter().map(|code| code.as_str()).collect();
        assert_eq!(listed, names);
        assert_eq!(LOG_NAMES.len(), 10 + 24);
    }
}
