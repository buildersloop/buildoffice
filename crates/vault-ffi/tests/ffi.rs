//! vault-ffi の結合テスト。C ABI の関数を Rust から呼ぶ。

mod common;

use std::fs;
use std::ptr;
use std::thread;

use common::{
    TestDir, call, call_raw, create, create_raw, error_code, journal, names_in, new_vault, ok,
    open, open_default_raw, open_raw, real_default_state, secrets, skip_when_real_default_exists,
};
use serde_json::{Value, json};
use vault_ffi::{vf_call, vf_close, vf_create, vf_open, vf_open_default};

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

    let request = json!({"parent": dir.path().to_str().unwrap(), "name": "c"}).to_string();
    // SAFETY: 上と同じ。
    let ptr = unsafe { vf_create(request.as_ptr(), request.len(), ptr::null_mut()) };
    assert!(ptr.is_null());
    assert!(!dir.join("c").exists(), "vf_create も何もしない");

    // vf_open_default も何もしない（本物のホームの既定の保管庫の場所の有無・種類は前後で同じ）。万一 null の確かめが
    // 壊れても本物のホームに作らないように、形の誤りの要求を渡す。
    // DefaultRequest の決まりを変えるときはこのテストを見直す（形の誤りの要求が通るようになると本物のホームに保管庫を
    // 作るため）。本物の既定の保管庫がすでにあるときは、ここだけを飛ばす
    if !skip_when_real_default_exists() {
        let before = real_default_state();
        let request = br#"{"x":1}"#;
        // SAFETY: 上と同じ。
        let ptr = unsafe { vf_open_default(request.as_ptr(), request.len(), ptr::null_mut()) };
        assert!(ptr.is_null());
        assert_eq!(real_default_state(), before, "本物のホームに触れない");
    }

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
    // vf_open_default の部分は、本物のホームを使う。DefaultRequest の決まりを変えるときはこのテストを見直す（形の誤りの
    // 要求が通るようになると本物のホームに保管庫を作るため）。本物の既定の保管庫がすでにあるときは、その部分だけを飛ばす
    let skip_default = skip_when_real_default_exists();
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

        let mut out_len = 0usize;
        // SAFETY: 上と同じ。
        let ptr = unsafe { vf_create(ptr::null(), len, &mut out_len) };
        // SAFETY: ptr と out_len は、いま vf_create が返したもの。
        let reply = unsafe { common::take(ptr, out_len) };
        assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{len}");

        // vf_open_default：null で長さがある・null で長さ 0（要求が空）は、どちらもホームに触れずに INVALID_ARGUMENT
        if !skip_default {
            let before = real_default_state();
            let mut out_len = 0usize;
            // SAFETY: 上と同じ。
            let ptr = unsafe { vf_open_default(ptr::null(), len, &mut out_len) };
            // SAFETY: ptr と out_len は、いま vf_open_default が返したもの。
            let reply = unsafe { common::take(ptr, out_len) };
            assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{len}");
            assert_eq!(real_default_state(), before, "本物のホームに触れない");
        }
    }
}

