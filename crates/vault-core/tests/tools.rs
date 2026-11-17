//! ツール（`api::call`）の結合テスト：vault_info・vault_list・vault_read・vault_write・vault_mkdir・vault_move

mod common;

use std::fs;
use std::path::Path;

use common::{TestDir, call, env, expect_code, journal, new_vault, trash_entry};
use serde_json::{Value, json};
use vault_core::{EntryKind, ErrorCode, Op, TrashReason, Vault};

fn text(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key}: {v}"))
        .to_owned()
}

fn index(vault_path: &Path) -> Value {
    serde_json::from_slice(&fs::read(vault_path.join(".vault").join("index.json")).unwrap())
        .unwrap()
}

#[test]
fn info_returns_only_the_folder_name() {
    let dir = TestDir::new("tools-info");
    let (path, vault) = new_vault(&dir, "my vault");
    let info = call(&vault, "vault_info", Value::Null).unwrap();
    assert_eq!(info["name"], "my vault");
    assert_eq!(info["format"], 1);
    assert_eq!(info["read_only"], false);
    assert_eq!(info["vault_id"], vault.vault_id());
    assert_eq!(info.as_object().unwrap().len(), 4);
    let ro = Vault::open(&path, true, &env()).unwrap();
    assert_eq!(
        call(&ro, "vault_info", json!({})).unwrap()["read_only"],
        true
    );
}

/// 深いパス a/b/c.md の読み書き。途中のフォルダは作られ、履歴に mkdir が残る。
#[test]
fn deep_paths_can_be_written_and_read() {
    let dir = TestDir::new("tools-deep");
    let (path, vault) = new_vault(&dir, "v");
    let w = call(
        &vault,
        "vault_write",
        json!({"path": "a/b/c.md", "content": "こんにちは"}),
    )
    .unwrap();
    assert_eq!(w["path"], "a/b/c.md");
    assert_eq!(w["created"], true);
    assert_eq!(w["size"], "こんにちは".len());
    assert_eq!(w["previous_trash_id"], Value::Null);

    let r = call(&vault, "vault_read", json!({"path": "a/b/c.md"})).unwrap();
    assert_eq!(r["content"], "こんにちは");
    assert_eq!(r["encoding"], "utf8");
    assert_eq!(r["truncated"], false);
    assert_eq!(r["version"], w["version"]);
    assert_eq!(
        fs::read_to_string(path.join("a").join("b").join("c.md")).unwrap(),
        "こんにちは"
    );

    let ops: Vec<(Op, Option<String>)> =
        journal(&path).into_iter().map(|e| (e.op, e.path)).collect();
    assert_eq!(
        ops,
        [
            (Op::Init, None),
            (Op::Mkdir, Some("a".to_owned())),
            (Op::Mkdir, Some("a/b".to_owned())),
            (Op::Write, Some("a/b/c.md".to_owned())),
        ]
    );
    let idx = index(&path);
    assert_eq!(idx["a"]["kind"], "dir");
    assert_eq!(idx["a/b"]["kind"], "dir");
    assert_eq!(idx["a/b/c.md"]["kind"], "file");
    assert_eq!(idx["a/b/c.md"]["size"], "こんにちは".len());
    // 一時ファイルは残らない
    assert_eq!(
        fs::read_dir(path.join(".vault").join("tmp"))
            .unwrap()
            .count(),
        0
    );
}

