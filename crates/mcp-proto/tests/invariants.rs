//! 不変条件のテスト。どれも、Modern・Legacy・`initialize` の前の3つの場合で確かめる。
//!
//! - Modern：`_meta` 付きの要求
//! - Legacy：`initialize` のあとの `_meta` のない要求（名前は `initialize` の clientInfo から）
//! - `initialize` の前：`_meta` 付きの要求（Modern として処理される）と、`_meta` のない要求（-32602 で拒否される）

mod common;

use std::time::Duration;

use common::{
    Fixture, deep_call, error_code, fixture, fixture_with, initialize, modern, notification,
    request,
};
use mcp_proto::codes;
use serde_json::{Value, json};

/// 3つの場合の tools/call（`name` を呼ぶ）の行。Legacy は先に initialize を送る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Modern,
    Legacy,
    BeforeInitMeta,
    BeforeInitNoMeta,
}

const CASES: [Case; 4] = [
    Case::Modern,
    Case::Legacy,
    Case::BeforeInitMeta,
    Case::BeforeInitNoMeta,
];

/// その場合のサーバーを用意する（Legacy なら initialize を済ませる。Modern は、initialize のあとの _meta 付きにする）。
fn prepare(f: &mut Fixture, case: Case, client_name: Option<&str>) {
    match case {
        Case::Legacy => {
            f.one(initialize(0, "2025-11-25", client_name));
        }
        Case::Modern => {
            f.one(initialize(0, "2025-06-18", Some("legacy-name")));
        }
        Case::BeforeInitMeta | Case::BeforeInitNoMeta => {}
    }
}

/// その場合の要求の行。
fn line(case: Case, id: u64, method: &str, params: Value, client_name: Option<&str>) -> String {
    match case {
        Case::Modern | Case::BeforeInitMeta => modern(id, method, params, client_name),
        Case::Legacy | Case::BeforeInitNoMeta => request(id, method, params),
    }
}

// ---------------- 1. 受付口・読み取り専用・回数の制限は1つ

/// tools/call は、どの場合でも同じ1つの受付口を通る。読み取り専用の判定（受付口の中）も同じ。
/// initialize の前の _meta のない tools/call は、受付口に届かない。
#[test]
fn invariant1_every_tools_call_goes_through_the_one_host() {
    for case in CASES {
        let mut f = fixture_with(true, None);
        prepare(&mut f, case, Some("c"));
        let reply = f.one(line(
            case,
            1,
            "tools/call",
            json!({"name": "fake_write"}),
            Some("c"),
        ));
        if case == Case::BeforeInitNoMeta {
            assert_eq!(error_code(&reply), Some(-32602), "{case:?}");
            assert!(f.calls().is_empty(), "{case:?}: 受付口に届かない");
            continue;
        }
        let result = &reply["result"];
        assert_eq!(result["isError"], true, "{case:?}");
        assert_eq!(
            result["content"][0]["text"], "READ_ONLY: 読み取り専用で開いた保管庫には書けない",
            "{case:?}"
        );
        let calls = f.calls();
        assert_eq!(calls.len(), 1, "{case:?}");
        assert_eq!(calls[0].name, "fake_write");
        // 読み取り専用なら、tools/list に書くツールを出さない（受付口の一覧のまま）
        let list = f.one(line(case, 2, "tools/list", json!({}), Some("c")));
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["fake_read"], "{case:?}");
    }
}

/// 3つの場合の要求を混ぜても、回数の制限は1つで共有される。
#[test]
fn invariant1_the_rate_limit_is_shared_by_every_case() {
    let mut f = fixture();
    let burst = 100; // 1秒に50回・ためられるのは100回まで
    let mut lines = Vec::new();
    // initialize の前（_meta のない ping、_meta 付きの ping）
    for i in 0..30 {
        lines.push(request(i, "ping", json!({})));
        lines.push(modern(100 + i, "ping", json!({}), Some("c")));
    }
    // initialize（1回と数える）と、Legacy
    lines.push(initialize(1000, "2025-11-25", Some("c")));
    while lines.len() < burst {
        let id = 2000 + lines.len() as u64;
        lines.push(request(id, "tools/list", json!({})));
    }
    let run = f.send(&lines);
    assert!(run.replies.iter().all(|r| r.get("error").is_none()));
    // ここで 100 回。次は、どの場合でも -31000
    let run = f.send(&[
        modern(1, "ping", json!({}), Some("c")),
        request(2, "ping", json!({})),
        request(3, "tools/call", json!({"name": "fake_read"})),
        modern(4, "tools/call", json!({"name": "fake_read"}), Some("c")),
    ]);
    for reply in &run.replies {
        assert_eq!(error_code(reply), Some(codes::RATE_LIMITED), "{reply}");
    }
    assert!(f.calls().is_empty(), "超えた tools/call は実行しない");
}

