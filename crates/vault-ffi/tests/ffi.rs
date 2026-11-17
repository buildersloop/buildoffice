//! vault-ffi の結合テスト。C ABI の関数を Rust から呼ぶ。

mod common;

use std::fs;
use std::ptr;
use std::thread;

use common::{
    TestDir, call, call_raw, error_code, journal, new_vault, ok, open, open_raw, secrets,
};
use serde_json::{Value, json};
use vault_ffi::{vf_call, vf_close, vf_open};

/// `out_len` が null なら、何もせずに null を返す（保管庫も作らない）。
#[test]
fn a_null_out_len_returns_null_and_does_nothing() {
    let dir = TestDir::new("ffi-null-out");
    let path = dir.join("v");
    let request = json!({"root": path.to_str().unwrap(), "init": true}).to_string();
    // SAFETY: request は request.len() バイトを読める。out_len は null（関数は何もせずに null を返す決まり）。
    let ptr = unsafe { vf_open(request.as_ptr(), request.len(), ptr::null_mut()) };
    assert!(ptr.is_null());
    assert!(!path.exists(), "何もしない");

    let (_path, handle) = new_vault(&dir, "w");
    let request =
        json!({"tool": "vault_write", "args": {"path": "a.txt", "content": "a"}}).to_string();
    // SAFETY: 上と同じ。
    let ptr = unsafe { vf_call(handle, request.as_ptr(), request.len(), ptr::null_mut()) };
    assert!(ptr.is_null());
    assert!(!dir.join("w").join("a.txt").exists(), "何もしない");
}

/// `req_ptr` が null で `req_len > 0` なら INVALID_ARGUMENT。null で長さ 0 は「要求が空」で INVALID_ARGUMENT。
#[test]
fn a_null_request_pointer_is_an_invalid_argument() {
    let dir = TestDir::new("ffi-null-req");
    let (_path, handle) = new_vault(&dir, "v");
    for len in [5usize, 0] {
        let mut out_len = 0usize;
        // SAFETY: req_ptr は null（長さがあれば読まずに断り、0 なら空とする決まり）。out_len は書ける usize。
        let ptr = unsafe { vf_open(ptr::null(), len, &mut out_len) };
        // SAFETY: ptr と out_len は、いま vf_open が返したもの。
        let reply = unsafe { common::take(ptr, out_len) };
        assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{len}");

        let mut out_len = 0usize;
        // SAFETY: 上と同じ。
        let ptr = unsafe { vf_call(handle, ptr::null(), len, &mut out_len) };
        // SAFETY: ptr と out_len は、いま vf_call が返したもの。
        let reply = unsafe { common::take(ptr, out_len) };
        assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{len}");
    }
}

/// 壊れた・形の違う要求は INVALID_ARGUMENT。文には、渡された値（パスなど）を入れない。
#[test]
fn broken_requests_are_invalid_arguments_without_echoing_values() {
    let dir = TestDir::new("ffi-broken");
    let (_path, handle) = new_vault(&dir, "v");
    let secret = r"C:\Profiles\someone-secret\v";
    let open_requests: Vec<Vec<u8>> = vec![
        b"{".to_vec(),
        b"not json".to_vec(),
        b"[]".to_vec(),
        vec![0xff, 0xfe, 0x00],
        json!({"root": secret}).to_string().into_bytes(),
        json!({"root": secret, "init": "yes"})
            .to_string()
            .into_bytes(),
        json!({"root": 5, "init": false}).to_string().into_bytes(),
        json!({"root": secret, "init": false, "extra": secret})
            .to_string()
            .into_bytes(),
    ];
    for request in &open_requests {
        let reply = open_raw(request);
        assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{reply}");
        assert!(!reply.to_string().contains("someone-secret"), "{reply}");
    }
    let call_requests: Vec<Vec<u8>> = vec![
        b"{".to_vec(),
        b"{}".to_vec(),
        json!({"tool": 5}).to_string().into_bytes(),
        json!({"tool": "vault_info", "extra": secret})
            .to_string()
            .into_bytes(),
    ];
    for request in &call_requests {
        let reply = call_raw(handle, request);
        assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{reply}");
        assert!(!reply.to_string().contains("someone-secret"), "{reply}");
    }
    // 32 MiB を超える要求は、読まずに断る
    let big = vec![b' '; vault_ffi::MAX_ALLOC_BYTES + 1];
    assert_eq!(error_code(&open_raw(&big)), "INVALID_ARGUMENT");
    assert_eq!(error_code(&call_raw(handle, &big)), "INVALID_ARGUMENT");
}

