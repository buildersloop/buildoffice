//! MCP のサーバーとしての結合テスト（不変条件 1〜3）。本物のバイナリを起動する。

mod common;

use std::fs;
use std::thread;
use std::time::Duration;

use common::{
    Mcp, TestDir, init_vault, initialize, journal, legacy_call, meta, modern_call, ok, request,
    secrets, skip, tool_error_code,
};
use serde_json::{Value, json};

/// write→read→delete→trash_list→restore→changes の順に呼ぶ。`call(id, tool, arguments)` が1回の tools/call。
fn flow(mut call: impl FnMut(u64, &str, Value) -> Value) {
    let written = ok(&call(
        10,
        "vault_write",
        json!({"path": "notes/a.txt", "content": "hello"}),
    ));
    assert_eq!(written["path"], "notes/a.txt");
    assert_eq!(written["created"], true);
    let read = ok(&call(11, "vault_read", json!({"path": "notes/a.txt"})));
    assert_eq!(read["content"], "hello");
    let deleted = ok(&call(12, "vault_delete", json!({"path": "notes/a.txt"})));
    let trash_id = deleted["trash_id"].as_str().unwrap().to_owned();
    let listed = ok(&call(13, "vault_trash_list", json!({})));
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["trash_id"] == trash_id.as_str())
    );
    let restored = ok(&call(14, "vault_restore", json!({"trash_id": trash_id})));
    assert_eq!(restored["path"], "notes/a.txt");
    let changes = ok(&call(15, "vault_changes", json!({"since_seq": 0})));
    let ops: Vec<&str> = changes["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["op"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["init", "mkdir", "write", "delete", "restore"]);
}

/// Modern（_meta 付き）で、write→read→delete→trash_list→restore→changes。履歴の actor は ai、id は clientInfo から。
#[test]
fn modern_flow() {
    let dir = TestDir::new("serve-modern");
    let path = init_vault(&dir, "v");
    let mut mcp = Mcp::start(&path, &[]);
    let discover = mcp.send(&request(
        1,
        "server/discover",
        json!({"_meta": meta(Some("c"))}),
    ));
    assert_eq!(
        discover["result"]["supportedVersions"],
        json!(["2026-07-28"])
    );
    assert_eq!(
        discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "vault-mcp"
    );
    let list = mcp.send(&request(2, "tools/list", json!({"_meta": meta(Some("c"))})));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 10);
    flow(|id, tool, args| mcp.send(&modern_call(id, tool, args, Some("modern-client"))));
    assert_eq!(mcp.finish().status.code(), Some(0));
    for entry in journal(&path).iter().skip(1) {
        assert_eq!(entry["actor"]["kind"], "ai", "{entry}");
        assert_eq!(entry["actor"]["id"], "mcp:modern-client", "{entry}");
    }
}

/// Legacy（initialize のあと）で、同じ順に呼ぶ。2024-11-05 は structuredContent がないので、text から読む。
#[test]
fn legacy_flow() {
    for version in ["2025-11-25", "2024-11-05"] {
        let dir = TestDir::new("serve-legacy");
        let path = init_vault(&dir, "v");
        let mut mcp = Mcp::start(&path, &[]);
        let init = mcp.send(&initialize(1, version, Some("legacy-client")));
        assert_eq!(init["result"]["protocolVersion"], version);
        mcp.send_only(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        );
        flow(|id, tool, args| mcp.send(&legacy_call(id, tool, args)));
        assert_eq!(mcp.finish().status.code(), Some(0));
        for entry in journal(&path).iter().skip(1) {
            assert_eq!(entry["actor"]["kind"], "ai", "{version} {entry}");
            assert_eq!(
                entry["actor"]["id"], "mcp:legacy-client",
                "{version} {entry}"
            );
        }
    }
}