/// 上書きで、古い中身がゴミ箱に入る（reason overwrite）。
#[test]
fn overwriting_moves_the_old_content_to_the_trash() {
    let dir = TestDir::new("tools-overwrite");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "n.txt", "content": "v1"}),
    )
    .unwrap();
    let w = call(
        &vault,
        "vault_write",
        json!({"path": "n.txt", "content": "v2!"}),
    )
    .unwrap();
    assert_eq!(w["created"], false);
    let trash_id = text(&w, "previous_trash_id");

    let data = path
        .join(".vault")
        .join("trash")
        .join(&trash_id)
        .join("data");
    assert_eq!(fs::read_to_string(&data).unwrap(), "v1");
    assert_eq!(fs::read_to_string(path.join("n.txt")).unwrap(), "v2!");
    let entry = trash_entry(&path, &trash_id);
    assert_eq!(entry.original_path, "n.txt");
    assert_eq!(entry.kind, EntryKind::File);
    assert_eq!(entry.size, 2);
    assert_eq!(entry.reason, TrashReason::Overwrite);
    assert_eq!(entry.actor.id(), "test");

    let last = journal(&path).pop().unwrap();
    assert_eq!(last.op, Op::Write);
    assert_eq!(last.trash_id.as_deref(), Some(trash_id.as_str()));
    assert_eq!(last.size, Some(3));
}

#[test]
fn overwrite_false_and_expected_version() {
    let dir = TestDir::new("tools-version");
    let (path, vault) = new_vault(&dir, "v");
    let w = call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "one"}),
    )
    .unwrap();
    let version = text(&w, "version");

    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "x", "overwrite": false}),
        ),
        ErrorCode::AlreadyExists,
        "overwrite:false",
    );
    // 版が合わなければ CONFLICT
    for wrong in ["0-0", "3-1", ""] {
        expect_code(
            call(
                &vault,
                "vault_write",
                json!({"path": "f.txt", "content": "x", "expected_version": wrong}),
            ),
            ErrorCode::Conflict,
            wrong,
        );
    }
    // ないファイルに版を渡しても CONFLICT
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "none.txt", "content": "x", "expected_version": version}),
        ),
        ErrorCode::Conflict,
        "ないファイル",
    );
    assert!(!path.join("none.txt").exists());
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "one");
    // 版が合えば書ける
    let w2 = call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "two", "expected_version": version}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "two");
    // 前の版では、もう書けない
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "three", "expected_version": version}),
        ),
        ErrorCode::Conflict,
        "古い版",
    );
    assert_ne!(w2["version"], version);
}

#[test]
fn write_errors() {
    let dir = TestDir::new("tools-write-errors");
    let (path, vault) = new_vault(&dir, "v");
    fs::create_dir(path.join("d")).unwrap();
    fs::write(path.join("file"), b"x").unwrap();

    let cases = [
        (json!({"path": "d", "content": "x"}), ErrorCode::NotAFile),
        (json!({"path": "", "content": "x"}), ErrorCode::InvalidPath),
        (
            json!({"path": "missing/x.txt", "content": "x", "create_dirs": false}),
            ErrorCode::NotFound,
        ),
        (
            json!({"path": "file/x.txt", "content": "x"}),
            ErrorCode::NotADirectory,
        ),
        (
            json!({"path": "b.bin", "content": "not base64!", "encoding": "base64"}),
            ErrorCode::InvalidArgument,
        ),
        (
            json!({"path": "../x", "content": "x"}),
            ErrorCode::InvalidPath,
        ),
        (
            json!({"path": ".vault/x", "content": "x"}),
            ErrorCode::ReservedPath,
        ),
        (
            json!({"path": "x", "content": "x", "extra": 1}),
            ErrorCode::InvalidArgument,
        ),
        (json!({"path": "x"}), ErrorCode::InvalidArgument),
        (
            json!({"path": "x", "content": 5}),
            ErrorCode::InvalidArgument,
        ),
        (
            json!({"path": "x", "content": "x", "encoding": "latin1"}),
            ErrorCode::InvalidArgument,
        ),
    ];
    for (args, code) in cases {
        let what = args.to_string();
        expect_code(call(&vault, "vault_write", args), code, &what);
    }
    assert!(!path.join("missing").exists());

    // 16 MiB を超える中身は TOO_LARGE（ちょうどは書ける）
    let big = "a".repeat(16 * 1024 * 1024 + 1);
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "big.txt", "content": big}),
        ),
        ErrorCode::TooLarge,
        "16 MiB + 1",
    );
    assert!(!path.join("big.txt").exists());
    let exact = "a".repeat(16 * 1024 * 1024);
    call(
        &vault,
        "vault_write",
        json!({"path": "big.txt", "content": exact}),
    )
    .unwrap();
}