/// 要求の中の要素（文字列の外の [・{・,）は 100 万個まで。ちょうど 100 万個なら読み、超えたら読まずに INVALID_ARGUMENT。
/// 文字列の中の記号は数えない（メモリの回帰。mcp-proto と同じ）。
#[test]
fn the_number_of_elements_in_a_request_is_limited() {
    let dir = TestDir::new("ffi-elements");
    let (_path, handle) = new_vault(&dir, "v");
    let head = r#"{"tool":"vault_info","args":{"a":["#;
    let tail = "]}}";
    let outside = |s: &str| s.chars().filter(|c| matches!(c, '[' | '{' | ',')).count();
    let request = |total: usize| {
        let zeros = total - outside(head) - outside(tail) + 1;
        let mut text = String::from(head);
        text.push_str(&"0,".repeat(zeros - 1));
        text.push('0');
        text.push_str(tail);
        text
    };
    // ちょうど 100 万個：読む（vault_info は引数 a を知らないので、vault-core の引数の誤りになる）
    let reply = call_raw(handle, request(1_000_000).as_bytes());
    assert_eq!(error_code(&reply), "INVALID_ARGUMENT");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(message.contains("引数が正しくない"), "{message}");
    // 100 万 1 個：読まずに断る
    let reply = call_raw(handle, request(1_000_001).as_bytes());
    assert_eq!(error_code(&reply), "INVALID_ARGUMENT");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(message.contains("要素が多すぎる"), "{message}");
    // vf_open も同じ
    let over = format!(r#"{{"root":[{}0],"init":false}}"#, "0,".repeat(1_000_000));
    let reply = open_raw(over.as_bytes());
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("要素が多すぎる")
    );
    // 文字列の中の記号は数えない
    let commas = ",[{".repeat(1_000_000);
    let reply = call(handle, "vault_info", json!({"s": commas}));
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(message.contains("引数が正しくない"), "{message}");
}

/// 知らないハンドル・閉じたハンドルは INVALID_ARGUMENT。vf_close は何度呼んでもよい。
#[test]
fn unknown_and_closed_handles() {
    let dir = TestDir::new("ffi-handles");
    for unknown in [0u64, u64::MAX, 987_654_321] {
        let reply = call(unknown, "vault_info", json!({}));
        assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{unknown}");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("ハンドル")
        );
    }
    let (_path, handle) = new_vault(&dir, "v");
    ok(&call(handle, "vault_info", json!({})));
    vf_close(handle);
    assert_eq!(
        error_code(&call(handle, "vault_info", json!({}))),
        "INVALID_ARGUMENT"
    );
    vf_close(handle);
    vf_close(u64::MAX);
}

/// tool_specs() にない名前は UNKNOWN_TOOL。
#[test]
fn unknown_tools_are_rejected() {
    let dir = TestDir::new("ffi-unknown-tool");
    let (_path, handle) = new_vault(&dir, "v");
    for name in ["vault_format", "", "VAULT_INFO", "__vault_ffi_test_panicx"] {
        let reply = call(handle, name, json!({}));
        assert_eq!(error_code(&reply), "UNKNOWN_TOOL", "{name}");
    }
}

