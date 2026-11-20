//! 読み込んだ Value のメモリの測定（安全の確認での指摘）。ふつうのテストでは動かさない。
//!
//! 外から、このテストのプロセスの最大の作業セット（PeakWorkingSet64）を見る。処理のあと少し待つので、そのあいだに読む。
//! `cargo test --release -p mcp-proto --test memory -- --ignored --nocapture`

mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::fixture;
use mcp_proto::{CallError, Name, Server, ServerInfo, ToolDef, ToolHost};
use serde_json::{Value, json};

/// 引数を写さない受付口（サーバーそのもののメモリを測るため。偽の受付口は、記録のために引数を写す）。
struct CountingHost;

impl ToolHost for CountingHost {
    fn tools(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    fn call(
        &mut self,
        _name: &str,
        arguments: &Value,
        _actor_id: &str,
    ) -> Result<Value, CallError> {
        let items = arguments["a"]
            .as_array()
            .map(Vec::len)
            .or_else(|| arguments["a"].as_object().map(serde_json::Map::len))
            .unwrap_or(0);
        Ok(json!({ "items": items }))
    }

    fn log_names(&self) -> &[Name] {
        &[]
    }
}

/// arguments に `[0,0,…]`（1行がほぼ 32 MiB）を入れた tools/call の行。
fn big_array_line() -> String {
    let head = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fake_read","_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"arguments":{"a":["#;
    let tail = "0]}}}";
    let budget = 32 * 1024 * 1024 - head.len() - tail.len();
    let mut line = String::with_capacity(32 * 1024 * 1024);
    line.push_str(head);
    line.push_str(&"0,".repeat(budget / 2));
    line.push_str(tail);
    assert!(line.len() <= 32 * 1024 * 1024);
    line
}

#[test]
#[ignore = "メモリを測る（外から最大の作業セットを見る）"]
fn memory_of_a_32_mib_array_line() {
    let line = big_array_line();
    let mut f = fixture();
    let start = Instant::now();
    let run = f.send(&[line]);
    println!(
        "32 MiB の [0,0,…]：返事 {}／受付口に届いた {} 回／{:?}",
        run.replies[0],
        f.calls().len(),
        start.elapsed()
    );
    thread::sleep(Duration::from_millis(1500));
}

/// 上限ちょうど（文字列の外の [・{・, が 100 万個）の行。`object` なら `{"k0":0,"k1":0,…}`、そうでなければ `[0,0,…]`。
fn line_at_the_limit(object: bool) -> String {
    let head = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fake_read","_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"arguments":{"a":"#;
    let tail = "}}}";
    let outside = |s: &str| s.chars().filter(|c| matches!(c, '[' | '{' | ',')).count();
    // 中の [ か { の1つと、要素のあいだの , が「要素の数 - 1」
    let items = 1_000_000 - outside(head) - outside(tail);
    let mut line = String::from(head);
    if object {
        line.push('{');
        let members: Vec<String> = (0..items).map(|i| format!(r#""k{i}":0"#)).collect();
        line.push_str(&members.join(","));
        line.push('}');
    } else {
        line.push('[');
        line.push_str(&"0,".repeat(items - 1));
        line.push_str("0]");
    }
    line.push_str(tail);
    line
}

/// 引数を写さない受付口のサーバーで、1行を処理する（入力は、テストが持つ1行と、読みの1行ぶんの場所を使う）。
fn measure(label: &str, line: String) {
    let mut server = Server::new(
        CountingHost,
        ServerInfo {
            name: "vault-mcp".to_owned(),
            version: "0.1.0".to_owned(),
            instructions: String::new(),
        },
    );
    let mut input = line.into_bytes();
    input.push(b'\n');
    let mut out = Vec::new();
    let start = Instant::now();
    server
        .serve(std::io::Cursor::new(input), &mut out, std::io::sink())
        .unwrap();
    let elapsed = start.elapsed();
    let reply = String::from_utf8_lossy(&out);
    let found = reply
        .find("items")
        .map_or("", |i| &reply[i..reply.len().min(i + 20)]);
    println!("{label}：返事 {found}…／{elapsed:?}");
    thread::sleep(Duration::from_millis(1500));
}

#[test]
#[ignore = "メモリを測る（外から最大の作業セットを見る）"]
fn memory_at_the_limit_with_an_array() {
    measure(
        "要素 100 万個（上限ちょうど）の配列 [0,0,…]",
        line_at_the_limit(false),
    );
}

#[test]
#[ignore = "メモリを測る（外から最大の作業セットを見る）"]
fn memory_at_the_limit_with_an_object() {
    measure(
        "要素 100 万個（上限ちょうど）のオブジェクト {\"k0\":0,…}",
        line_at_the_limit(true),
    );
}
