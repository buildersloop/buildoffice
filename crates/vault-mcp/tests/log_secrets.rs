//! ログに秘密が出ないことの結合テスト。本物のバイナリを起動する。
//!
//! 偽の秘密（絶対パス・ユーザー名・IP・鍵・招待コード・短い数字・SID・ホスト名）を、クライアントが送れる所
//! （メソッド名・ツール名・引数・id・clientInfo.name・protocolVersion・`_meta`）と、起動の引数（保管庫のパス・
//! `--actor-id`）に入れても、stderr には1つも出ないこと。stderr の各行は `mcp: method=… tool=… result=… ms=…` の形だけ。

mod common;

use std::fs;

use common::{Mcp, TestDir, init_vault, ok, run, secrets, tool_error_code};
use serde_json::{Value, json};

/// 偽の秘密。どれも、ログに出てはいけない。
const FAKES: [&str; 18] = [
    r"C:\Profiles\SECRET-USER-q7\a.txt",
    r"\\srv\share",
    "/home/secret-user-q7",
    "192.168.77.88",
    "fe80::1",
    "[2001:db8::1]:443",
    // 64 字の16進（先頭が英字と数字の2つ）
    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    // 44 字の base64（`=` あり・なし）
    "n4bQgYhMfWWaL+qgxVrQFaO/TxsrC4Is0V1sFbDwCgg=",
    "Zm9vYmFyQmF6UXV4UTdLZXlTZWNyZXRWYWx1ZTEyMzQ1",
    "山田太郎",
    "ABCD-EFGH-JKLM-NPQR",
    "12345678",
    "S-1-5-21-1004336348-1177238915-682003330-1001",
    "secret-host-q7.example",
    "home/alice",
    "",
    "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
];

/// stderr に出てはいけない断片（大文字と小文字を区別せずに探す）。偽の秘密の一部でも出たら失敗にする。
const FRAGMENTS: [&str; 21] = [
    "secret",
    "q7",
    "profiles",
    "srv",
    "share",
    "/home",
    "77.88",
    "192.168",
    "fe80",
    "db8",
    "deadbeef",
    "9f86d081",
    "n4bqgyhm",
    "zm9vymfy",
    "山田",
    "abcd-efgh",
    "12345678",
    "s-1-5-21",
    ".example",
    "alice",
    "mmmmmmmm",
];

/// 1行が `mcp: method=\S+ tool=\S+ result=\S+ ms=\d+` に合うか（正規表現は使わない）。
fn is_log_line(line: &str) -> bool {
    let parts: Vec<&str> = line.split(' ').collect();
    if parts.len() != 5 || parts[0] != "mcp:" {
        return false;
    }
    let field = |part: &str, key: &str| -> Option<String> {
        part.strip_prefix(key)
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_whitespace))
            .map(str::to_owned)
    };
    field(parts[1], "method=").is_some()
        && field(parts[2], "tool=").is_some()
        && field(parts[3], "result=").is_some()
        && field(parts[4], "ms=").is_some_and(|ms| ms.chars().all(|c| c.is_ascii_digit()))
}

/// 偽の秘密も、その断片も、絶対パスとユーザー名も、`text` に出ていないこと。
fn assert_no_secrets(text: &str, vault: &std::path::Path) {
    let lower = text.to_lowercase();
    for fake in FAKES.iter().filter(|fake| fake.chars().count() >= 4) {
        assert!(
            !lower.contains(&fake.to_lowercase()),
            "{fake:?} が出た：{text}"
        );
    }
    for fragment in FRAGMENTS {
        assert!(!lower.contains(fragment), "{fragment:?} が出た：{text}");
    }
    for needle in secrets(vault) {
        assert!(
            !lower.contains(&needle.to_lowercase()),
            "{needle:?} が出た：{text}"
        );
    }
}

/// Modern の `_meta`。clientInfo.name と、知らない項目にも偽の値を入れる。
fn fake_meta(fake: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {"x-fake": fake},
        "io.modelcontextprotocol/clientInfo": {"name": fake, "version": fake},
        "x-fake": fake,
    })
}