/// vf_open_default：要求の誤り（キーがある・JSON でない・空・配列・ほかの型・32 MiB を超える）は INVALID_ARGUMENT。
/// 要求を先に読むので、本物のホームの既定の保管庫の場所の有無・種類は前後で同じ（ホームに触れない）。文には、渡された
/// 値を入れない。成功の道は本物のホームに作るので、ここでは呼ばない（src のテストが偽のホームで見る）。
///
/// DefaultRequest の決まりを変えるときはこのテストを見直す（形の誤りの要求が通るようになると本物のホームに保管庫を
/// 作るため）。本物の既定の保管庫がすでにあるときは、「SKIP：…」を出してこのテストを飛ばす。
#[test]
fn open_default_rejects_broken_requests_without_touching_the_home() {
    if skip_when_real_default_exists() {
        return;
    }
    let before = real_default_state();
    let secret = r"C:\Profiles\someone-secret\v";
    let requests: Vec<Vec<u8>> = vec![
        json!({"x": secret}).to_string().into_bytes(),
        json!({"root": secret, "init": true})
            .to_string()
            .into_bytes(),
        json!({"parent": secret, "name": "someone-secret-name"})
            .to_string()
            .into_bytes(),
        json!({"name": "Buildoffice"}).to_string().into_bytes(),
        b"not json".to_vec(),
        Vec::new(),
        b"[]".to_vec(),
        b"null".to_vec(),
        b"{".to_vec(),
        b"{}{}".to_vec(),
        vec![0xff, 0xfe, 0x00],
    ];
    for request in &requests {
        let reply = open_default_raw(request);
        assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{reply}");
        assert!(!reply.to_string().contains("someone-secret"), "{reply}");
        assert_eq!(real_default_state(), before, "本物のホームに触れない");
    }
    // 32 MiB を超える要求は、読まずに断る
    let big = vec![b' '; vault_ffi::MAX_ALLOC_BYTES + 1];
    assert_eq!(error_code(&open_default_raw(&big)), "INVALID_ARGUMENT");
    // 要素が多すぎる要求も、読まずに断る
    let over = format!(r#"{{"x":[{}0]}}"#, "0,".repeat(1_000_000));
    let reply = open_default_raw(over.as_bytes());
    assert_eq!(error_code(&reply), "INVALID_ARGUMENT");
    assert_eq!(real_default_state(), before, "本物のホームに触れない");
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
    let create_requests: Vec<Vec<u8>> = vec![
        b"{".to_vec(),
        b"not json".to_vec(),
        b"[]".to_vec(),
        b"{}".to_vec(),
        vec![0xff, 0xfe, 0x00],
        json!({"parent": secret}).to_string().into_bytes(),
        json!({"name": "someone-secret-name"})
            .to_string()
            .into_bytes(),
        json!({"parent": secret, "name": 5})
            .to_string()
            .into_bytes(),
        json!({"parent": [secret], "name": "someone-secret-name"})
            .to_string()
            .into_bytes(),
        json!({"parent": secret, "name": "v", "extra": secret})
            .to_string()
            .into_bytes(),
        json!({"root": secret, "init": true})
            .to_string()
            .into_bytes(),
    ];
    for request in &create_requests {
        let reply = create_raw(request);
        assert_eq!(error_code(&reply), "INVALID_ARGUMENT", "{reply}");
        assert!(!reply.to_string().contains("someone-secret"), "{reply}");
    }
    // 32 MiB を超える要求は、読まずに断る
    let big = vec![b' '; vault_ffi::MAX_ALLOC_BYTES + 1];
    assert_eq!(error_code(&open_raw(&big)), "INVALID_ARGUMENT");
    assert_eq!(error_code(&call_raw(handle, &big)), "INVALID_ARGUMENT");
    assert_eq!(error_code(&create_raw(&big)), "INVALID_ARGUMENT");
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

/// vf_create：置き場所の中に名前を付けて作り、書ける状態で開く。履歴の init の actor は {user, app}、
/// vault_info の name は渡した名前。置き場所は、すでにあるフォルダ。
#[test]
fn create_makes_a_vault_and_opens_it_for_writing() {
    let dir = TestDir::new("ffi-create");
    let existing_parent = dir.join("existing-parent");
    fs::create_dir(&existing_parent).unwrap();
    for (parent, name) in [
        (dir.path().to_path_buf(), "MyVault"),
        (existing_parent, "日本語の保管庫"),
    ] {
        let reply = create(&parent, name);
        let handle = ok(&reply)["handle"].as_u64().unwrap();
        let path = parent.join(name);
        let info = ok(&call(handle, "vault_info", json!({})));
        assert_eq!(info["name"], name);
        assert_eq!(info["read_only"], false);
        ok(&call(
            handle,
            "vault_write",
            json!({"path": "a.txt", "content": "a"}),
        ));
        assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
        let entries = journal(&path);
        assert_eq!(entries[0]["op"], "init");
        assert_eq!(entries[0]["actor"]["kind"], "user");
        assert_eq!(entries[0]["actor"]["id"], "app");
        // 作った保管庫は、vf_open（init:false）でも開ける
        let again = ok(&open(
            json!({"root": path.to_str().unwrap(), "init": false}),
        ))["handle"]
            .as_u64()
            .unwrap();
        vf_close(again);
        vf_close(handle);
    }
    // 置き場所の末尾に区切りがあってもよい
    let trailing = format!("{}{}", dir.path().display(), std::path::MAIN_SEPARATOR);
    let reply = create_raw(
        json!({"parent": trailing, "name": "t"})
            .to_string()
            .as_bytes(),
    );
    vf_close(ok(&reply)["handle"].as_u64().unwrap());
    assert!(dir.join("t").join(".vault").is_dir());
}

/// vf_create：使えない名前は INVALID_ROOT（「保管庫の名前：」）で、置き場所の中に何も作らない。
#[test]
fn create_rejects_bad_names_and_makes_nothing() {
    let dir = TestDir::new("ffi-create-names");
    let long = "a".repeat(256);
    for name in [
        "",
        r"a\b",
        "a/b",
        ".",
        "..",
        "CON",
        "nul.txt",
        "x.",
        "x ",
        "a:b",
        "a*b",
        "a\u{202E}b",
        ".vault",
        long.as_str(),
        "file~1",
    ] {
        let reply = create(dir.path(), name);
        assert_eq!(error_code(&reply), "INVALID_ROOT", "{name:?}");
        let message = reply["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("保管庫の名前："), "{name:?}: {message}");
        assert!(names_in(dir.path()).is_empty(), "{name:?} で何かを作った");
    }
}

/// vf_create：置き場所のフォルダが、すでにあるときだけ作る。ない（何段でも）・ファイル・途中がファイルは INVALID_ROOT
/// （「置き場所のフォルダがない」。パスも名前も入れない）で、何も作らない。vault-mcp の init は、まだない祖先を作ってよい
/// （そちらは変えない）ので、同じ場所を init:true の vf_open で作ると、作れる（この2つの違いを確かめる）。
#[test]
fn create_needs_an_existing_parent_folder_and_makes_nothing_otherwise() {
    let dir = TestDir::new("ffi-create-parent");
    let file = dir.join("a-file.txt");
    fs::write(&file, "keep").unwrap();
    let before = names_in(dir.path());
    let missing = dir.join("no-such-parent");
    let deep_missing = dir.join("no-such-a").join("b").join("c");
    let under_file = file.join("sub");
    for (what, parent) in [
        ("ない置き場所", missing.clone()),
        ("何段もない置き場所", deep_missing.clone()),
        ("ファイルの置き場所", file.clone()),
        ("途中がファイルの置き場所", under_file.clone()),
    ] {
        let reply = create(&parent, "secret-vault-name");
        assert_eq!(error_code(&reply), "INVALID_ROOT", "{what}");
        let message = reply["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("置き場所のフォルダがない"),
            "{what}: {message}"
        );
        assert!(!message.starts_with("保管庫の名前："), "{what}: {message}");
        let all = reply.to_string();
        for needle in secrets(&parent)
            .into_iter()
            .chain(["secret-vault-name".to_owned()])
        {
            assert!(!all.contains(&needle), "{what}: {needle:?} が出た");
        }
        assert_eq!(names_in(dir.path()), before, "{what}: 何も作らない");
        assert!(
            !missing.exists() && !dir.join("no-such-a").exists(),
            "{what}"
        );
    }
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "keep",
        "ファイルは変わらない"
    );

    // 置き場所の誤り・名前の誤りは、これまでどおり先に断る（置き場所がなくても、名前の誤りは「保管庫の名前：」）
    let reply = create(&missing, "x.");
    assert_eq!(error_code(&reply), "INVALID_ROOT");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("保管庫の名前：")
    );
    assert_eq!(names_in(dir.path()), before);

    // 置き場所を作れば、同じ要求で作れる（断った理由は置き場所だけだった）
    fs::create_dir(&missing).unwrap();
    let reply = create(&missing, "secret-vault-name");
    vf_close(ok(&reply)["handle"].as_u64().unwrap());
    assert!(missing.join("secret-vault-name").join(".vault").is_dir());

    // vf_open の init:true（vault-mcp の init と同じ vault-core の init）は、まだない祖先を作ってよい（今のまま）
    let nested = deep_missing.join("v");
    let reply = open(json!({"root": nested.to_str().unwrap(), "init": true}));
    vf_close(ok(&reply)["handle"].as_u64().unwrap());
    assert!(nested.join(".vault").is_dir());
}

/// vf_create：保管庫にできないフォルダ（空でない・すでに保管庫・保管庫の中）と、置き場所の誤り（相対・UNC・
/// `\\?\` の形）は INVALID_ROOT で、中身を変えない・何も作らない。
#[test]
fn create_refuses_folders_that_cannot_become_vaults() {
    let dir = TestDir::new("ffi-create-folders");
    // 空でないフォルダ（隠しファイルだけでも）
    let full = dir.join("full");
    fs::create_dir(&full).unwrap();
    write_hidden(&full.join(".hidden"), b"keep");
    let reply = create(dir.path(), "full");
    assert_eq!(error_code(&reply), "INVALID_ROOT");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("空でない")
    );
    assert_eq!(names_in(&full), [".hidden"], "中身は変わらない");
    assert_eq!(fs::read(full.join(".hidden")).unwrap(), b"keep");

    // すでに保管庫のフォルダ・保管庫の中
    let (vault, handle) = new_vault(&dir, "v");
    let before = names_in(&vault);
    let reply = create(dir.path(), "v");
    assert_eq!(error_code(&reply), "INVALID_ROOT");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("すでに保管庫")
    );
    let reply = create(&vault, "inner");
    assert_eq!(error_code(&reply), "INVALID_ROOT");
    assert_eq!(names_in(&vault), before, "保管庫の中に何も作らない");
    // 保管庫の中の、すでにあるフォルダ（置き場所がなければ、別の理由で断られるので、先に作っておく）
    let sub = vault.join("sub");
    fs::create_dir(&sub).unwrap();
    let before_sub = names_in(&vault);
    let reply = create(&sub, "inner");
    assert_eq!(error_code(&reply), "INVALID_ROOT");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("保管庫の中に保管庫は作れない"),
        "{reply}"
    );
    assert!(
        names_in(&sub).is_empty(),
        "保管庫の中のフォルダに何も作らない"
    );
    assert_eq!(names_in(&vault), before_sub);
    vf_close(handle);

    // 置き場所が相対パス・UNC・\\?\ の形
    let before = names_in(dir.path());
    for parent in [
        "relative".to_owned(),
        r"relative\p".to_owned(),
        r"\\vault-test-no-such-server\share".to_owned(),
        format!(r"\\?\{}", dir.path().display()),
        format!(r"{}\..\x", dir.path().display()),
    ] {
        let reply = create_raw(
            json!({"parent": parent, "name": "v2"})
                .to_string()
                .as_bytes(),
        );
        assert_eq!(error_code(&reply), "INVALID_ROOT", "{parent}");
        let message = reply["error"]["message"].as_str().unwrap();
        assert!(
            !message.starts_with("保管庫の名前："),
            "{parent}: {message}"
        );
    }
    assert_eq!(names_in(dir.path()), before, "何も作らない");
    assert!(!std::path::Path::new("relative").exists());
}