#[test]
fn base64_round_trip_and_read_options() {
    let dir = TestDir::new("tools-read");
    let (_path, vault) = new_vault(&dir, "v");
    let bytes: Vec<u8> = (0..=255).collect();
    let encoded = vault_core::base64::encode(&bytes);
    call(
        &vault,
        "vault_write",
        json!({"path": "bin", "content": encoded, "encoding": "base64"}),
    )
    .unwrap();
    let r = call(
        &vault,
        "vault_read",
        json!({"path": "bin", "encoding": "base64"}),
    )
    .unwrap();
    assert_eq!(r["content"], encoded);
    assert_eq!(r["size"], 256);
    // utf8 では読めない
    expect_code(
        call(&vault, "vault_read", json!({"path": "bin"})),
        ErrorCode::NotUtf8,
        "utf8 で読む",
    );
    // offset と max_bytes
    let r = call(
        &vault,
        "vault_read",
        json!({"path": "bin", "encoding": "base64", "offset": 250, "max_bytes": 3}),
    )
    .unwrap();
    assert_eq!(
        vault_core::base64::decode(r["content"].as_str().unwrap()).unwrap(),
        [250, 251, 252]
    );
    assert_eq!(r["truncated"], true);
    // 終わりより先は空
    let r = call(&vault, "vault_read", json!({"path": "bin", "offset": 1000})).unwrap();
    assert_eq!(r["content"], "");
    assert_eq!(r["truncated"], false);

    // 文字の途中で切れるときは、完全な文字の分だけ
    call(
        &vault,
        "vault_write",
        json!({"path": "jp.txt", "content": "あいう"}),
    )
    .unwrap();
    let r = call(
        &vault,
        "vault_read",
        json!({"path": "jp.txt", "max_bytes": 4}),
    )
    .unwrap();
    assert_eq!(r["content"], "あ");
    assert_eq!(r["truncated"], true);
    let r = call(&vault, "vault_read", json!({"path": "jp.txt", "offset": 3})).unwrap();
    assert_eq!(r["content"], "いう");
    // 文字の途中から読むと NOT_UTF8
    expect_code(
        call(&vault, "vault_read", json!({"path": "jp.txt", "offset": 1})),
        ErrorCode::NotUtf8,
        "文字の途中から",
    );
    // 既定は 256 KiB まで
    call(
        &vault,
        "vault_write",
        json!({"path": "large.txt", "content": "b".repeat(300 * 1024)}),
    )
    .unwrap();
    let r = call(&vault, "vault_read", json!({"path": "large.txt"})).unwrap();
    assert_eq!(r["content"].as_str().unwrap().len(), 256 * 1024);
    assert_eq!(r["truncated"], true);
    assert_eq!(r["size"], 300 * 1024);

    let errors = [
        (json!({"path": "none"}), ErrorCode::NotFound),
        (json!({"path": ""}), ErrorCode::NotAFile),
        (
            json!({"path": "bin", "max_bytes": 16 * 1024 * 1024 + 1}),
            ErrorCode::TooLarge,
        ),
        (
            json!({"path": "bin", "offset": -1}),
            ErrorCode::InvalidArgument,
        ),
    ];
    for (args, code) in errors {
        let what = args.to_string();
        expect_code(call(&vault, "vault_read", args), code, &what);
    }
    call(&vault, "vault_mkdir", json!({"path": "dir"})).unwrap();
    expect_code(
        call(&vault, "vault_read", json!({"path": "dir"})),
        ErrorCode::NotAFile,
        "フォルダ",
    );
}