fn line(id: Value, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// 偽の秘密を、クライアントが送れる所と起動の引数に入れても、stderr には出ない。各行は決まった形で、
/// 知っているツールの行には、そのツールの名前が出る。
#[test]
fn fake_secrets_never_reach_stderr() {
    let dir = TestDir::new("log-secrets");
    let path = init_vault(&dir, "SECRET-USER-q7");
    let mut mcp = Mcp::start(&path, &["--actor-id", "SECRET-ACTOR-q7@192.168.77.88"]);
    let mut sent = 0;

    // Legacy の initialize：clientInfo.name・protocolVersion・id（文字列）に偽の値
    let reply = mcp.send(&line(
        json!("SECRET-ID-q7-192.168.77.88"),
        "initialize",
        json!({
            "protocolVersion": "192.168.77.88",
            "capabilities": {},
            "clientInfo": {"name": r"C:\Profiles\SECRET-USER-q7", "version": "fe80::1"},
        }),
    ));
    assert!(reply.get("result").is_some(), "{reply}");
    sent += 1;

    for (i, fake) in FAKES.iter().enumerate() {
        let id = json!(format!("{fake}-{i}"));
        // tools/call の name（Modern と Legacy）
        let reply = mcp.send(&line(
            id.clone(),
            "tools/call",
            json!({"name": fake, "arguments": {"path": fake}, "_meta": fake_meta(fake)}),
        ));
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        let reply = mcp.send(&line(
            id.clone(),
            "tools/call",
            json!({"name": fake, "arguments": {"path": fake}}),
        ));
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        // method（Modern と Legacy と通知）
        let reply = mcp.send(&line(id.clone(), fake, json!({"_meta": fake_meta(fake)})));
        assert!(reply.get("error").is_some(), "{reply}");
        let reply = mcp.send(&line(id, fake, json!({"x": fake})));
        assert!(reply.get("error").is_some(), "{reply}");
        mcp.send_only(
            &json!({"jsonrpc": "2.0", "method": fake, "params": {"x": fake}}).to_string(),
        );
        // Modern の版に偽の値（-32022）
        let mut meta = fake_meta(fake);
        meta["io.modelcontextprotocol/protocolVersion"] = json!(fake);
        let reply = mcp.send(&line(json!(i), "ping", json!({"_meta": meta})));
        assert!(reply.get("error").is_some(), "{reply}");
        sent += 6;
    }

    // 書く中身に偽の鍵と招待コード、パスに偽のファイル名
    let content = FAKES.join("\n");
    ok(&mcp.send(&line(
        json!("SECRET-ID-q7"),
        "tools/call",
        json!({
            "name": "vault_write",
            "arguments": {"path": "SECRET-FILE-q7.txt", "content": content},
            "_meta": fake_meta("SECRET-CLIENT-q7"),
        }),
    )));
    let read = ok(&mcp.send(&line(
        json!(1),
        "tools/call",
        json!({
            "name": "vault_read",
            "arguments": {"path": "SECRET-FILE-q7.txt"},
            "_meta": fake_meta("192.168.77.88"),
        }),
    )));
    assert_eq!(read["content"], content.as_str());
    let missing = mcp.send(&line(
        json!(2),
        "tools/call",
        json!({"name": "vault_read", "arguments": {"path": "SECRET-MISSING-q7.txt"}}),
    ));
    assert_eq!(tool_error_code(&missing), "NOT_FOUND");
    sent += 3;

    let done = mcp.finish();
    assert_eq!(done.status.code(), Some(0), "{}", done.stderr);
    let lines: Vec<&str> = done.stderr.lines().collect();
    assert_eq!(lines.len(), sent, "1要求1行：{}", done.stderr);
    for log in &lines {
        assert!(is_log_line(log), "形が違う：{log}");
    }
    assert_no_secrets(&done.stderr, &path);
    // 知っているツールの行には、そのツールの名前が出る（知らない名前は ?）
    let tail = &lines[lines.len() - 3..];
    assert!(
        tail[0].starts_with("mcp: method=tools/call tool=vault_write result=ok ms="),
        "{}",
        tail[0]
    );
    assert!(
        tail[1].starts_with("mcp: method=tools/call tool=vault_read result=ok ms="),
        "{}",
        tail[1]
    );
    assert!(
        tail[2].starts_with("mcp: method=tools/call tool=vault_read result=NOT_FOUND ms="),
        "{}",
        tail[2]
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("mcp: method=tools/call tool=? result=-32602 ms=")),
        "{}",
        done.stderr
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("mcp: method=? tool=- result=-32601 ms=")),
        "{}",
        done.stderr
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("mcp: method=? tool=- result=notification-ignored ms=")),
        "{}",
        done.stderr
    );
}

/// init と open の失敗の文（コマンドの案内）にも、絶対パスとユーザー名と、フォルダの名前が出ない
/// （serve のときの stderr は MCP のクライアントのログに残る）。
#[test]
fn init_and_open_failures_carry_no_paths_or_user_names() {
    let dir = TestDir::new("log-secrets-cli");
    let vault = init_vault(&dir, "SECRET-USER-q7");
    // 空でないフォルダへの init
    let full = dir.join("SECRET-FULL-q7");
    fs::create_dir(&full).unwrap();
    fs::write(full.join("SECRET-FILE-q7.txt"), b"192.168.77.88").unwrap();
    // まだない深いフォルダ・保管庫でないフォルダ・ないフォルダ
    let plain = dir.join("SECRET-PLAIN-q7");
    fs::create_dir(&plain).unwrap();
    let missing = dir.join("SECRET-MISSING-q7");
    let cases: Vec<(&str, std::path::PathBuf)> = vec![
        ("init", full.clone()),
        ("init", vault.clone()),
        ("init", "SECRET-RELATIVE-q7".into()),
        ("--vault", plain.clone()),
        ("--vault", missing.clone()),
        ("--vault", "SECRET-RELATIVE-q7".into()),
    ];
    for (command, target) in &cases {
        let out = run(&[command.as_ref(), target.as_os_str()]);
        assert_eq!(out.status.code(), Some(1), "{command} {target:?}");
        assert!(out.stdout.is_empty());
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(text.starts_with("vault-mcp: "), "{text}");
        assert_no_secrets(&text, &vault);
        for needle in secrets(dir.path()) {
            assert!(!text.contains(&needle), "{needle:?} が出た：{text}");
        }
    }
    assert!(!full.join(".vault").exists());
    assert!(!plain.join(".vault").exists());
    assert!(!missing.exists());
}