// ---------------- 2. actor の kind は通り道で決まる

/// クライアントが _meta や clientInfo や引数に、別の kind らしい値を入れても、受付口に渡るのは `mcp:<名前>` の id だけ
/// （kind は渡さない。受付口は MCP の通り道なので常に ai にする）。
#[test]
fn invariant2_the_client_cannot_choose_the_actor_kind() {
    for case in [Case::Modern, Case::Legacy, Case::BeforeInitMeta] {
        let mut f = fixture();
        if case == Case::Legacy {
            f.one(request(
                0,
                "initialize",
                json!({
                    "protocolVersion": "2025-11-25",
                    "clientInfo": {"name": "x", "kind": "user", "actor": {"kind": "user"}},
                }),
            ));
        } else {
            prepare(&mut f, case, Some("x"));
        }
        let mut params = json!({
            "name": "fake_read",
            "arguments": {"actor": {"kind": "user", "id": "app"}},
        });
        if case != Case::Legacy {
            let mut meta = common::meta(None);
            meta["io.modelcontextprotocol/clientInfo"] =
                json!({"name": "x", "kind": "user", "actor": {"kind": "user"}});
            meta["io.modelcontextprotocol/actor"] = json!({"kind": "user", "id": "app"});
            meta["kind"] = json!("user");
            params["_meta"] = meta;
        }
        let reply = f.one(request(1, "tools/call", params));
        assert_eq!(reply["result"]["isError"], false, "{case:?}");
        let calls = f.calls();
        assert_eq!(calls.len(), 1, "{case:?}");
        assert_eq!(calls[0].actor_id, "mcp:x", "{case:?}");
    }
}

// ---------------- 3. 名前

/// 名前が、印字できない文字を除いて空になったとき、または clientInfo がないときは `mcp:unknown`。
/// Legacy は initialize の clientInfo から、Modern と initialize の前の _meta 付きの要求は、要求ごとの _meta から取る。
#[test]
fn invariant3_actor_names() {
    let names: [(Option<&str>, &str); 5] = [
        (Some("claude-code"), "mcp:claude-code"),
        (Some("\u{1}\u{7f}\n"), "mcp:unknown"),
        (Some("日本語"), "mcp:unknown"),
        (Some("  spaced  "), "mcp:spaced"),
        (None, "mcp:unknown"),
    ];
    for case in [Case::Modern, Case::Legacy, Case::BeforeInitMeta] {
        for (name, expected) in names {
            let mut f = fixture();
            prepare(&mut f, case, name);
            f.one(line(
                case,
                1,
                "tools/call",
                json!({"name": "fake_read"}),
                name,
            ));
            assert_eq!(f.calls()[0].actor_id, expected, "{case:?} {name:?}");
        }
        // 長い名前は 64 字で切る
        let long = "n".repeat(100);
        let mut f = fixture();
        prepare(&mut f, case, Some(&long));
        f.one(line(
            case,
            1,
            "tools/call",
            json!({"name": "fake_read"}),
            Some(&long),
        ));
        assert_eq!(f.calls()[0].actor_id, format!("mcp:{}", "n".repeat(64)));
        // 上書き（--actor-id）は、どの場合でも clientInfo より優先し、同じく整える
        for (over, expected) in [("cli-tool", "mcp:cli-tool"), ("\u{7}", "mcp:unknown")] {
            let mut f = fixture_with(false, Some(over));
            prepare(&mut f, case, Some("client"));
            f.one(line(
                case,
                1,
                "tools/call",
                json!({"name": "fake_read"}),
                Some("client"),
            ));
            assert_eq!(f.calls()[0].actor_id, expected, "{case:?} {over:?}");
        }
    }
}

// ---------------- 4. 回数の制限

/// 回数を使い切る（止まった時計で、残りの回数の ping。_meta 付きなので、どの場合でも受け付けられる）。
/// `used` は、もう使った回数（initialize も1回と数える）。
fn exhaust(f: &mut Fixture, used: u64) {
    let lines: Vec<String> = (used..100)
        .map(|i| modern(10_000 + i, "ping", json!({}), Some("c")))
        .collect();
    let run = f.send(&lines);
    assert!(run.replies.iter().all(|r| r.get("error").is_none()));
}