#[test]
fn list_orders_folders_first_and_recurses_depth_first() {
    let dir = TestDir::new("tools-list");
    let (path, vault) = new_vault(&dir, "v");
    for p in ["b.txt", "a.txt", "z/y.txt", "z/x/w.txt", "m/n.txt"] {
        call(&vault, "vault_write", json!({"path": p, "content": "1"})).unwrap();
    }
    call(&vault, "vault_mkdir", json!({"path": "empty"})).unwrap();

    let names = |v: &Value| -> Vec<String> {
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| text(e, "path"))
            .collect()
    };
    let top = call(&vault, "vault_list", Value::Null).unwrap();
    assert_eq!(names(&top), ["empty", "m", "z", "a.txt", "b.txt"]);
    assert_eq!(top["truncated"], false);
    let all = call(&vault, "vault_list", json!({"recursive": true})).unwrap();
    assert_eq!(
        names(&all),
        [
            "empty",
            "m",
            "m/n.txt",
            "z",
            "z/x",
            "z/x/w.txt",
            "z/y.txt",
            "a.txt",
            "b.txt"
        ]
    );
    let sub = call(&vault, "vault_list", json!({"path": "z"})).unwrap();
    assert_eq!(names(&sub), ["z/x", "z/y.txt"]);

    // 項目の形
    let a = &top["entries"][3];
    assert_eq!(a["name"], "a.txt");
    assert_eq!(a["kind"], "file");
    assert_eq!(a["size"], 1);
    assert!(text(a, "modified").ends_with('Z'));
    let r = call(&vault, "vault_read", json!({"path": "a.txt"})).unwrap();
    assert_eq!(a["version"], r["version"]);
    assert_eq!(top["entries"][0]["kind"], "dir");

    // limit と truncated
    let limited = call(&vault, "vault_list", json!({"recursive": true, "limit": 4})).unwrap();
    assert_eq!(names(&limited), ["empty", "m", "m/n.txt", "z"]);
    assert_eq!(limited["truncated"], true);
    let exact = call(&vault, "vault_list", json!({"limit": 5})).unwrap();
    assert_eq!(exact["truncated"], false);

    // どの深さでも .vault は出さない
    fs::create_dir_all(path.join("z").join(".VAULT")).unwrap();
    let all = call(&vault, "vault_list", json!({"recursive": true})).unwrap();
    assert!(
        !names(&all)
            .iter()
            .any(|n| n.to_lowercase().contains(".vault"))
    );

    let errors = [
        (json!({"path": "none"}), ErrorCode::NotFound),
        (json!({"path": "a.txt"}), ErrorCode::NotADirectory),
        (json!({"limit": 0}), ErrorCode::InvalidArgument),
        (json!({"limit": 10001}), ErrorCode::InvalidArgument),
        (json!({"path": "../x"}), ErrorCode::InvalidPath),
    ];
    for (args, code) in errors {
        let what = args.to_string();
        expect_code(call(&vault, "vault_list", args), code, &what);
    }
}

#[cfg(windows)]
#[test]
fn list_shows_links_without_their_targets() {
    use common::junction;

    let dir = TestDir::new("tools-list-link");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside-secret-place");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"s").unwrap();
    if !junction(&path.join("j"), &outside) {
        return;
    }
    let all = call(&vault, "vault_list", json!({"recursive": true})).unwrap();
    let entries = all["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "リンクの中には入らない");
    assert_eq!(entries[0]["path"], "j");
    assert_eq!(entries[0]["kind"], "link");
    let out = all.to_string();
    assert!(!out.contains("outside-secret-place") && !out.contains("secret.txt"));
    // ほかの操作は拒否
    expect_code(
        call(&vault, "vault_read", json!({"path": "j/secret.txt"})),
        ErrorCode::OutsideVault,
        "リンクの中を読む",
    );
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "j/new.txt", "content": "x"}),
        ),
        ErrorCode::OutsideVault,
        "リンクの中に書く",
    );
    assert!(!outside.join("new.txt").exists());
}

