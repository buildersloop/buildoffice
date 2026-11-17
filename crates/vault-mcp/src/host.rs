//! ツールの受付口（mcp-proto の `ToolHost`）を、vault-core の `tool_specs()` と `api::call` でつなぐ。
//!
//! - actor は `Actor::ai(actor_id)`：MCP の通り道から来た操作なので、kind は常に `ai`。
//! - 引数（`&Value`）は、写さずに `api::call` に渡す。
//! - 読み取り専用のときは、書くツール（write・mkdir・move・delete・restore）を tools/list に出さない。
//!   呼ばれたら、vault-core が `READ_ONLY` を返す。
//! - ツールの処理が途中で止まった（panic）ら、このプロセスでは、それ以降の書くツールを止める（`INTERNAL`）。
//!   読むツールと vault_changes は続けて使える。

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use mcp_proto::{CallError, ToolDef, ToolHost};
use serde_json::Value;
use vault_core::api::{self, WRITE_TOOLS};
use vault_core::{Actor, ErrorCode, Vault};

/// テストだけ：feature `test-panic` の debug ビルドのとき、この環境変数にツールの名前を入れると、そのツールの呼び出しで
/// panic する（panic の文が stderr に出ないこと・panic のあとに書き込みを止めることを、本物のバイナリで確かめるため）。
/// release のビルドには、どんなビルドのしかたでも（`--all-targets`・`cargo test --release` でも）、入口も環境変数の名前も入らない。
#[cfg(all(feature = "test-panic", debug_assertions))]
pub const TEST_PANIC_ENV: &str = "VAULT_MCP_TEST_PANIC_TOOL";

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
}