/// 隠しファイルを書く（Windows では隠しの印を付ける。ほかでは名前の . だけ）。
fn write_hidden(path: &std::path::Path, bytes: &[u8]) {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_ATTRIBUTE_HIDDEN
        options.attributes(0x2);
    }
    let mut file = options.open(path).unwrap();
    std::io::Write::write_all(&mut file, bytes).unwrap();
}

/// vf_create：守る場所（%LOCALAPPDATA%・%APPDATA%・SystemRoot の下、ホームの直下の . から始まるフォルダ）は
/// INVALID_ROOT で、何も作らない。名前は、この PC に残らないよう、テストごとに違うものにする。
#[cfg(windows)]
#[test]
fn create_refuses_protected_places() {
    let name = format!(
        "vault-ffi-test-should-not-exist-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    for var in ["LOCALAPPDATA", "APPDATA", "SystemRoot"] {
        let parent = std::path::PathBuf::from(std::env::var_os(var).expect(var));
        let reply = create(&parent, &name);
        assert_eq!(error_code(&reply), "INVALID_ROOT", "{var}");
        assert!(!parent.join(&name).exists(), "{var} の下に作った");
    }
    let home = std::path::PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
    let dotted = format!(".{name}");
    let reply = create(&home, &dotted);
    assert_eq!(error_code(&reply), "INVALID_ROOT");
    assert!(!home.join(&dotted).exists(), "ホームの直下に作った");
}