#[test]
fn mkdir_creates_parents_and_is_idempotent_for_folders() {
    let dir = TestDir::new("tools-mkdir");
    let (path, vault) = new_vault(&dir, "v");
    let m = call(&vault, "vault_mkdir", json!({"path": "p/q/r"})).unwrap();
    assert_eq!(m, json!({"path": "p/q/r", "created": true}));
    assert!(path.join("p").join("q").join("r").is_dir());
    let again = call(&vault, "vault_mkdir", json!({"path": "p/q/r"})).unwrap();
    assert_eq!(again["created"], false);
    call(&vault, "vault_write", json!({"path": "f", "content": "x"})).unwrap();
    expect_code(
        call(&vault, "vault_mkdir", json!({"path": "f"})),
        ErrorCode::AlreadyExists,
        "ファイルがある",
    );
    let mkdirs: Vec<String> = journal(&path)
        .into_iter()
        .filter(|e| e.op == Op::Mkdir)
        .filter_map(|e| e.path)
        .collect();
    assert_eq!(mkdirs, ["p", "p/q", "p/q/r"]);
}

#[test]
fn move_renames_without_overwriting() {
    let dir = TestDir::new("tools-move");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "A"}),
    )
    .unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "B"}),
    )
    .unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "d/x.txt", "content": "X"}),
    )
    .unwrap();

    // 移す先があれば ALREADY_EXISTS で、どちらも変わらない
    expect_code(
        call(
            &vault,
            "vault_move",
            json!({"from": "a.txt", "to": "b.txt"}),
        ),
        ErrorCode::AlreadyExists,
        "上書きしない",
    );
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "A");
    assert_eq!(fs::read_to_string(path.join("b.txt")).unwrap(), "B");

    let m = call(
        &vault,
        "vault_move",
        json!({"from": "a.txt", "to": "d/a2.txt"}),
    )
    .unwrap();
    assert_eq!(m, json!({"from": "a.txt", "to": "d/a2.txt"}));
    assert!(!path.join("a.txt").exists());
    assert_eq!(
        fs::read_to_string(path.join("d").join("a2.txt")).unwrap(),
        "A"
    );

    // フォルダごと移す（索引も移る）
    call(&vault, "vault_move", json!({"from": "d", "to": "e"})).unwrap();
    assert!(path.join("e").join("x.txt").is_file());
    let idx = index(&path);
    assert!(idx.get("d").is_none() && idx.get("d/x.txt").is_none());
    assert_eq!(idx["e"]["kind"], "dir");
    assert_eq!(idx["e/x.txt"]["kind"], "file");
    assert_eq!(idx["e/a2.txt"]["kind"], "file");

    let last = journal(&path).pop().unwrap();
    assert_eq!(last.op, Op::Move);
    assert_eq!(last.path.as_deref(), Some("d"));
    assert_eq!(last.to.as_deref(), Some("e"));

    let errors = [
        (json!({"from": "none", "to": "z"}), ErrorCode::NotFound),
        (
            json!({"from": "e", "to": "e/inner"}),
            ErrorCode::InvalidArgument,
        ),
        (
            json!({"from": "b.txt", "to": "nodir/b.txt"}),
            ErrorCode::NotFound,
        ),
        (
            json!({"from": "b.txt", "to": "b.txt"}),
            ErrorCode::AlreadyExists,
        ),
        (json!({"from": "", "to": "z"}), ErrorCode::InvalidPath),
        (
            json!({"from": "b.txt", "to": ".vault/b"}),
            ErrorCode::ReservedPath,
        ),
        (json!({"from": "b.txt"}), ErrorCode::InvalidArgument),
    ];
    for (args, code) in errors {
        let what = args.to_string();
        expect_code(call(&vault, "vault_move", args), code, &what);
    }
    assert_eq!(fs::read_to_string(path.join("b.txt")).unwrap(), "B");
}