/// actor の kind は通り道で決まる（ai）。クライアントが _meta や clientInfo に別の kind らしい値を入れても変わらない。
/// 名前が印字できない文字だけなら mcp:unknown。--actor-id は clientInfo より優先する。
#[test]
fn actors_are_ai_with_names_from_client_info() {
    let dir = TestDir::new("serve-actor");
    let path = init_vault(&dir, "v");
    let mut mcp = Mcp::start(&path, &[]);
    let mut spoof = meta(None);
    spoof["io.modelcontextprotocol/clientInfo"] =
        json!({"name": "x", "kind": "user", "actor": {"kind": "user", "id": "app"}});
    spoof["kind"] = json!("user");
    let reply = mcp.send(&request(
        1,
        "tools/call",
        json!({"name": "vault_mkdir", "arguments": {"path": "a"}, "_meta": spoof}),
    ));
    ok(&reply);
    ok(&mcp.send(&modern_call(
        2,
        "vault_mkdir",
        json!({"path": "b"}),
        Some("\u{7}\n"),
    )));
    ok(&mcp.send(&modern_call(3, "vault_mkdir", json!({"path": "c"}), None)));
    mcp.send(&initialize(4, "2025-06-18", Some("\t日本語")));
    ok(&mcp.send(&legacy_call(5, "vault_mkdir", json!({"path": "d"}))));
    mcp.finish();

    let mut override_mcp = Mcp::start(&path, &["--actor-id", "my-tool"]);
    ok(&override_mcp.send(&modern_call(
        6,
        "vault_mkdir",
        json!({"path": "e"}),
        Some("c"),
    )));
    override_mcp.send(&initialize(7, "2025-11-25", Some("c")));
    ok(&override_mcp.send(&legacy_call(8, "vault_mkdir", json!({"path": "f"}))));
    override_mcp.finish();

    let actors: Vec<(String, String, String)> = journal(&path)
        .iter()
        .skip(1)
        .map(|e| {
            (
                e["path"].as_str().unwrap().to_owned(),
                e["actor"]["kind"].as_str().unwrap().to_owned(),
                e["actor"]["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let expected = [
        ("a", "mcp:x"),
        ("b", "mcp:unknown"),
        ("c", "mcp:unknown"),
        ("d", "mcp:unknown"),
        ("e", "mcp:my-tool"),
        ("f", "mcp:my-tool"),
    ];
    assert_eq!(actors.len(), expected.len());
    for ((path, kind, id), (want_path, want_id)) in actors.iter().zip(expected) {
        assert_eq!(path, want_path);
        assert_eq!(kind, "ai", "{path}");
        assert_eq!(id, want_id, "{path}");
    }
    // 履歴には session も付く
    assert!(journal(&path)[1]["actor"]["session"].is_string());
}

/// `../x` などの外へのパスは拒否され、外にファイルはできない。vault_info に絶対パスが出ない。
#[test]
fn paths_outside_are_rejected_and_info_has_no_absolute_path() {
    let dir = TestDir::new("serve-outside");
    let path = init_vault(&dir, "v");
    let mut mcp = Mcp::start(&path, &[]);
    for bad in [
        "../x.txt",
        "a/../../x.txt",
        "C:/x.txt",
        "/x.txt",
        r"..\x.txt",
    ] {
        let reply = mcp.send(&modern_call(
            1,
            "vault_write",
            json!({"path": bad, "content": "x"}),
            Some("c"),
        ));
        assert_eq!(tool_error_code(&reply), "INVALID_PATH", "{bad}");
    }
    assert!(!dir.join("x.txt").exists());
    let info = ok(&mcp.send(&modern_call(2, "vault_info", json!({}), Some("c"))));
    assert_eq!(info["name"], "v");
    let text = info.to_string();
    let done = mcp.finish();
    for needle in secrets(&path) {
        assert!(!text.contains(&needle), "{needle:?}");
        assert!(!done.stderr.contains(&needle), "{needle:?}");
    }
}

/// --read-only：Modern・Legacy・initialize の前のどれでも、書くツールは tools/list に出ず、呼べば READ_ONLY か拒否。
/// ファイルも履歴も変わらない。
#[test]
fn read_only_in_every_case() {
    let dir = TestDir::new("serve-readonly");
    let path = init_vault(&dir, "v");
    let before = journal(&path).len();
    let mut mcp = Mcp::start(&path, &["--read-only"]);
    // initialize の前：_meta のない tools/call は -32602 で、受付口に届かない
    let reply = mcp.send(&legacy_call(
        1,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    ));
    assert_eq!(reply["error"]["code"], -32602);
    // Modern
    let list = mcp.send(&request(2, "tools/list", json!({"_meta": meta(Some("c"))})));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "vault_info",
            "vault_list",
            "vault_read",
            "vault_trash_list",
            "vault_changes"
        ]
    );
    for (tool, args) in [
        ("vault_write", json!({"path": "a.txt", "content": "a"})),
        ("vault_mkdir", json!({"path": "d"})),
        ("vault_delete", json!({"path": "x"})),
    ] {
        let reply = mcp.send(&modern_call(3, tool, args, Some("c")));
        assert_eq!(tool_error_code(&reply), "READ_ONLY", "{tool}");
    }
    // Legacy
    mcp.send(&initialize(4, "2025-11-25", Some("c")));
    let list = mcp.send(&request(5, "tools/list", json!({})));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 5);
    let reply = mcp.send(&legacy_call(
        6,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    ));
    assert_eq!(tool_error_code(&reply), "READ_ONLY");
    // 読むツールは使える
    ok(&mcp.send(&legacy_call(7, "vault_list", json!({}))));
    mcp.finish();
    assert!(!path.join("a.txt").exists() && !path.join("d").exists());
    assert_eq!(journal(&path).len(), before, "履歴に何も足さない");
}

