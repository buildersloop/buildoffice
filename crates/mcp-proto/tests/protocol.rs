//! mcp-proto のテスト。偽の受付口と、メモリ上の入出力で確かめる。

mod common;

use std::time::Duration;

use common::{
    Fixture, SECRET_CONTENT, SlowInput, deep_call, error_code, fixture, initialize, log_ms, modern,
    notification, request,
};
use serde_json::{Value, json};

/// Legacy の4つの版（実装の定数ではなく、文字どおりに書く）。
const LEGACY: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MODERN_VERSION: &str = "2026-07-28";
const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

fn server_info_meta(reply: &Value) -> &Value {
    &reply["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]
}

// ---------------- Modern

#[test]
fn discover_offers_only_the_modern_version() {
    let mut f = fixture();
    let reply = f.one(modern(1, "server/discover", json!({}), Some("c")));
    let result = &reply["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["supportedVersions"], json!([MODERN_VERSION]));
    assert_eq!(result["capabilities"], json!({"tools": {}}));
    assert!(result["instructions"].as_str().unwrap().contains("ゴミ箱"));
    // キャッシュのヒント（2026-07-28 の caching の MUST）：ttlMs は 0、cacheScope は private
    assert_eq!(result["ttlMs"], 0);
    assert_eq!(result["cacheScope"], "private");
    assert_eq!(
        server_info_meta(&reply),
        &json!({"name": "vault-mcp", "version": "0.1.0"})
    );
    assert_eq!(reply["id"], 1);
}

#[test]
fn modern_results_carry_result_type_and_server_info() {
    let mut f = fixture();
    let list = f.one(modern(1, "tools/list", json!({}), Some("c")));
    assert_eq!(list["result"]["resultType"], "complete");
    assert!(server_info_meta(&list).is_object());
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "fake_read");
    assert_eq!(
        tools[0]["annotations"],
        json!({"readOnlyHint": true, "destructiveHint": false})
    );
    assert_eq!(tools[1]["annotations"]["destructiveHint"], true);
    assert!(list["result"].get("nextCursor").is_none());
    assert_eq!(list["result"]["ttlMs"], 0, "キャッシュのヒント");
    assert_eq!(list["result"]["cacheScope"], "private");

    let ok = f.one(modern(
        2,
        "tools/call",
        json!({"name": "fake_read"}),
        Some("c"),
    ));
    let result = &ok["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["content"], SECRET_CONTENT);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap(),
        result["structuredContent"]
    );
    assert!(server_info_meta(&ok).is_object());

    let failed = f.one(modern(
        3,
        "tools/call",
        json!({"name": "fake_fail"}),
        Some("c"),
    ));
    let result = &failed["result"];
    assert_eq!(result["resultType"], "complete", "isError でも complete");
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["text"], "NOT_FOUND: ない：a.txt");
    assert_eq!(
        result["structuredContent"],
        json!({"error": {"code": "NOT_FOUND", "message": "ない：a.txt"}})
    );
}

/// キャッシュのヒント（ttlMs・cacheScope）は、Modern の server/discover と tools/list にだけ付ける
/// （2026-07-28 の caching の MUST）。Legacy の返事と、tools/call・ping の結果には付けない。
#[test]
fn cache_hints_only_on_modern_discover_and_list() {
    let has_hints = |reply: &Value| {
        let result = &reply["result"];
        match (result.get("ttlMs"), result.get("cacheScope")) {
            (Some(ttl), Some(scope)) => {
                assert_eq!(ttl, 0, "{reply}");
                assert_eq!(scope, "private", "{reply}");
                true
            }
            (None, None) => false,
            _ => panic!("片方だけ：{reply}"),
        }
    };
    let mut f = fixture();
    let run = f.send(&[
        modern(1, "server/discover", json!({}), Some("c")),
        modern(2, "tools/list", json!({}), Some("c")),
        modern(3, "tools/call", json!({"name": "fake_read"}), Some("c")),
        modern(4, "tools/call", json!({"name": "fake_fail"}), Some("c")),
        modern(5, "ping", json!({}), Some("c")),
        initialize(6, "2025-11-25", Some("c")),
        request(7, "tools/list", json!({})),
        request(8, "tools/call", json!({"name": "fake_read"})),
        request(9, "ping", json!({})),
    ]);
    let hints: Vec<bool> = run.replies.iter().map(has_hints).collect();
    assert_eq!(
        hints,
        [true, true, false, false, false, false, false, false, false]
    );
}