/// 読み取り専用で、書くツール5つが READ_ONLY。読むツールは使える。管理用のデータも変えない。
#[test]
fn read_only_rejects_all_five_writing_tools() {
    let dir = TestDir::new("tools-read-only");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "A"}),
    )
    .unwrap();
    let before = journal(&path).len();
    let ro = Vault::open(&path, true, &env()).unwrap();
    let cases = [
        ("vault_write", json!({"path": "b.txt", "content": "B"})),
        ("vault_mkdir", json!({"path": "d"})),
        ("vault_move", json!({"from": "a.txt", "to": "c.txt"})),
        ("vault_delete", json!({"path": "a.txt"})),
        ("vault_restore", json!({"trash_id": "1-2-3"})),
    ];
    for (tool, args) in cases {
        expect_code(call(&ro, tool, args), ErrorCode::ReadOnly, tool);
    }
    // 公開の Rust の関数でも同じ
    expect_code(
        ro.mkdir(&common::actor(), "d"),
        ErrorCode::ReadOnly,
        "mkdir",
    );
    assert!(call(&ro, "vault_read", json!({"path": "a.txt"})).is_ok());
    assert!(call(&ro, "vault_list", json!({})).is_ok());
    assert!(!path.join("b.txt").exists() && !path.join("d").exists());
    assert!(path.join("a.txt").exists());
    assert_eq!(journal(&path).len(), before);
}

#[test]
fn unknown_tools_unfinished_tools_and_argument_shapes() {
    let dir = TestDir::new("tools-misc");
    let (_path, vault) = new_vault(&dir, "v");
    expect_code(
        call(&vault, "vault_format_disk", json!({})),
        ErrorCode::UnknownTool,
        "知らないツール",
    );
    // 10個すべてを作った：NOT_IMPLEMENTED を返すツールは残っていない
    assert!(call(&vault, "vault_changes", json!({})).is_ok());
    // 引数が null・空のオブジェクトは同じ。オブジェクトでなければ INVALID_ARGUMENT
    assert_eq!(
        call(&vault, "vault_info", Value::Null).unwrap(),
        call(&vault, "vault_info", json!({})).unwrap()
    );
    for bad in [json!([]), json!("x"), json!(1), json!({"unexpected": true})] {
        let what = bad.to_string();
        expect_code(
            call(&vault, "vault_info", bad),
            ErrorCode::InvalidArgument,
            &what,
        );
    }
}

