//! vault-mcp のプログラム。中身は `lib.rs`（`vault_mcp::run`）。
#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    vault_mcp::run()
}