#[test]
fn unsupported_versions_are_minus_32022() {
    let mut f = fixture();
    for version in ["1900-01-01", "2025-11-25", "2024-11-05"] {
        let mut meta = common::meta(Some("c"));
        meta["io.modelcontextprotocol/protocolVersion"] = json!(version);
        let reply = f.one(request(1, "tools/list", json!({"_meta": meta})));
        assert_eq!(error_code(&reply), Some(-32022), "{version}");
        assert_eq!(reply["error"]["message"], "Unsupported protocol version");
        assert_eq!(
            reply["error"]["data"],
            json!({"supported": ["2026-07-28"], "requested": version})
        );
    }
    assert!(f.calls().is_empty());
}

#[test]
fn missing_required_meta_is_minus_32602_but_client_info_is_optional() {
    let mut f = fixture();
    // clientCapabilities がない
    let reply = f.one(request(
        1,
        "tools/list",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
    ));
    assert_eq!(error_code(&reply), Some(-32602));
    // clientCapabilities がオブジェクトでない
    let mut meta = common::meta(None);
    meta["io.modelcontextprotocol/clientCapabilities"] = json!("all");
    let reply = f.one(request(2, "tools/list", json!({"_meta": meta})));
    assert_eq!(error_code(&reply), Some(-32602));
    // protocolVersion が文字列でない
    let mut meta = common::meta(None);
    meta["io.modelcontextprotocol/protocolVersion"] = json!(20260728);
    let reply = f.one(request(3, "tools/list", json!({"_meta": meta})));
    assert_eq!(error_code(&reply), Some(-32602));
    // clientInfo がなくても通る（actor は mcp:unknown）
    let reply = f.one(modern(4, "tools/call", json!({"name": "fake_read"}), None));
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(f.calls()[0].actor_id, "mcp:unknown");
}

#[test]
fn modern_ping() {
    let mut f = fixture();
    let reply = f.one(modern("p", "ping", json!({}), Some("c")));
    assert_eq!(reply["result"]["resultType"], "complete");
    assert!(server_info_meta(&reply).is_object());
    assert_eq!(reply["id"], "p");
}

// ---------------- Legacy

#[test]
fn initialize_negotiates_the_four_legacy_versions() {
    for version in LEGACY {
        let mut f = fixture();
        let reply = f.one(initialize(1, version, Some("c")));
        let result = &reply["result"];
        let keys: Vec<&String> = result.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["capabilities", "protocolVersion", "serverInfo"],
            "instructions は付けない"
        );
        assert_eq!(result["protocolVersion"], version);
        assert_eq!(
            result["capabilities"],
            json!({"tools": {"listChanged": false}})
        );
        assert_eq!(
            result["serverInfo"],
            json!({"name": "vault-mcp", "version": "0.1.0"})
        );
        assert!(result.get("resultType").is_none());
    }
    for other in [
        json!("1999-01-01"),
        json!("2026-07-28"),
        json!(5),
        Value::Null,
    ] {
        let mut f = fixture();
        let reply = f.one(request(1, "initialize", json!({"protocolVersion": other})));
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25", "{other}");
    }
}

