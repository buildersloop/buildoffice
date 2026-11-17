//! vault-mcp の結合テストの共通の道具。本物のバイナリ（`env!("CARGO_BIN_EXE_vault-mcp")`）を起動する。
//! 保管庫は `env!("CARGO_TARGET_TMPDIR")` の下にだけ作り、`TestDir` の Drop で片づける。

// テストのファイルごとに使う道具が違うので、使わないものがあっても警告にしない
#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub const EXE: &str = env!("CARGO_BIN_EXE_vault-mcp");
/// テストだけの panic の入口（debug ビルドで、この環境変数にツールの名前を入れる）。
pub const TEST_PANIC_ENV: &str = "VAULT_MCP_TEST_PANIC_TOOL";

/// テストの場所。
pub fn tests_base() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("vault-mcp-tests")
}

/// テストごとのフォルダ。Drop で中身ごと消す（テストの場所の下だけ）。
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new(name: &str) -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = tests_base().join(format!(
            "{name}-{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let base = tests_base();
        if self.path.starts_with(&base) && self.path != base {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// バイナリを、引数を付けて走らせ、終わるまで待つ（stdin は空）。
pub fn run(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(EXE)
        .args(args)
        .env_remove(TEST_PANIC_ENV)
        .stdin(Stdio::null())
        .output()
        .expect("vault-mcp を起動できない")
}

/// `vault-mcp init <dir>/<name>` で保管庫を作り、そのパスを返す。
pub fn init_vault(dir: &TestDir, name: &str) -> PathBuf {
    let path = dir.join(name);
    let out = run(&["init".as_ref(), path.as_os_str()]);
    assert!(
        out.status.success(),
        "init が失敗した：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    path
}

/// 動いている vault-mcp のプロセス。stdout は別のスレッドで1行ずつ読む（返事を待つときに時間切れにできるように）。
pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Option<JoinHandle<String>>,
}

/// プロセスを終えたあとに残ったもの。
pub struct Finished {
    pub status: ExitStatus,
    pub stderr: String,
    /// 終えるまでに読まなかった stdout の行。
    pub rest: Vec<String>,
}

impl Mcp {
    /// `vault-mcp --vault <vault> <extra…>` を起動する。
    pub fn start(vault: &Path, extra: &[&str]) -> Self {
        Self::start_with_env(vault, extra, None)
    }

    pub fn start_with_env(vault: &Path, extra: &[&str], env: Option<(&str, &str)>) -> Self {
        let mut command = Command::new(EXE);
        command
            .arg("--vault")
            .arg(vault)
            .args(extra)
            .env_remove(TEST_PANIC_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((key, value)) = env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("vault-mcp を起動できない");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        Self {
            child,
            stdin,
            lines,
            stderr: Some(stderr),
        }
    }

    /// 1行を送る（返事を待たない）。
    pub fn send_only(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin はもう閉じた");
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    /// 1行を送り、返事を1行読む（20 秒で時間切れ）。
    pub fn send(&mut self, line: &str) -> Value {
        self.send_only(line);
        let reply = self
            .lines
            .recv_timeout(Duration::from_secs(20))
            .expect("返事が来ない");
        serde_json::from_str(&reply).unwrap_or_else(|e| panic!("{e}: {reply}"))
    }

    /// stdin を閉じ、終わるまで待つ。
    pub fn finish(mut self) -> Finished {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        let stderr = self.stderr.take().unwrap().join().unwrap();
        let rest = self.lines.try_iter().collect();
        Finished {
            status,
            stderr,
            rest,
        }
    }
}

/// Modern の `_meta`（name が None なら clientInfo を付けない）。
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

pub fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// Modern の tools/call。
pub fn modern_call(id: u64, tool: &str, arguments: Value, name: Option<&str>) -> String {
    request(
        id,
        "tools/call",
        json!({"name": tool, "arguments": arguments, "_meta": meta(name)}),
    )
}

/// Legacy（initialize のあと）の tools/call。
pub fn legacy_call(id: u64, tool: &str, arguments: Value) -> String {
    request(
        id,
        "tools/call",
        json!({"name": tool, "arguments": arguments}),
    )
}

pub fn initialize(id: u64, version: &str, name: Option<&str>) -> String {
    let mut params = json!({"protocolVersion": version, "capabilities": {}});
    if let Some(name) = name {
        params["clientInfo"] = json!({"name": name, "version": "1.0"});
    }
    request(id, "initialize", params)
}

/// tools/call の結果の中身（structuredContent があればそれ、なければ text を JSON として読む）。
pub fn tool_value(reply: &Value) -> Value {
    let result = &reply["result"];
    assert!(result.is_object(), "結果がない：{reply}");
    if let Some(structured) = result.get("structuredContent") {
        return structured.clone();
    }
    let text = result["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap_or_else(|_| json!({"text": text}))
}

/// tools/call が成功したことを確かめて、中身を返す。
pub fn ok(reply: &Value) -> Value {
    assert_eq!(reply["result"]["isError"], false, "失敗した：{reply}");
    tool_value(reply)
}

/// tools/call がツールのエラーになったことを確かめて、コードを返す。
pub fn tool_error_code(reply: &Value) -> String {
    assert_eq!(reply["result"]["isError"], true, "成功した：{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    text.split(':').next().unwrap().to_owned()
}

/// 保管庫の履歴をすべて読む（ファイルの順・行の順）。
pub fn journal(vault: &Path) -> Vec<Value> {
    let dir = vault.join(".vault").join("journal");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|file| {
            fs::read_to_string(file)
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// 出力に出てはいけない文字（保管庫の絶対パス・テストの場所・ユーザー名）。
pub fn secrets(vault: &Path) -> Vec<String> {
    let canon = fs::canonicalize(vault).unwrap();
    let mut needles = vec![
        vault.display().to_string(),
        canon.display().to_string(),
        vault.display().to_string().replace('\\', "/"),
        env!("CARGO_TARGET_TMPDIR").to_owned(),
        "vault-mcp-tests".to_owned(),
        r"\\?\".to_owned(),
    ];
    for var in ["USERNAME", "USER"] {
        if let Ok(name) = std::env::var(var)
            && name.len() >= 3
        {
            needles.push(name);
        }
    }
    needles
}

/// テストを飛ばすときの知らせ。
pub fn skip(test: &str, reason: &str) {
    eprintln!("[skip] {test}: {reason}");
}