/// エラーを起こす呼び出しの返事とログに、保管庫の絶対パス・テストの場所・ユーザー名が出ない。
#[test]
fn errors_never_show_absolute_paths_or_the_user_name() {
    let dir = TestDir::new("serve-no-abs");
    let path = init_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    let junction = junction(&path.join("j"), &outside);
    let mut mcp = Mcp::start(&path, &[]);
    let mut replies = Vec::new();
    let mut call = |tool: &str, args: Value| {
        let reply = mcp.send(&modern_call(1, tool, args, Some("c")));
        replies.push(reply.to_string());
        reply
    };
    ok(&call(
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    ));
    let codes = [
        ("vault_read", json!({"path": "missing.txt"}), "NOT_FOUND"),
        (
            "vault_write",
            json!({"path": "a.txt", "content": "b", "overwrite": false}),
            "ALREADY_EXISTS",
        ),
        (
            "vault_write",
            json!({"path": "a.txt", "content": "b", "expected_version": "1-1"}),
            "CONFLICT",
        ),
        ("vault_mkdir", json!({"path": "a.txt"}), "ALREADY_EXISTS"),
        ("vault_list", json!({"path": "a.txt"}), "NOT_A_DIRECTORY"),
        (
            "vault_move",
            json!({"from": "nothing", "to": "x"}),
            "NOT_FOUND",
        ),
        (
            "vault_read",
            json!({"path": ".vault/vault.json"}),
            "RESERVED_PATH",
        ),
        (
            "vault_read",
            json!({"path": "a.txt", "unknown": 1}),
            "INVALID_ARGUMENT",
        ),
        ("vault_restore", json!({"trash_id": "1-2-3"}), "NOT_FOUND"),
    ];
    for (tool, args, code) in codes {
        assert_eq!(tool_error_code(&call(tool, args)), code, "{tool}");
    }
    if junction {
        let reply = call("vault_read", json!({"path": "j/secret.txt"}));
        assert_eq!(tool_error_code(&reply), "OUTSIDE_VAULT");
    }
    let done = mcp.finish();
    let all = replies.join("\n") + &done.stderr;
    for needle in secrets(&path) {
        assert!(!all.contains(&needle), "{needle:?} が出た");
    }
    assert!(!all.contains("secret"), "外のファイルの中身が出た");
}