#[test]
fn legacy_results_follow_the_negotiated_version() {
    for version in LEGACY {
        let mut f = fixture();
        let run = f.send(&[
            initialize(1, version, Some("c")),
            notification("notifications/initialized", json!({})),
            request(2, "tools/list", json!({})),
            request(3, "tools/call", json!({"name": "fake_read"})),
            request(4, "tools/call", json!({"name": "fake_fail"})),
            request(5, "ping", json!({})),
        ]);
        assert_eq!(run.replies.len(), 5, "通知には返事をしない");
        let list = &run.replies[1]["result"];
        let ok = &run.replies[2]["result"];
        let failed = &run.replies[3]["result"];
        let structured = version >= "2025-06-18";
        let annotations = version >= "2025-03-26";
        assert_eq!(
            list["tools"][0].get("annotations").is_some(),
            annotations,
            "{version}"
        );
        assert_eq!(
            ok.get("structuredContent").is_some(),
            structured,
            "{version}"
        );
        assert_eq!(
            failed.get("structuredContent").is_some(),
            structured,
            "{version}"
        );
        assert_eq!(failed["isError"], true);
        assert_eq!(failed["content"][0]["text"], "NOT_FOUND: ない：a.txt");
        assert_eq!(ok["isError"], false);
        assert!(ok["content"][0]["text"].as_str().unwrap().contains("size"));
        for reply in &run.replies {
            let result = &reply["result"];
            assert!(result.get("resultType").is_none(), "{version}: {reply}");
            assert!(result.get("_meta").is_none(), "{version}: {reply}");
        }
        assert_eq!(
            run.replies[4]["result"],
            json!({}),
            "Legacy の ping は {{}}"
        );
    }
}

/// initialize は1回だけ（2回目は -32600。取り決めた版は変えない）。Legacy では server/discover を知らない。
#[test]
fn a_second_initialize_and_discover_without_meta() {
    let mut f = fixture();
    let run = f.send(&[
        initialize(1, "2024-11-05", Some("c")),
        initialize(2, "2025-11-25", Some("d")),
        request(3, "server/discover", json!({})),
        request(4, "tools/list", json!({})),
    ]);
    assert_eq!(error_code(&run.replies[1]), Some(-32600));
    assert_eq!(error_code(&run.replies[2]), Some(-32601));
    assert!(
        run.replies[3]["result"]["tools"][0]
            .get("annotations")
            .is_none(),
        "2024-11-05 のまま"
    );
}

// ---------------- initialize の前

#[test]
fn requests_without_meta_before_initialize_are_rejected() {
    let mut f = fixture();
    let run = f.send(&[
        request(1, "tools/list", json!({})),
        request(2, "tools/call", json!({"name": "fake_read"})),
        request(3, "server/discover", json!({})),
        request(4, "resources/list", json!({})),
        request(5, "ping", json!({})),
        request(6, "ping", Value::Null),
    ]);
    for reply in &run.replies[..4] {
        assert_eq!(error_code(reply), Some(-32602), "{reply}");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("initialize")
        );
    }
    assert_eq!(run.replies[4]["result"], json!({}));
    assert_eq!(run.replies[5]["result"], json!({}));
    assert!(f.calls().is_empty(), "受付口に届かない");
}

/// initialize のあとでも、_meta 付きの要求は Modern、_meta のない要求は Legacy（Q4）。Modern は前の要求から引き継がない。
#[test]
fn modern_and_legacy_side_by_side() {
    let mut f = fixture();
    let run = f.send(&[
        initialize(1, "2025-03-26", Some("legacy-client")),
        modern(
            2,
            "tools/call",
            json!({"name": "fake_read"}),
            Some("modern-client"),
        ),
        request(3, "tools/call", json!({"name": "fake_read"})),
        modern(4, "tools/call", json!({"name": "fake_read"}), None),
        modern(5, "ping", json!({}), None),
        request(6, "ping", json!({})),
    ]);
    let modern_result = &run.replies[1]["result"];
    assert_eq!(modern_result["resultType"], "complete");
    assert!(modern_result.get("structuredContent").is_some());
    let legacy_result = &run.replies[2]["result"];
    assert!(legacy_result.get("resultType").is_none());
    assert!(
        legacy_result.get("structuredContent").is_none(),
        "2025-03-26 には structuredContent がない"
    );
    assert_eq!(run.replies[4]["result"]["resultType"], "complete");
    assert_eq!(run.replies[5]["result"], json!({}));
    let actors: Vec<String> = f.calls().into_iter().map(|c| c.actor_id).collect();
    assert_eq!(
        actors,
        ["mcp:modern-client", "mcp:legacy-client", "mcp:unknown"],
        "Modern は要求ごとの _meta だけを使う"
    );
}

// ---------------- 共通

