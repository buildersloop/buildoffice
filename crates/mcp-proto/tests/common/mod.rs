//! mcp-proto の結合テストの共通の道具：呼ばれたことを記録する偽の受付口と、メモリ上の入出力。

// テストのファイルごとに使う道具が違うので、使わないものがあっても警告にしない
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{self, BufRead, Cursor, Read};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use mcp_proto::{CallError, Server, ServerInfo, ToolDef, ToolHost};
use serde_json::{Value, json};

/// 受付口に届いた呼び出し。
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub arguments: Value,
    pub actor_id: String,
}

/// 偽の受付口。`fake_read`・`fake_write`（読み取り専用なら READ_ONLY）・`fake_fail`・`fake_panic` を知っている。
pub struct FakeHost {
    pub calls: Rc<RefCell<Vec<Call>>>,
    pub read_only: bool,
}

/// ツールの中身として返す、ログに出てはいけない文字。
pub const SECRET_CONTENT: &str = "secret-file-content\nsecond line";

impl ToolHost for FakeHost {
    fn tools(&self) -> Vec<ToolDef> {
        let mut tools = vec![ToolDef {
            name: "fake_read".to_owned(),
            description: "読む".to_owned(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            read_only_hint: true,
            destructive_hint: false,
        }];
        if !self.read_only {
            tools.push(ToolDef {
                name: "fake_write".to_owned(),
                description: "書く（ゴミ箱から戻せる）".to_owned(),
                input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
                read_only_hint: false,
                destructive_hint: true,
            });
        }
        tools
    }

    fn call(&mut self, name: &str, arguments: &Value, actor_id: &str) -> Result<Value, CallError> {
        self.calls.borrow_mut().push(Call {
            name: name.to_owned(),
            arguments: arguments.clone(),
            actor_id: actor_id.to_owned(),
        });
        match name {
            "fake_read" => Ok(json!({"content": SECRET_CONTENT, "size": 31})),
            "fake_write" if self.read_only => Err(CallError::Tool {
                code: "READ_ONLY".to_owned(),
                message: "読み取り専用で開いた保管庫には書けない".to_owned(),
            }),
            "fake_write" => Ok(json!({"path": "a.txt", "created": true})),
            "fake_fail" => Err(CallError::Tool {
                code: "NOT_FOUND".to_owned(),
                message: "ない：a.txt".to_owned(),
            }),
            "fake_panic" => panic!("偽の受付口が止まった（テスト）"),
            "fake_scalar" => Ok(json!(1)),
            _ => Err(CallError::UnknownTool),
        }
    }
}

/// 時間を進められる時計。
#[derive(Clone)]
pub struct FakeClock(Rc<Cell<Instant>>);

impl FakeClock {
    pub fn new() -> Self {
        Self(Rc::new(Cell::new(Instant::now())))
    }

    pub fn now(&self) -> Instant {
        self.0.get()
    }

    pub fn advance(&self, by: Duration) {
        self.0.set(self.0.get() + by);
    }
}

/// テストのサーバー（時計は止まったまま）。
pub struct Fixture {
    pub server: Server<FakeHost>,
    pub calls: Rc<RefCell<Vec<Call>>>,
    pub clock: FakeClock,
}

pub fn fixture() -> Fixture {
    fixture_with(false, None)
}

pub fn fixture_with(read_only: bool, actor_override: Option<&str>) -> Fixture {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let clock = FakeClock::new();
    let tick = clock.clone();
    let server = Server::new(
        FakeHost {
            calls: Rc::clone(&calls),
            read_only,
        },
        ServerInfo {
            name: "vault-mcp".to_owned(),
            version: "0.1.0".to_owned(),
            instructions:
                "保管庫の中のファイルを相対パスで読み書きする。削除と上書きはゴミ箱から戻せる。"
                    .to_owned(),
        },
    )
    .with_actor_override(actor_override.map(str::to_owned))
    .with_clock(move || tick.now());
    Fixture {
        server,
        calls,
        clock,
    }
}

/// 送った行に対する、返事（1行ずつ）とログ。
pub struct Run {
    pub replies: Vec<Value>,
    pub log: String,
}

impl Fixture {
    /// 行を送る（最後に改行を付けて、EOF まで）。stdout の各行が1つの JSON で、中に改行がないことも確かめる。
    pub fn send(&mut self, lines: &[String]) -> Run {
        let mut input = lines.join("\n");
        input.push('\n');
        self.send_raw(input.into_bytes())
    }