/// init:true は作ってから開く（履歴の init の actor は {user, app}）。init:false は開くだけ。
#[test]
fn init_and_open() {
    let dir = TestDir::new("ffi-init-open");
    let (path, handle) = new_vault(&dir, "v");
    let entries = journal(&path);
    assert_eq!(entries[0]["op"], "init");
    assert_eq!(entries[0]["actor"]["kind"], "user");
    assert_eq!(entries[0]["actor"]["id"], "app");
    // 同じ保管庫を、もう一度開く（init:false）。どちらのハンドルも使える
    let again = ok(&open(
        json!({"root": path.to_str().unwrap(), "init": false}),
    ));
    let second = again["handle"].as_u64().unwrap();
    assert_ne!(second, handle);
    ok(&call(
        handle,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    ));
    let read = ok(&call(second, "vault_read", json!({"path": "a.txt"})));
    assert_eq!(read["content"], "a");
    // すでに保管庫に init:true → INVALID_ROOT（すでに保管庫）
    let reply = open(json!({"root": path.to_str().unwrap(), "init": true}));
    assert_eq!(error_code(&reply), "INVALID_ROOT");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("すでに保管庫")
    );
    // 保管庫でないフォルダに init:false → NOT_A_VAULT（作らない）
    let plain = dir.join("plain");
    fs::create_dir(&plain).unwrap();
    let reply = open(json!({"root": plain.to_str().unwrap(), "init": false}));
    assert_eq!(error_code(&reply), "NOT_A_VAULT");
    assert!(!plain.join(".vault").exists());
    // 相対パス・UNC・\\?\ の形は INVALID_ROOT（ドライブ文字から始まる形だけ）
    for root in [
        "relative/v".to_owned(),
        r"\\vault-test-no-such-server\share\v".to_owned(),
        format!(r"\\?\{}", dir.join("x").display()),
    ] {
        let reply = open(json!({"root": root, "init": true}));
        assert_eq!(error_code(&reply), "INVALID_ROOT", "{root}");
    }
    // 読み取り専用で開くと、書くツールは READ_ONLY
    let read_only = ok(&open(
        json!({"root": path.to_str().unwrap(), "init": false, "read_only": true}),
    ))["handle"]
        .as_u64()
        .unwrap();
    let reply = call(
        read_only,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    );
    assert_eq!(error_code(&reply), "READ_ONLY");
    ok(&call(read_only, "vault_list", json!({})));
    assert!(!path.join("b.txt").exists());
    for h in [handle, second, read_only] {
        vf_close(h);
    }
}

/// tool_names.json の10個すべてを vf_call で呼べる（網羅率）。履歴の actor は {user, app}。
#[test]
fn every_tool_round_trips_and_actors_are_the_app() {
    let names: Vec<String> =
        serde_json::from_str(include_str!("../../vault-core/tool_names.json")).unwrap();
    assert_eq!(names.len(), 10);
    let dir = TestDir::new("ffi-all-tools");
    let (path, handle) = new_vault(&dir, "v");
    let mut called = Vec::new();
    let mut run = |tool: &str, args: Value| {
        called.push(tool.to_owned());
        ok(&call(handle, tool, args))
    };
    let info = run("vault_info", json!({}));
    assert_eq!(info["name"], "v");
    run(
        "vault_write",
        json!({"path": "notes/a.txt", "content": "hello"}),
    );
    assert_eq!(
        run("vault_read", json!({"path": "notes/a.txt"}))["content"],
        "hello"
    );
    run("vault_mkdir", json!({"path": "archive"}));
    run(
        "vault_move",
        json!({"from": "notes/a.txt", "to": "archive/a.txt"}),
    );
    let listed = run("vault_list", json!({"recursive": true}));
    assert!(listed["entries"].as_array().unwrap().len() >= 3);
    let deleted = run("vault_delete", json!({"path": "archive/a.txt"}));
    let trash = run("vault_trash_list", json!({}));
    assert_eq!(trash["items"][0]["trash_id"], deleted["trash_id"]);
    run(
        "vault_restore",
        json!({"trash_id": deleted["trash_id"].clone()}),
    );
    let changes = run("vault_changes", json!({"since_seq": 0}));
    assert!(changes["changes"].as_array().unwrap().len() >= 6);

    let mut sorted_called = called.clone();
    sorted_called.sort();
    let mut sorted_names = names.clone();
    sorted_names.sort();
    assert_eq!(sorted_called, sorted_names, "10個すべてを呼んだ");

    for entry in journal(&path) {
        assert_eq!(entry["actor"]["kind"], "user", "{entry}");
        assert_eq!(entry["actor"]["id"], "app", "{entry}");
        assert!(entry["actor"]["session"].is_string());
    }
    vf_close(handle);
}

