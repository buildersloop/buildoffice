//! 起動のしかたの結合テスト：引数の誤り・--help・
//! `vault-mcp init`・初期化していない保管庫での起動。どれも stdout には何も出さない（MCP のメッセージ専用）。

mod common;

use std::fs;

use common::{Mcp, TestDir, init_vault, journal, run, secrets};
use serde_json::json;

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// 引数の誤りは、使い方を stderr に出して、終了コード 2。--help は 0。
#[test]
fn usage_on_mistakes_and_help() {
    for args in [
        &[][..],
        &["--secret-flag"],
        &["--vault"],
        &["init"],
        &["--read-only"],
    ] {
        let os: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_ref()).collect();
        let out = run(&os);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let text = stderr(&out);
        assert!(
            text.contains("使い方") && text.contains("引数の誤り"),
            "{text}"
        );
        assert!(!text.contains("secret-flag"), "値は表示しない");
    }
    let out = run(&["--help".as_ref()]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    assert!(stderr(&out).contains("vault-mcp init"));
}

/// `vault-mcp init <絶対パス>`：保管庫を作って終わる。履歴の init の actor は {user, cli}。
/// 空でないフォルダ・相対パス・保管庫にできない場所は、作らずに終了コード 1（stderr に絶対パスを出さない）。
#[test]
fn init_creates_a_vault_and_refuses_bad_places() {
    let dir = TestDir::new("cli-init");
    let path = init_vault(&dir, "v");
    assert!(path.join(".vault").join("vault.json").is_file());
    let entries = journal(&path);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["op"], "init");
    assert_eq!(entries[0]["actor"]["kind"], "user");
    assert_eq!(entries[0]["actor"]["id"], "cli");

    // 空でないフォルダ
    let full = dir.join("full");
    fs::create_dir(&full).unwrap();
    fs::write(full.join("a.txt"), b"a").unwrap();
    let out = run(&["init".as_ref(), full.as_os_str()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(!full.join(".vault").exists());
    let text = stderr(&out);
    assert!(text.contains("保管庫を作れない"), "{text}");
    for needle in secrets(&full) {
        assert!(!text.contains(&needle), "{needle:?} が出た：{text}");
    }
    // すでに保管庫：そうとわかる文だけで、場所のヒント（vault-mcp --help）は付けない
    let out = run(&["init".as_ref(), path.as_os_str()]);
    assert_eq!(out.status.code(), Some(1));
    let text = stderr(&out);
    assert!(text.contains("すでに保管庫になっている"), "{text}");
    assert!(
        !text.contains("vault-mcp --help") && !text.contains("保管庫にできる場所"),
        "{text}"
    );
    // 相対パス
    let out = run(&["init".as_ref(), "relative-vault".as_ref()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("INVALID_ROOT"), "{}", stderr(&out));
}

/// 初期化していない保管庫では、`--vault` での起動に失敗する（作らない）。stderr に init の案内を出す。
#[test]
fn serving_needs_an_initialized_vault() {
    let dir = TestDir::new("cli-not-a-vault");
    let plain = dir.join("plain");
    fs::create_dir(&plain).unwrap();
    for target in [plain.clone(), dir.join("missing")] {
        let out = run(&["--vault".as_ref(), target.as_os_str()]);
        assert_eq!(out.status.code(), Some(1), "{target:?}");
        assert!(out.stdout.is_empty());
        let text = stderr(&out);
        assert!(
            text.contains("NOT_A_VAULT") && text.contains("vault-mcp init"),
            "{text}"
        );
        for needle in secrets(dir.path()) {
            assert!(!text.contains(&needle), "{needle:?} が出た：{text}");
        }
        assert!(!target.join(".vault").exists(), "作らない");
    }
    assert!(!dir.join("missing").exists(), "ないフォルダも作らない");
}

/// `--vault` と `--actor-id` の値が `--` で始まるときは、引数の誤り（終了コード 2）で、何も起動しない
/// （`--actor-id --read-only` で、読み取り専用が黙って外れないように）。
#[test]
fn values_starting_with_dashes_are_mistakes() {
    let dir = TestDir::new("cli-dashes");
    let path = init_vault(&dir, "v");
    let before = journal(&path).len();
    let p = path.as_os_str();
    for args in [
        vec![
            "--vault".as_ref(),
            p,
            "--actor-id".as_ref(),
            "--read-only".as_ref(),
        ],
        vec!["--vault".as_ref(), "--read-only".as_ref()],
        vec![
            "--read-only".as_ref(),
            "--vault".as_ref(),
            "--actor-id".as_ref(),
            "x".as_ref(),
        ],
        vec![
            "--vault".as_ref(),
            p,
            "--actor-id".as_ref(),
            "--vault".as_ref(),
        ],
    ] {
        // stdin に書き込みの要求を入れても、起動しないので何も起きない
        let out = std::process::Command::new(common::EXE)
            .args(&args)
            .env_remove(common::TEST_PANIC_ENV)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": "vault_write", "arguments": {"path": "x.txt", "content": "x"},
                    "_meta": common::meta(Some("c"))}});
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = writeln!(stdin, "{line}");
                }
                child.wait_with_output()
            })
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(stderr(&out).contains("引数の誤り"), "{}", stderr(&out));
    }
    assert!(!path.join("x.txt").exists());
    assert_eq!(journal(&path).len(), before, "何も書いていない");
}

/// 保管庫を開いたら、stdin の EOF で終了コード 0 で終わる。
#[test]
fn serving_ends_at_eof() {
    let dir = TestDir::new("cli-eof");
    let path = init_vault(&dir, "v");
    let mut mcp = Mcp::start(&path, &[]);
    let reply = mcp.send(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string());
    assert_eq!(reply["result"], json!({}));
    let done = mcp.finish();
    assert_eq!(done.status.code(), Some(0));
    assert!(done.rest.is_empty());
}