/// panic の文（場所・中身）は stderr に出さない。panic のあとは、このプロセスでは書くツールを止める（読むのは続けられる）。
/// テストだけの panic の入口（feature test-panic の debug ビルド。結合テストでは dev-dependencies で有効になる）を使う。
/// release のビルドには入口がないので、`cargo test --release` では動かさない（下の ignored のテストで、飛ばしたことを示す）。
#[cfg(debug_assertions)]
#[test]
fn panics_are_quiet_and_stop_writes() {
    // 読むツールで panic：そのあとの書き込みは止まり、読むのは続けられる
    let dir = TestDir::new("serve-panic");
    let path = init_vault(&dir, "v");
    let mut mcp = Mcp::start_with_env(&path, &[], Some((common::TEST_PANIC_ENV, "vault_info")));
    ok(&mcp.send(&modern_call(
        1,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
        Some("c"),
    )));
    let reply = mcp.send(&modern_call(2, "vault_info", json!({}), Some("c")));
    assert_eq!(reply["error"]["code"], -32603, "{reply}");
    let reply = mcp.send(&modern_call(
        3,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
        Some("c"),
    ));
    assert_eq!(tool_error_code(&reply), "INTERNAL");
    let read = ok(&mcp.send(&modern_call(
        4,
        "vault_read",
        json!({"path": "a.txt"}),
        Some("c"),
    )));
    assert_eq!(read["content"], "a");
    let done = mcp.finish();
    assert_eq!(done.status.code(), Some(0), "サーバーは止まらない");
    assert!(!path.join("b.txt").exists());
    assert_quiet(&done.stderr);
}

/// 書くツール（vault_write）で panic：そのあと書く5つのツールはすべて INTERNAL。vault_changes と読むツールは使える。
#[cfg(debug_assertions)]
#[test]
fn a_panic_in_a_write_tool_stops_every_write_tool() {
    let dir = TestDir::new("serve-panic-write");
    let path = init_vault(&dir, "v");
    let mut mcp = Mcp::start_with_env(&path, &[], Some((common::TEST_PANIC_ENV, "vault_write")));
    ok(&mcp.send(&modern_call(
        1,
        "vault_mkdir",
        json!({"path": "d"}),
        Some("c"),
    )));
    let deleted = ok(&mcp.send(&modern_call(
        2,
        "vault_delete",
        json!({"path": "d"}),
        Some("c"),
    )));
    ok(&mcp.send(&modern_call(
        3,
        "vault_mkdir",
        json!({"path": "e"}),
        Some("c"),
    )));
    let reply = mcp.send(&modern_call(
        4,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
        Some("c"),
    ));
    assert_eq!(reply["error"]["code"], -32603, "{reply}");
    let writes = [
        ("vault_write", json!({"path": "b.txt", "content": "b"})),
        ("vault_mkdir", json!({"path": "f"})),
        ("vault_move", json!({"from": "e", "to": "g"})),
        ("vault_delete", json!({"path": "e"})),
        (
            "vault_restore",
            json!({"trash_id": deleted["trash_id"].clone()}),
        ),
    ];
    for (i, (tool, args)) in writes.into_iter().enumerate() {
        let reply = mcp.send(&modern_call(10 + i as u64, tool, args, Some("c")));
        assert_eq!(tool_error_code(&reply), "INTERNAL", "{tool}");
    }
    // vault_changes（走査も）と、読むツールは続けて使える
    let changes = ok(&mcp.send(&modern_call(20, "vault_changes", json!({}), Some("c"))));
    assert!(changes["changes"].as_array().unwrap().len() >= 4);
    ok(&mcp.send(&modern_call(21, "vault_info", json!({}), Some("c"))));
    ok(&mcp.send(&modern_call(22, "vault_list", json!({}), Some("c"))));
    ok(&mcp.send(&modern_call(23, "vault_trash_list", json!({}), Some("c"))));
    let reply = mcp.send(&modern_call(
        24,
        "vault_read",
        json!({"path": "missing.txt"}),
        Some("c"),
    ));
    assert_eq!(
        tool_error_code(&reply),
        "NOT_FOUND",
        "読むツールは、ふつうに動く"
    );
    let done = mcp.finish();
    assert_eq!(done.status.code(), Some(0));
    // 何も変わっていない（e は残り、f・g・a.txt・b.txt はない。d はゴミ箱のまま）
    assert!(path.join("e").is_dir());
    for name in ["f", "g", "a.txt", "b.txt", "d"] {
        assert!(!path.join(name).exists(), "{name}");
    }
    assert_quiet(&done.stderr);
}