/// 不変条件 (c)：外のファイルとハードリンクした中のファイルに vault_write しても、外の中身は変わらない。
#[test]
fn writing_a_hard_linked_file_does_not_change_the_outside_file() {
    let dir = TestDir::new("tools-hardlink");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside.txt");
    fs::write(&outside, b"outside").unwrap();
    fs::hard_link(&outside, path.join("inner.txt")).unwrap();

    let w = call(
        &vault,
        "vault_write",
        json!({"path": "inner.txt", "content": "new"}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
    assert_eq!(fs::read_to_string(path.join("inner.txt")).unwrap(), "new");
    // 古い名前（外とつながったもの）はゴミ箱へ移っただけ
    let data = path
        .join(".vault")
        .join("trash")
        .join(text(&w, "previous_trash_id"))
        .join("data");
    assert_eq!(fs::read_to_string(data).unwrap(), "outside");
}

/// 返り値とエラーの文に、根の絶対パスやユーザー名が出ない（Vault の Debug や resolve の値を入れない）。
#[test]
fn outputs_never_contain_absolute_paths_or_the_user_name() {
    let dir = TestDir::new("tools-no-abs");
    let (path, mut vault) = new_vault(&dir, "v");
    vault.set_min_rescan_interval(std::time::Duration::ZERO); // vault_changes のたびに走査する
    vault.set_rescan_interval_factor(0); // 前の走査の時間で伸ばさない（続けて走査する）
    let mut outputs = Vec::new();
    let mut run = |tool: &str, args: Value| {
        outputs.push(match call(&vault, tool, args) {
            Ok(v) => v.to_string(),
            Err(e) => e.to_string(),
        });
    };
    run("vault_info", json!({}));
    run("vault_write", json!({"path": "a/b.txt", "content": "x"}));
    run("vault_write", json!({"path": "a/b.txt", "content": "y"}));
    run(
        "vault_write",
        json!({"path": "a/b.txt", "content": "z", "expected_version": "1-1"}),
    );
    run(
        "vault_write",
        json!({"path": "a/b.txt", "content": "z", "overwrite": false}),
    );
    run("vault_write", json!({"path": "a", "content": "z"}));
    run(
        "vault_write",
        json!({"path": "q/r.txt", "content": "z", "create_dirs": false}),
    );
    run("vault_read", json!({"path": "a/b.txt"}));
    run("vault_read", json!({"path": "missing.txt"}));
    run("vault_read", json!({"path": "a"}));
    run("vault_list", json!({"recursive": true}));
    run("vault_list", json!({"path": "missing"}));
    run("vault_mkdir", json!({"path": "a/b.txt"}));
    run("vault_move", json!({"from": "a/b.txt", "to": "a"}));
    run("vault_move", json!({"from": "missing", "to": "x"}));
    run("vault_move", json!({"from": "a", "to": "nodir/x"}));
    run("vault_delete", json!({"path": "a"}));
    fs::write(path.join("outside-made.txt"), b"x").unwrap();
    fs::create_dir_all(path.join("deep").join("er")).unwrap();
    run("vault_changes", json!({}));
    run(
        "vault_changes",
        json!({"since_seq": 1, "limit": 1, "rescan": false}),
    );
    run("vault_changes", json!({"limit": 0}));
    fs::write(path.join(".vault").join("index.json"), b"broken").unwrap();
    run("vault_changes", json!({}));
    run("vault_nothing", json!({}));

    let canon = fs::canonicalize(&path).unwrap();
    let mut needles = vec![
        path.display().to_string(),
        canon.display().to_string(),
        path.display().to_string().replace('\\', "/"),
        env!("CARGO_TARGET_TMPDIR").to_owned(),
        "vault-core-tests".to_owned(),
        r"\\?\".to_owned(),
    ];
    for var in ["USERNAME", "USER"] {
        if let Ok(name) = std::env::var(var)
            && name.len() >= 3
        {
            needles.push(name);
        }
    }
    for out in &outputs {
        for needle in &needles {
            assert!(!out.contains(needle.as_str()), "{needle:?} が出た：{out}");
        }
        assert!(!out.contains("Vault {"), "Debug の出力が入った：{out}");
    }
}

/// Windows で、上書きするファイルがほかで開かれていると、100ms おきに3回試してから BUSY。中身は変わらない。
#[cfg(windows)]
#[test]
fn busy_files_are_retried_then_reported() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    let dir = TestDir::new("tools-busy");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    let before = journal(&path).len();
    // ほかのアプリが、共有なしで開いている
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path.join("f.txt"))
        .unwrap();
    let started = Instant::now();
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "new"}),
        ),
        ErrorCode::Busy,
        "使用中",
    );
    assert!(started.elapsed() >= Duration::from_millis(200), "3回試す");
    drop(holder);
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "old");
    assert_eq!(journal(&path).len(), before, "履歴は増えない");
    let vault_dir = path.join(".vault");
    assert_eq!(
        fs::read_dir(vault_dir.join("tmp")).unwrap().count(),
        0,
        "一時ファイルは残らない"
    );
    assert_eq!(
        fs::read_dir(vault_dir.join("trash")).unwrap().count(),
        0,
        "ゴミ箱の entry も残らない（移せなかったので片づけた）"
    );
    // 閉じたあとは書ける
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "new"}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "new");
}

/// 索引が読めないときは直さない（作り直すのは走査）。書く操作そのものは成功する。
#[test]
fn an_unreadable_index_is_left_for_the_scan_to_rebuild() {
    let dir = TestDir::new("tools-bad-index");
    let (path, vault) = new_vault(&dir, "v");
    let index_file = path.join(".vault").join("index.json");
    fs::write(&index_file, b"{broken").unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    call(&vault, "vault_mkdir", json!({"path": "d"})).unwrap();
    assert_eq!(fs::read(&index_file).unwrap(), b"{broken");
    // 索引がないときも同じ（作らない）
    fs::remove_file(&index_file).unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    )
    .unwrap();
    assert!(!index_file.exists());
    assert_eq!(journal(&path).len(), 4, "履歴には残る");
}