#[test]
fn notifications_get_no_reply_and_never_reach_the_host() {
    let mut f = fixture();
    let run = f.send(&[
        notification("notifications/initialized", json!({})),
        notification("notifications/cancelled", json!({"requestId": 1})),
        notification("notifications/foo", json!({})),
        notification("tools/call", json!({"name": "fake_write"})),
        json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "fake_write", "_meta": common::meta(Some("c"))}}).to_string(),
        initialize(1, "2025-11-25", Some("c")),
        notification("tools/call", json!({"name": "fake_write"})),
    ]);
    assert_eq!(run.replies.len(), 1, "initialize の返事だけ");
    assert!(f.calls().is_empty(), "id のない tools/call は実行しない");
}

#[test]
fn batches_and_non_objects_are_minus_32600() {
    let mut f = fixture();
    let run = f.send(&[
        format!("[{}]", request(1, "ping", json!({}))),
        "[]".to_owned(),
        "1".to_owned(),
        "\"text\"".to_owned(),
    ]);
    for reply in &run.replies {
        assert_eq!(error_code(reply), Some(-32600), "{reply}");
        assert_eq!(reply["id"], Value::Null);
    }
    assert_eq!(run.replies.len(), 4);
}

#[test]
fn malformed_requests() {
    let mut f = fixture();
    let run = f.send(&[
        "{not json".to_owned(),
        "\u{feff}{}".to_owned(),
        json!({"jsonrpc": "2.0", "id": null, "method": "ping"}).to_string(),
        json!({"jsonrpc": "1.0", "id": 7, "method": "ping"}).to_string(),
        json!({"jsonrpc": "2.0", "id": 8}).to_string(),
        json!({"jsonrpc": "2.0", "id": true, "method": "ping"}).to_string(),
        json!({"jsonrpc": "2.0", "id": 9, "method": "ping", "params": [1]}).to_string(),
        json!({"jsonrpc": "2.0", "id": 10, "method": "tools/list", "params": {"_meta": "x"}})
            .to_string(),
    ]);
    let codes: Vec<Option<i64>> = run.replies.iter().map(error_code).collect();
    assert_eq!(
        codes,
        [
            Some(-32700),
            Some(-32700),
            Some(-32600),
            Some(-32600),
            Some(-32600),
            Some(-32600),
            Some(-32602),
            Some(-32602),
        ]
    );
    assert_eq!(run.replies[0]["id"], Value::Null);
    assert_eq!(run.replies[2]["id"], Value::Null);
    assert_eq!(run.replies[3]["id"], 7, "id が読めれば、そのまま返す");
    assert_eq!(run.replies[5]["id"], Value::Null);
}

/// 1行は 32 MiB まで。超えたら改行まで読み捨てて -32600（id は null）。次の行はふつうに処理する。
#[test]
fn lines_over_32_mib_are_cut() {
    let mut f = fixture();
    let mut input = vec![b' '; MAX_LINE_BYTES + 1];
    input.push(b'\n');
    input.extend_from_slice(modern(1, "ping", json!({}), Some("c")).as_bytes());
    input.push(b'\n');
    let run = f.send_raw(input);
    assert_eq!(run.replies.len(), 2);
    assert_eq!(error_code(&run.replies[0]), Some(-32600));
    assert_eq!(run.replies[0]["id"], Value::Null);
    assert_eq!(run.replies[1]["result"]["resultType"], "complete");
}

/// 入れ子が深すぎる JSON（128 を超える）は -32700 で、受付口に届かない。浅いものは届く。
#[test]
fn json_deeper_than_the_limit_is_a_parse_error() {
    let mut f = fixture();
    let run = f.send(&[deep_call(1, 200, Some("c")), deep_call(2, 100, Some("c"))]);
    assert_eq!(error_code(&run.replies[0]), Some(-32700));
    assert_eq!(run.replies[1]["result"]["isError"], false);
    assert_eq!(f.calls().len(), 1);
}

#[test]
fn arguments_missing_or_null_become_an_empty_object() {
    let mut f = fixture();
    f.send(&[
        modern(1, "tools/call", json!({"name": "fake_read"}), Some("c")),
        modern(
            2,
            "tools/call",
            json!({"name": "fake_read", "arguments": null}),
            Some("c"),
        ),
        modern(
            3,
            "tools/call",
            json!({"name": "fake_read", "arguments": [1]}),
            Some("c"),
        ),
    ]);
    let args: Vec<Value> = f.calls().into_iter().map(|c| c.arguments).collect();
    assert_eq!(
        args,
        [json!({}), json!({}), json!([1])],
        "ほかの形は受付口が確かめる"
    );
}