/// 返り値とエラーの文に、保管庫の絶対パス・テストの場所・ユーザー名が出ない。
#[test]
fn outputs_never_show_absolute_paths_or_the_user_name() {
    let dir = TestDir::new("ffi-no-abs");
    let (path, handle) = new_vault(&dir, "v");
    let mut replies = vec![
        call(handle, "vault_info", json!({})),
        call(handle, "vault_read", json!({"path": "missing.txt"})),
        call(
            handle,
            "vault_write",
            json!({"path": "../x.txt", "content": "x"}),
        ),
        call(handle, "vault_list", json!({"path": "nothing"})),
        call(handle, "vault_restore", json!({"trash_id": "1-2-3"})),
        call(handle, "vault_read", json!({"path": "a", "unknown": 1})),
        call(handle, "vault_changes", json!({})),
    ];
    let plain = dir.join("plain");
    fs::create_dir(&plain).unwrap();
    replies.push(open(
        json!({"root": plain.to_str().unwrap(), "init": false}),
    ));
    replies.push(open(json!({"root": path.to_str().unwrap(), "init": true})));
    replies.push(open(
        json!({"root": dir.join("missing").to_str().unwrap(), "init": false}),
    ));
    let all: String = replies
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    for needle in secrets(&path) {
        assert!(!all.contains(&needle), "{needle:?} が出た");
    }
    vf_close(handle);
}

/// panic は INTERNAL で、プロセスは止まらない。そのハンドルでは書く5つのツールを止め、読むツールと vault_changes は使える。
/// ほかのハンドルは書ける（テスト用の入口は、feature test-panic の debug ビルドだけ）。
#[cfg(debug_assertions)]
#[test]
fn a_panic_stops_writes_on_that_handle() {
    let dir = TestDir::new("ffi-panic");
    let (path, handle) = new_vault(&dir, "v");
    ok(&call(
        handle,
        "vault_write",
        json!({"path": "keep.txt", "content": "keep"}),
    ));
    ok(&call(handle, "vault_mkdir", json!({"path": "e"})));
    let deleted = ok(&call(handle, "vault_delete", json!({"path": "e"})));
    ok(&call(handle, "vault_mkdir", json!({"path": "e"})));
    let reply = call(handle, vault_ffi::TEST_PANIC_TOOL, json!({}));
    assert_eq!(error_code(&reply), "INTERNAL");
    assert!(
        !reply.to_string().contains("PANIC-SECRET"),
        "panic の文を返さない"
    );
    let writes = [
        ("vault_write", json!({"path": "b.txt", "content": "b"})),
        ("vault_mkdir", json!({"path": "f"})),
        ("vault_move", json!({"from": "e", "to": "g"})),
        ("vault_delete", json!({"path": "e"})),
        (
            "vault_restore",
            json!({"trash_id": deleted["trash_id"].clone(), "to": "r"}),
        ),
    ];
    for (tool, args) in writes {
        assert_eq!(error_code(&call(handle, tool, args)), "INTERNAL", "{tool}");
    }
    for tool in [
        "vault_info",
        "vault_list",
        "vault_trash_list",
        "vault_changes",
    ] {
        ok(&call(handle, tool, json!({})));
    }
    let read = ok(&call(handle, "vault_read", json!({"path": "keep.txt"})));
    assert_eq!(read["content"], "keep", "panic のあとも読める");
    for name in ["b.txt", "f", "g", "r"] {
        assert!(!path.join(name).exists(), "{name}");
    }
    // 開き直したハンドルは書ける
    let fresh = ok(&open(
        json!({"root": path.to_str().unwrap(), "init": false}),
    ))["handle"]
        .as_u64()
        .unwrap();
    ok(&call(
        fresh,
        "vault_write",
        json!({"path": "c.txt", "content": "c"}),
    ));
    vf_close(handle);
    vf_close(fresh);
}

/// release のビルドでは、panic のテストを飛ばす（テスト用の入口は debug ビルドにだけある）。
#[cfg(not(debug_assertions))]
#[test]
#[ignore = "panic のテストは debug ビルドだけ（release には、テスト用の panic の入口がない）"]
fn panic_test_is_skipped_in_release() {}

/// いくつものスレッドから、同じハンドルで同時に呼べる（表は Mutex、保管庫は Arc）。
#[test]
fn many_threads_can_use_one_handle() {
    let dir = TestDir::new("ffi-threads");
    let (path, handle) = new_vault(&dir, "v");
    thread::scope(|s| {
        for t in 0..4 {
            s.spawn(move || {
                for i in 0..10 {
                    let reply = call(
                        handle,
                        "vault_write",
                        json!({"path": format!("t{t}/f{i}.txt"), "content": "x"}),
                    );
                    ok(&reply);
                }
            });
        }
    });
    let writes = journal(&path)
        .into_iter()
        .filter(|e| e["op"] == "write")
        .count();
    assert_eq!(writes, 40);
    let mut seqs: Vec<u64> = journal(&path)
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    let before = seqs.len();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), before, "seq は重ならない");
    vf_close(handle);
}