/// release のビルドでは、panic のテスト（2つ）を飛ばす（テスト用の panic の入口は debug ビルドにだけある）。
#[cfg(not(debug_assertions))]
#[test]
#[ignore = "panic のテストは debug ビルドだけ（release には、テスト用の panic の入口がない）"]
fn panic_tests_are_skipped_in_release() {}

/// stderr に、panic の決まった1行だけが出て、panic の文・場所・中身が出ないこと。
#[cfg(debug_assertions)]
fn assert_quiet(stderr: &str) {
    assert!(stderr.contains("内部のエラー（panic"), "{stderr}");
    for secret in [
        "PANIC-SECRET",
        "secret-user",
        "panicked at",
        "テスト用の panic",
        "host.rs",
    ] {
        assert!(!stderr.contains(secret), "{secret} が出た：{stderr}");
    }
}

/// stderr のログは1要求1行で、メソッド名・ツール名・結果・ms だけ。ms は待ち時間を含まない。引数と中身は出さない。
#[test]
fn logs_are_short_and_do_not_include_waiting() {
    let dir = TestDir::new("serve-log");
    let path = init_vault(&dir, "v");
    let mut mcp = Mcp::start(&path, &[]);
    ok(&mcp.send(&modern_call(
        1,
        "vault_write",
        json!({"path": "a.txt", "content": "CONTENT-SHOULD-NOT-BE-LOGGED"}),
        Some("c"),
    )));
    thread::sleep(Duration::from_millis(400));
    ok(&mcp.send(&modern_call(
        2,
        "vault_read",
        json!({"path": "a.txt"}),
        Some("c"),
    )));
    thread::sleep(Duration::from_millis(400));
    mcp.send(&request(3, "ping", json!({})));
    let done = mcp.finish();
    let lines: Vec<&str> = done.stderr.lines().collect();
    assert_eq!(lines.len(), 3, "{}", done.stderr);
    assert!(lines[0].starts_with("mcp: method=tools/call tool=vault_write result=ok ms="));
    assert!(lines[1].starts_with("mcp: method=tools/call tool=vault_read result=ok ms="));
    assert!(lines[2].starts_with("mcp: method=ping tool=- result=ok ms="));
    for line in &lines {
        let ms: u64 = line.rsplit("ms=").next().unwrap().trim().parse().unwrap();
        assert!(ms < 300, "待ち時間が入った：{line}");
    }
    assert!(!done.stderr.contains("CONTENT-SHOULD-NOT-BE-LOGGED"));
    assert!(!done.stderr.contains("a.txt"), "引数（パス）も出さない");
}

/// ジャンクションを作る（作れなければ false）。
fn junction(link: &std::path::Path, target: &std::path::Path) -> bool {
    if !cfg!(windows) {
        skip("junction", "Windows だけ");
        return false;
    }
    let out = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .expect("cmd を起動できない");
    let made = out.status.success()
        && fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink());
    if !made {
        skip("junction", "ジャンクションを作れなかった");
    }
    made
}