/// stdin の EOF で終わる。改行で終わらない最後の行も処理する。空白だけの行は無視する。
#[test]
fn eof_ends_serving_and_blank_lines_are_ignored() {
    let mut f = fixture();
    let mut input = b"\n   \r\n".to_vec();
    input.extend_from_slice(modern(1, "ping", json!({}), Some("c")).as_bytes());
    input.extend_from_slice(b"\r\n");
    input.extend_from_slice(modern(2, "ping", json!({}), Some("c")).as_bytes());
    let run = f.send_raw(input);
    assert_eq!(run.replies.len(), 2);
    assert_eq!(run.replies[1]["id"], 2);
    let run = f.send_raw(Vec::new());
    assert!(run.replies.is_empty());
}

#[test]
fn unknown_tools_unknown_methods_and_missing_names() {
    let mut f = fixture();
    let run = f.send(&[
        modern(1, "tools/call", json!({"name": "vault_format"}), Some("c")),
        modern(2, "tools/call", json!({}), Some("c")),
        modern(3, "tools/call", json!({"name": 5}), Some("c")),
        modern(4, "resources/list", json!({}), Some("c")),
        modern(5, "initialize", json!({}), Some("c")),
    ]);
    let codes: Vec<Option<i64>> = run.replies.iter().map(error_code).collect();
    assert_eq!(
        codes,
        [
            Some(-32602),
            Some(-32602),
            Some(-32602),
            Some(-32601),
            Some(-32601)
        ]
    );
}

/// ツールの処理が途中で止まったら -32603（サーバーは止まらない）。
#[test]
fn a_panicking_tool_is_an_internal_error() {
    let mut f = fixture();
    let run = f.send(&[
        modern(1, "tools/call", json!({"name": "fake_panic"}), Some("c")),
        modern(2, "ping", json!({}), Some("c")),
    ]);
    assert_eq!(error_code(&run.replies[0]), Some(-32603));
    assert_eq!(run.replies[1]["result"]["resultType"], "complete");
}

/// クライアントからの返事（method がない）には、返事をしない。
#[test]
fn responses_from_the_client_are_ignored() {
    let mut f = fixture();
    let run = f.send(&[
        json!({"jsonrpc": "2.0", "id": 1, "result": {}}).to_string(),
        json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -1, "message": "x"}}).to_string(),
    ]);
    assert!(run.replies.is_empty());
}

/// ログには、メソッド名・ツール名・結果のコード・かかった時間だけ。引数と中身は出さない。
/// stdout の返事は1行に1つで、中身の改行は1行の中に入る（エスケープされる）。
#[test]
fn logs_carry_only_names_codes_and_times() {
    let mut f = fixture();
    let run = f.send(&[
        modern(
            1,
            "tools/call",
            json!({"name": "fake_read", "arguments": {"path": "secret-argument.txt"}}),
            Some("c"),
        ),
        modern(2, "tools/call", json!({"name": "fake_fail"}), Some("c")),
        modern(3, "tools/call", json!({"name": "bad\nname"}), Some("c")),
        "{oops".to_owned(),
        notification("notifications/initialized", json!({})),
    ]);
    assert_eq!(run.replies.len(), 4);
    let lines: Vec<&str> = run.log.lines().collect();
    assert_eq!(lines.len(), 5, "{}", run.log);
    assert!(lines[0].starts_with("mcp: method=tools/call tool=fake_read result=ok ms="));
    assert!(lines[1].contains("tool=fake_fail result=NOT_FOUND"));
    assert!(lines[2].contains("tool=? result=-32602"));
    assert!(lines[3].contains("method=- tool=- result=-32700"));
    assert!(lines[4].contains("method=notifications/initialized"));
    for secret in ["secret-argument", "secret-file-content", "ない：a.txt"] {
        assert!(!run.log.contains(secret), "{secret} がログに出た");
    }
}