/// vf_create：置き場所が、守る場所（%LOCALAPPDATA%）を指すジャンクションでも、リンクをたどった先で判定して
/// INVALID_ROOT で、何も作らない（置き場所はあるフォルダなので、置き場所の確かめは通る。回帰テスト）。
/// ジャンクションは、テスト用のフォルダの下に作り、テストの最後にリンクだけを外す（指す先は消さない）。
/// ジャンクションを作れないボリュームでは、飛ばして SKIP と出す。
#[cfg(windows)]
#[test]
fn create_refuses_a_junction_that_points_at_a_protected_place() {
    use common::{Junction, unique_name};

    let local = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA"));
    let dir = TestDir::new("ffi-create-junction");
    // dir より後に作る（Drop は逆の順なので、リンクが先に外れる）
    let link = dir.join("to-localappdata");
    let Some(_junction) = Junction::new("create_refuses_a_junction", &link, &local) else {
        return;
    };
    let name = unique_name("vault-ffi-test-should-not-exist");
    let reply = create(&link, &name);
    assert_eq!(error_code(&reply), "INVALID_ROOT", "{reply}");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(
        !message.starts_with("保管庫の名前：") && !message.contains("置き場所のフォルダがない"),
        "リンクの先を守る場所と判定して断る：{message}"
    );
    assert!(!local.join(&name).exists(), "%LOCALAPPDATA% の下に作った");
    assert!(!link.join(&name).exists(), "リンクの先に作った");
    // リンクの先を指す文は出さない
    let all = reply.to_string();
    for needle in secrets(&link).into_iter().chain([name]) {
        assert!(!all.contains(&needle), "{needle:?} が出た");
    }
}

