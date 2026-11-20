//! 自作の MCP 層（stdio・改行区切りの JSON-RPC 2.0、Modern と Legacy の両対応）。
//!
//! 保管庫のことは知らない（vault-core に依存しない）。ツールの一覧と、ツールを呼ぶ処理（受付口）は、
//! [`ToolHost`] として外から渡す。vault-mcpが、vault-core の `api::tool_specs()` と `api::call` でつなぐ。
//!
//! - stdout には MCP のメッセージだけを、1行に1つ書く。stderr（`serve` の `log`）には、メソッド名・ツール名・
//!   結果のコード・かかった時間だけを書く（ファイルの中身とツールの引数は書かない）。1行は safe-log で組み立て、
//!   メソッド名は扱うメソッドの一覧、ツール名とコードは受付口の `log_names` にあるときだけ書く。ほかは `?`。
//! - 処理は1本のスレッドで、同期して行う。stdin が閉じられたら（EOF）終わる。
//! - actor：MCP の通り道から来た操作なので、kind は常に `ai`（受付口が決める）。id は `mcp:<名前>` で、
//!   名前は上書き（`--actor-id`）があればそれ、なければ Modern は要求ごとの `_meta` の clientInfo.name、
//!   Legacy は `initialize` の clientInfo.name。印字できる ASCII だけにして 64 字で切り、空なら `unknown`。
#![forbid(unsafe_code)]

mod limit;
mod line;
mod server;

pub use limit::{RATE_LIMIT_BURST, RATE_LIMIT_PER_SECOND};
pub use line::MAX_LINE_BYTES;
/// 受付口（`ToolHost::log_names`）の一覧を作るための名前の型とマクロ（safe-log のもの。外で受付口を実装する側が、
/// safe-log に直に依存しなくて済むように出す）。
pub use safe_log::{Name, name};
pub use server::{
    CallError, DEFAULT_LEGACY_VERSION, LEGACY_VERSIONS, MAX_ACTOR_NAME_CHARS, MAX_JSON_ELEMENTS,
    MODERN_VERSION, Server, ServerInfo, ToolDef, ToolHost, UNKNOWN_ACTOR_NAME, clean_actor_name,
    codes,
};