    pub fn send_raw(&mut self, input: Vec<u8>) -> Run {
        self.send_from(Cursor::new(input))
    }

    /// 任意の入力から送る。
    pub fn send_from(&mut self, input: impl BufRead) -> Run {
        let mut out = Vec::new();
        let mut log = Vec::new();
        self.server.serve(input, &mut out, &mut log).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.is_empty() || text.ends_with('\n'));
        let replies = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
            .collect();
        Run {
            replies,
            log: String::from_utf8(log).unwrap(),
        }
    }

    /// 1行を送り、返事が1つであることを確かめて返す。
    pub fn one(&mut self, line: String) -> Value {
        let run = self.send(&[line]);
        assert_eq!(run.replies.len(), 1, "返事は1つ：{:?}", run.replies);
        run.replies.into_iter().next().unwrap()
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
}

/// 止まって読む入力：かたまりを1つ返すたびに、次のかたまりの前に `delay` だけ待つ。
pub struct SlowInput {
    chunks: VecDeque<Vec<u8>>,
    current: Vec<u8>,
    pos: usize,
    delay: Duration,
}

impl SlowInput {
    pub fn new(chunks: Vec<Vec<u8>>, delay: Duration) -> Self {
        Self {
            chunks: chunks.into(),
            current: Vec::new(),
            pos: 0,
            delay,
        }
    }
}

impl Read for SlowInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for SlowInput {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.pos >= self.current.len() {
            let Some(next) = self.chunks.pop_front() else {
                return Ok(&[]);
            };
            thread::sleep(self.delay);
            self.current = next;
            self.pos = 0;
        }
        Ok(&self.current[self.pos..])
    }

    fn consume(&mut self, amount: usize) {
        self.pos += amount;
    }
}

/// ログの1行の ms。
pub fn log_ms(line: &str) -> u128 {
    line.rsplit("ms=").next().unwrap().trim().parse().unwrap()
}

/// Modern の `_meta`（clientInfo の name を渡せる。None なら clientInfo を付けない）。
pub fn meta(name: Option<&str>) -> Value {
    let mut meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    if let Some(name) = name {
        meta["io.modelcontextprotocol/clientInfo"] = json!({"name": name, "version": "1.0"});
    }
    meta
}

/// 要求の行。
pub fn request(id: impl Into<Value>, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id.into(), "method": method, "params": params}).to_string()
}

/// Modern の要求の行（params に `_meta` を足す）。
pub fn modern(id: impl Into<Value>, method: &str, mut params: Value, name: Option<&str>) -> String {
    params["_meta"] = meta(name);
    request(id, method, params)
}

/// 通知の行。
pub fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

/// Legacy の initialize の行。
pub fn initialize(id: impl Into<Value>, version: &str, name: Option<&str>) -> String {
    let mut params = json!({"protocolVersion": version, "capabilities": {}});
    if let Some(name) = name {
        params["clientInfo"] = json!({"name": name, "version": "1.0"});
    }
    request(id, "initialize", params)
}

/// エラーの番号（なければ None）。
pub fn error_code(reply: &Value) -> Option<i64> {
    reply.get("error").and_then(|e| e["code"].as_i64())
}

/// arguments に、入れ子が `depth` 段の配列を入れた tools/call の行。文字で組み立てる
/// （serde_json の値で作ると、書き出すときにも深さの上限にかかるため）。`meta_name` があれば Modern の `_meta` を付ける。
pub fn deep_call(id: u64, depth: usize, meta_name: Option<&str>) -> String {
    let meta = meta_name
        .map(|name| format!(",\"_meta\":{}", meta(Some(name))))
        .unwrap_or_default();
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"fake_read\"{meta},\"arguments\":{{\"a\":{}{}}}}}}}",
        "[".repeat(depth),
        "]".repeat(depth)
    )
}