/// tools/call・discover・list・ping のどれでも、超えたら -31000。番号は -32020〜-32099（MCP の予約）に入らない。
#[test]
fn invariant4_every_request_is_rate_limited_with_minus_31000() {
    assert!(!(-32099..=-32020).contains(&codes::RATE_LIMITED));
    assert!(!(-32768..=-32000).contains(&codes::RATE_LIMITED));
    for case in CASES {
        let mut f = fixture();
        prepare(&mut f, case, Some("c"));
        let used = u64::from(matches!(case, Case::Modern | Case::Legacy));
        exhaust(&mut f, used);
        let mut methods = vec![
            ("tools/call", json!({"name": "fake_read"})),
            ("tools/list", json!({})),
            ("ping", json!({})),
        ];
        if matches!(case, Case::Modern | Case::BeforeInitMeta) {
            methods.push(("server/discover", json!({})));
        }
        if case == Case::BeforeInitNoMeta {
            methods.push(("initialize", json!({"protocolVersion": "2025-11-25"})));
        }
        for (i, (method, params)) in methods.into_iter().enumerate() {
            let reply = f.one(line(case, i as u64, method, params, Some("c")));
            assert_eq!(error_code(&reply), Some(-31000), "{case:?} {method}");
            assert_eq!(reply["error"]["message"], "Rate limited");
        }
        assert!(f.calls().is_empty(), "{case:?}");
    }
}

/// 回数の確認は、版の確認より前（超えたあとは、版の誤った要求にも、_meta のない要求にも -31000）。
#[test]
fn invariant4_the_rate_limit_comes_before_the_version_check() {
    let mut f = fixture();
    exhaust(&mut f, 0);
    let mut bad_version = common::meta(Some("c"));
    bad_version["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    let mut no_capabilities = common::meta(Some("c"));
    no_capabilities
        .as_object_mut()
        .unwrap()
        .remove("io.modelcontextprotocol/clientCapabilities");
    let run = f.send(&[
        request(1, "tools/list", json!({"_meta": bad_version})),
        request(2, "tools/list", json!({"_meta": no_capabilities})),
        request(3, "tools/list", json!({})),
        request(4, "unknown/method", json!({})),
    ]);
    for reply in &run.replies {
        assert_eq!(error_code(reply), Some(-31000), "{reply}");
    }
}

/// -32700・-32600 で返す要求と、通知は数えない。時間がたてば、1秒に 50 回ぶん戻る。
#[test]
fn invariant4_what_is_not_counted_and_refilling() {
    let mut f = fixture();
    let mut junk = Vec::new();
    for i in 0..150 {
        junk.push("{not json".to_owned());
        junk.push(format!("[{}]", request(i, "ping", json!({}))));
        junk.push(json!({"jsonrpc": "2.0", "id": null, "method": "ping"}).to_string());
        junk.push(json!({"jsonrpc": "1.0", "id": i, "method": "ping"}).to_string());
        junk.push(notification("notifications/initialized", json!({})));
        junk.push(notification("tools/call", json!({"name": "fake_read"})));
    }
    let run = f.send(&junk);
    assert!(
        run.replies
            .iter()
            .all(|r| matches!(error_code(r), Some(-32700 | -32600))),
        "数えない要求に -31000 が出ない"
    );
    let mut big = vec![b'x'; 32 * 1024 * 1024 + 1];
    big.push(b'\n');
    let run = f.send_raw(big);
    assert_eq!(error_code(&run.replies[0]), Some(-32600));
    // まだ 100 回使える
    exhaust(&mut f, 0);
    assert_eq!(
        error_code(&f.one(modern(1, "ping", json!({}), Some("c")))),
        Some(-31000)
    );
    // 1秒で 50 回ぶん戻る
    f.clock.advance(Duration::from_secs(1));
    let lines: Vec<String> = (0..50)
        .map(|i| modern(i, "ping", json!({}), Some("c")))
        .collect();
    let run = f.send(&lines);
    assert!(run.replies.iter().all(|r| r.get("error").is_none()));
    assert_eq!(
        error_code(&f.one(modern(1, "ping", json!({}), Some("c")))),
        Some(-31000)
    );
}

// ---------------- 5. 深さの上限

/// 入れ子が 128 を超える要求は -32700 で、受付口に届かない（どの場合でも）。
#[test]
fn invariant5_the_nesting_limit_stays() {
    for case in CASES {
        let mut f = fixture();
        prepare(&mut f, case, Some("c"));
        let meta_name = matches!(case, Case::Modern | Case::BeforeInitMeta).then_some("c");
        for depth in [128, 129, 200, 10_000] {
            let reply = f.one(deep_call(1, depth, meta_name));
            assert_eq!(error_code(&reply), Some(-32700), "{case:?} {depth}");
            assert_eq!(reply["id"], Value::Null);
        }
        assert!(f.calls().is_empty(), "{case:?}");
    }
}