/// id は、文字列と、i64・u64 で表せる数だけ。そのまま返す。
#[test]
fn ids_are_echoed_as_given() {
    let mut f = fixture();
    let run = f.send(&[
        modern("abc", "ping", json!({}), Some("c")),
        modern(-5, "ping", json!({}), Some("c")),
        modern(u64::MAX, "ping", json!({}), Some("c")),
        modern(i64::MIN, "ping", json!({}), Some("c")),
    ]);
    let ids: Vec<Value> = run.replies.iter().map(|r| r["id"].clone()).collect();
    assert_eq!(
        ids,
        [json!("abc"), json!(-5), json!(u64::MAX), json!(i64::MIN)]
    );
    assert!(run.replies.iter().all(|r| r.get("error").is_none()));
    assert!(run.replies.iter().all(|r| r["jsonrpc"] == "2.0"));
}

/// i64・u64 で表せない数（小数・大きすぎる数・-0・指数）の id は -32600 で、id は null。
#[test]
fn ids_that_are_not_integers_are_rejected() {
    let mut f = fixture();
    let ping = |id: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"ping","params":{{"_meta":{}}}}}"#,
            common::meta(Some("c"))
        )
    };
    let run = f.send(&[
        ping("1.5"),
        ping("-0"),
        ping("1e3"),
        ping("1.0"),
        ping("18446744073709551616"),
        ping("-9223372036854775809"),
    ]);
    for reply in &run.replies {
        assert_eq!(error_code(reply), Some(-32600), "{reply}");
        assert_eq!(reply["id"], Value::Null);
    }
    assert_eq!(run.replies.len(), 6);
}