/// vf_create：置き場所が 8.3 の短い名前（`C:\PROGRA~1` = Program Files）でも、守る場所として INVALID_ROOT で、何も作らない
/// （回帰テスト）。`C:\PROGRA~1` が実在するときだけ動かす（8.3 の名前を作らない設定の PC では、飛ばして SKIP と出す）。
#[cfg(windows)]
#[test]
fn create_refuses_an_8_3_short_name_of_a_protected_place() {
    use common::{skip, unique_name};

    let short = std::path::Path::new(r"C:\PROGRA~1");
    if !short.is_dir() {
        skip(
            "create_refuses_an_8_3_short_name",
            r"C:\PROGRA~1 がない（8.3 の短い名前を作らない設定か、Program Files が C: にない）",
        );
        return;
    }
    let long = std::path::PathBuf::from(r"C:\Program Files");
    let name = unique_name("vault-ffi-test-should-not-exist");
    let reply = create(short, &name);
    assert_eq!(error_code(&reply), "INVALID_ROOT", "{reply}");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(
        !message.starts_with("保管庫の名前：") && !message.contains("置き場所のフォルダがない"),
        "短い名前を守る場所と判定して断る：{message}"
    );
    assert!(!short.join(&name).exists(), "短い名前の下に作った");
    assert!(!long.join(&name).exists(), "Program Files の下に作った");
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
    // vf_create：成功・すでに保管庫・保管庫の中・空でないフォルダ・名前の誤り（名前は返さない）
    fs::write(plain.join("note.txt"), "x").unwrap();
    let made = create(dir.path(), "made");
    let made_handle = ok(&made)["handle"].as_u64().unwrap();
    replies.push(made);
    replies.push(create(dir.path(), "v"));
    replies.push(create(&path, "inner"));
    replies.push(create(dir.path(), "plain"));
    for name in ["secret-vault-name.", "secret-vault-name/x", "CON", ""] {
        replies.push(create(dir.path(), name));
    }
    let all: String = replies
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    for needle in secrets(&path)
        .into_iter()
        .chain(secrets(&dir.join("made")))
        .chain(["secret-vault-name".to_owned()])
    {
        assert!(!all.contains(&needle), "{needle:?} が出た");
    }
    vf_close(handle);
    vf_close(made_handle);
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