/// 1行の中の要素（文字列の外の [・{・,）は 100 万個まで。ちょうど 100 万個なら読み、超えたら読まずに -32600
/// （id は null。回数にも数えない）。文字列の中の記号は数えない（メモリの回帰）。
#[test]
fn the_number_of_elements_is_limited() {
    let head = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"fake_read","_meta":{},"arguments":{{"a":["#,
        common::meta(None)
    );
    let tail = "]}}}";
    let outside = |s: &str| s.chars().filter(|c| matches!(c, '[' | '{' | ',')).count();
    let line_with = |total: usize| {
        let zeros = total - outside(&head) - outside(tail) + 1;
        let mut line = head.clone();
        line.push_str(&"0,".repeat(zeros - 1));
        line.push('0');
        line.push_str(tail);
        (line, zeros)
    };
    let mut f = fixture();
    let (at_limit, zeros) = line_with(1_000_000);
    let reply = f.one(at_limit);
    assert_eq!(reply["result"]["isError"], false, "ちょうど 100 万個は読む");
    assert_eq!(f.calls()[0].arguments["a"].as_array().unwrap().len(), zeros);

    let (over, _) = line_with(1_000_001);
    let reply = f.one(over);
    assert_eq!(error_code(&reply), Some(-32600));
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(f.calls().len(), 1, "超えたものは受付口に届かない");

    // 文字列の中の , や [ は数えない
    let commas = ",[{".repeat(1_000_000);
    let reply = f.one(modern(
        2,
        "tools/call",
        json!({"name": "fake_read", "arguments": {"s": commas}}),
        Some("c"),
    ));
    assert_eq!(reply["result"]["isError"], false);

    // 超えたものは回数に数えない（新しいサーバーで、超えたものを送ってから 100 回使える）
    let mut f = fixture();
    let (over, _) = line_with(1_000_001);
    f.one(over);
    let pings: Vec<String> = (0..100)
        .map(|i| modern(i, "ping", json!({}), Some("c")))
        .collect();
    let run = f.send(&pings);
    assert!(run.replies.iter().all(|r| r.get("error").is_none()));
}

/// ログの ms は、1行を読み終えたあとから測る（次の行を待つ時間を入れない）。
#[test]
fn log_times_do_not_include_waiting_for_input() {
    let mut f = fixture();
    let line = |id: u64| {
        let mut bytes = modern(id, "ping", json!({}), Some("c")).into_bytes();
        bytes.push(b'\n');
        bytes
    };
    let input = SlowInput::new(vec![line(1), line(2), line(3)], Duration::from_millis(300));
    let run = f.send_from(input);
    assert_eq!(run.replies.len(), 3);
    let lines: Vec<&str> = run.log.lines().collect();
    assert_eq!(lines.len(), 3);
    for line in lines {
        assert!(log_ms(line) < 200, "待ち時間が入った：{line}");
    }
}

/// -32022 の data.requested は、印字できる ASCII で 64 字までのときだけ返す。
#[test]
fn requested_versions_are_echoed_only_when_short_and_printable() {
    let mut f = fixture();
    let ok = "v".repeat(64);
    for (version, echoed) in [
        (ok.as_str(), true),
        ("2025 11 25", true),
        (&*"v".repeat(65), false),
        ("2026-07-28\n", false),
        ("版", false),
    ] {
        let mut meta = common::meta(Some("c"));
        meta["io.modelcontextprotocol/protocolVersion"] = json!(version);
        let reply = f.one(request(1, "tools/list", json!({"_meta": meta})));
        assert_eq!(error_code(&reply), Some(-32022));
        let data = &reply["error"]["data"];
        assert_eq!(data["supported"], json!(["2026-07-28"]));
        assert_eq!(data.get("requested").is_some(), echoed, "{version:?}");
        if echoed {
            assert_eq!(data["requested"], version);
        }
    }
}

/// 受付口の結果はオブジェクトに限る。オブジェクトでなければ -32603。
#[test]
fn tool_results_must_be_objects() {
    let mut f = fixture();
    let reply = f.one(modern(
        1,
        "tools/call",
        json!({"name": "fake_scalar"}),
        Some("c"),
    ));
    assert_eq!(error_code(&reply), Some(-32603));
    f.one(initialize(2, "2025-06-18", Some("c")));
    let reply = f.one(request(3, "tools/call", json!({"name": "fake_scalar"})));
    assert_eq!(error_code(&reply), Some(-32603));
}

/// JSON の空白（空白・タブ・CR）だけの行だけを無視する。用紙送りなど、ほかの文字だけの行は -32700。
#[test]
fn only_json_whitespace_lines_are_ignored() {
    let mut f = fixture();
    let run = f.send(&[
        " \t \r".to_owned(),
        "\u{c}".to_owned(),
        "\u{b}".to_owned(),
        "\u{a0}".to_owned(),
    ]);
    let codes: Vec<Option<i64>> = run.replies.iter().map(error_code).collect();
    assert_eq!(codes, [Some(-32700), Some(-32700), Some(-32700)]);
}

/// `_meta: null` は「ない」として扱う。
#[test]
fn null_meta_is_no_meta() {
    let mut f = fixture();
    let reply = f.one(request(1, "tools/list", json!({"_meta": null})));
    assert_eq!(
        error_code(&reply),
        Some(-32602),
        "initialize の前の、_meta のない要求"
    );
    f.one(initialize(2, "2025-06-18", Some("c")));
    let reply = f.one(request(3, "tools/list", json!({"_meta": null})));
    assert!(
        reply["result"].get("resultType").is_none(),
        "Legacy で処理する"
    );
    assert!(reply["result"]["tools"].is_array());
}

/// clientInfo の形が違う（オブジェクトでない・name が文字列でない）ときは、ないものとして mcp:unknown。
#[test]
fn malformed_client_info_is_unknown() {
    for info in [
        json!("x"),
        json!({"name": 5}),
        json!({"name": ["a"]}),
        json!(null),
        json!([]),
    ] {
        // Modern
        let mut f = fixture();
        let mut meta = common::meta(None);
        meta["io.modelcontextprotocol/clientInfo"] = info.clone();
        let reply = f.one(request(
            1,
            "tools/call",
            json!({"name": "fake_read", "_meta": meta}),
        ));
        assert_eq!(reply["result"]["isError"], false, "{info}");
        assert_eq!(f.calls()[0].actor_id, "mcp:unknown", "{info}");
        // Legacy
        let mut f = fixture();
        f.one(request(
            1,
            "initialize",
            json!({"protocolVersion": "2025-11-25", "clientInfo": info}),
        ));
        f.one(request(2, "tools/call", json!({"name": "fake_read"})));
        assert_eq!(f.calls()[0].actor_id, "mcp:unknown", "{info}");
    }
}

/// 受付口は、作ったサーバーから見える（vault-mcp がつなぐ）。
#[test]
fn the_host_is_reachable() {
    let f: Fixture = fixture();
    assert!(!f.server.host().read_only);
}
