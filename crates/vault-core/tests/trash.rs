//! 削除・ゴミ箱の一覧・復元・orphan_tmp（vault_delete・vault_trash_list・vault_restore）。

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{TestDir, call, env, expect_code, journal, new_vault, set_read_only};
use serde_json::{Value, json};
use vault_core::{ActorKind, ErrorCode, Op, Vault};

fn text(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key}: {v}"))
        .to_owned()
}

fn trash_dir(vault_path: &Path) -> PathBuf {
    vault_path.join(".vault").join("trash")
}

fn index(vault_path: &Path) -> Value {
    serde_json::from_slice(&fs::read(vault_path.join(".vault").join("index.json")).unwrap())
        .unwrap()
}

fn items(vault: &Vault) -> Vec<Value> {
    call(vault, "vault_trash_list", json!({})).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn delete_moves_to_the_trash_and_restore_brings_it_back() {
    let dir = TestDir::new("trash-basic");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "hello"}),
    )
    .unwrap();

    let d = call(&vault, "vault_delete", json!({"path": "a.txt"})).unwrap();
    assert_eq!(d["path"], "a.txt");
    let id = text(&d, "trash_id");
    assert!(!path.join("a.txt").exists());
    let list = items(&vault);
    assert_eq!(list.len(), 1);
    let item = &list[0];
    assert_eq!(item["trash_id"], id.as_str());
    assert_eq!(item["original_path"], "a.txt");
    assert_eq!(item["kind"], "file");
    assert_eq!(item["size"], 5);
    assert_eq!(item["reason"], "delete");
    assert_eq!(item["actor"]["id"], "test");
    assert!(text(item, "deleted_at").ends_with('Z'));
    let last = journal(&path).pop().unwrap();
    assert_eq!(
        (last.op, last.path.as_deref(), last.trash_id.as_deref()),
        (Op::Delete, Some("a.txt"), Some(id.as_str()))
    );
    assert!(index(&path).get("a.txt").is_none());

    let r = call(&vault, "vault_restore", json!({"trash_id": id})).unwrap();
    assert_eq!(r, json!({"path": "a.txt", "replaced_trash_id": null}));
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "hello");
    assert_eq!(
        fs::read_dir(trash_dir(&path)).unwrap().count(),
        0,
        "entry も片づく"
    );
    assert!(items(&vault).is_empty());
    let last = journal(&path).pop().unwrap();
    assert_eq!(
        (last.op, last.path.as_deref(), last.trash_id.as_deref()),
        (Op::Restore, Some("a.txt"), Some(id.as_str()))
    );
    assert_eq!(index(&path)["a.txt"]["kind"], "file");
    // 2回目はもうない
    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": id})),
        ErrorCode::NotFound,
        "2回目",
    );
}

#[test]
fn delete_rules() {
    let dir = TestDir::new("trash-delete-rules");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "d/x.txt", "content": "x"}),
    )
    .unwrap();
    call(&vault, "vault_mkdir", json!({"path": "empty"})).unwrap();

    expect_code(
        call(&vault, "vault_delete", json!({"path": "none"})),
        ErrorCode::NotFound,
        "ない",
    );
    expect_code(
        call(&vault, "vault_delete", json!({"path": ""})),
        ErrorCode::InvalidPath,
        "根",
    );
    expect_code(
        call(&vault, "vault_delete", json!({"path": "d"})),
        ErrorCode::DirNotEmpty,
        "中のあるフォルダ",
    );
    assert!(path.join("d").join("x.txt").exists());
    // 空のフォルダは recursive なしで消せる
    call(&vault, "vault_delete", json!({"path": "empty"})).unwrap();
    // recursive なら中身ごと、1つの entry として移る
    let d = call(
        &vault,
        "vault_delete",
        json!({"path": "d", "recursive": true}),
    )
    .unwrap();
    let data = trash_dir(&path).join(text(&d, "trash_id")).join("data");
    assert_eq!(fs::read_to_string(data.join("x.txt")).unwrap(), "x");
    let item = items(&vault)
        .into_iter()
        .find(|i| i["original_path"] == "d")
        .unwrap();
    assert_eq!(item["kind"], "dir");
    assert_eq!(item["size"], 1, "フォルダの大きさは、中のファイルの合計");
    // 索引からも、中ごと除く
    let idx = index(&path);
    assert!(idx.get("d").is_none() && idx.get("d/x.txt").is_none());

    // 読み取り専用の印のファイルは消さない。フォルダごとなら移してよい（戻せるため）
    call(
        &vault,
        "vault_write",
        json!({"path": "keep/ro.txt", "content": "ro"}),
    )
    .unwrap();
    let ro = path.join("keep").join("ro.txt");
    set_read_only(&ro, true);
    let file_result = call(&vault, "vault_delete", json!({"path": "keep/ro.txt"}));
    let dir_result = call(
        &vault,
        "vault_delete",
        json!({"path": "keep", "recursive": true}),
    );
    expect_code(file_result, ErrorCode::AccessDenied, "読み取り専用の印");
    let d = dir_result.unwrap();
    let moved = trash_dir(&path)
        .join(text(&d, "trash_id"))
        .join("data")
        .join("ro.txt");
    set_read_only(&moved, false);
    assert_eq!(fs::read_to_string(moved).unwrap(), "ro");

    for (args, code) in [
        (
            json!({"path": "x", "recursive": "yes"}),
            ErrorCode::InvalidArgument,
        ),
        (
            json!({"path": "x", "force": true}),
            ErrorCode::InvalidArgument,
        ),
        (json!({"path": ".vault"}), ErrorCode::ReservedPath),
        (json!({"path": "../x"}), ErrorCode::InvalidPath),
    ] {
        let what = args.to_string();
        expect_code(call(&vault, "vault_delete", args), code, &what);
    }
}

#[test]
fn trash_list_is_newest_first_and_skips_broken_entries() {
    let dir = TestDir::new("trash-list");
    let (path, vault) = new_vault(&dir, "v");
    let mut ids = Vec::new();
    for name in ["1.txt", "2.txt", "3.txt", "4.txt"] {
        call(
            &vault,
            "vault_write",
            json!({"path": name, "content": name}),
        )
        .unwrap();
        let d = call(&vault, "vault_delete", json!({"path": name})).unwrap();
        ids.push(text(&d, "trash_id"));
    }
    let listed: Vec<String> = items(&vault).iter().map(|i| text(i, "trash_id")).collect();
    let mut newest_first = ids.clone();
    newest_first.reverse();
    assert_eq!(listed, newest_first);

    let limited = call(&vault, "vault_trash_list", json!({"limit": 2})).unwrap();
    assert_eq!(limited["items"].as_array().unwrap().len(), 2);
    assert_eq!(limited["truncated"], true);
    assert_eq!(
        call(&vault, "vault_trash_list", json!({"limit": 4})).unwrap()["truncated"],
        false
    );

    // data のない entry・読めない entry.json・知らない名前のフォルダは出さない
    let trash = trash_dir(&path);
    fs::remove_file(trash.join(&ids[0]).join("data")).unwrap();
    fs::write(trash.join(&ids[1]).join("entry.json"), b"{broken").unwrap();
    fs::create_dir(trash.join("not-an-id")).unwrap();
    let listed: Vec<String> = items(&vault).iter().map(|i| text(i, "trash_id")).collect();
    assert_eq!(listed, [ids[3].clone(), ids[2].clone()]);

    for bad in [json!({"limit": 0}), json!({"limit": 10001})] {
        let what = bad.to_string();
        expect_code(
            call(&vault, "vault_trash_list", bad),
            ErrorCode::InvalidArgument,
            &what,
        );
    }
}

/// 復元：trash_id の形と一覧との完全一致、偽の entry.json、data がリンク。
#[test]
fn restore_rejects_bad_ids_and_forged_entries() {
    let dir = TestDir::new("trash-forged");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();

    for bad in ["../x", "1-2-3/../../x", "..", "1-2-3\\..", "", "1-2"] {
        expect_code(
            call(&vault, "vault_restore", json!({"trash_id": bad})),
            ErrorCode::InvalidArgument,
            bad,
        );
    }
    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": "1-2-3"})),
        ErrorCode::NotFound,
        "ゴミ箱にない",
    );

    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "f"}),
    )
    .unwrap();
    let id = text(
        &call(&vault, "vault_delete", json!({"path": "f.txt"})).unwrap(),
        "trash_id",
    );
    let entry_file = trash_dir(&path).join(&id).join("entry.json");
    let original: Value = serde_json::from_slice(&fs::read(&entry_file).unwrap()).unwrap();
    let outside_abs = outside.join("o").display().to_string();
    for forged in [
        "../../o",
        "C:/Windows/x",
        "/etc/x",
        "a/../../b",
        outside_abs.as_str(),
        ".vault/x",
    ] {
        let mut entry = original.clone();
        entry["original_path"] = json!(forged);
        fs::write(&entry_file, serde_json::to_vec(&entry).unwrap()).unwrap();
        expect_code(
            call(&vault, "vault_restore", json!({"trash_id": id})),
            ErrorCode::InvalidTrashEntry,
            forged,
        );
    }
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "外に何も作らない"
    );
    assert!(!dir.join("o").exists() && !dir.path().parent().unwrap().join("o").exists());

    // entry.json がない・壊れている
    fs::write(&entry_file, b"{broken").unwrap();
    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": id})),
        ErrorCode::InvalidTrashEntry,
        "壊れた entry.json",
    );
    // もとに戻せば、戻せる
    fs::write(&entry_file, serde_json::to_vec(&original).unwrap()).unwrap();
    call(&vault, "vault_restore", json!({"trash_id": id})).unwrap();
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "f");

    // to も、決定3 の検証を通す
    let id = text(
        &call(&vault, "vault_delete", json!({"path": "f.txt"})).unwrap(),
        "trash_id",
    );
    for (to, code) in [
        ("../x", ErrorCode::InvalidPath),
        (".vault/x", ErrorCode::ReservedPath),
        ("", ErrorCode::InvalidPath),
    ] {
        expect_code(
            call(&vault, "vault_restore", json!({"trash_id": id, "to": to})),
            code,
            to,
        );
    }
}

/// data がジャンクションなら INVALID_TRASH_ENTRY（外のものを保管庫へ持ち込まない）。
#[cfg(windows)]
#[test]
fn restore_rejects_data_that_is_a_junction() {
    use common::junction;

    let dir = TestDir::new("trash-data-junction");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"s").unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "f"}),
    )
    .unwrap();
    let id = text(
        &call(&vault, "vault_delete", json!({"path": "f.txt"})).unwrap(),
        "trash_id",
    );
    let data = trash_dir(&path).join(&id).join("data");
    fs::remove_file(&data).unwrap();
    if !junction(&data, &outside) {
        return;
    }
    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": id})),
        ErrorCode::InvalidTrashEntry,
        "data がジャンクション",
    );
    assert!(!path.join("f.txt").exists());
    assert_eq!(fs::read_to_string(outside.join("secret.txt")).unwrap(), "s");
}

/// 戻す先がふさがっていれば CONFLICT。replace なら、今ある中身を先にゴミ箱へ移して入れ替える。
#[test]
fn restore_conflicts_and_replaces() {
    let dir = TestDir::new("trash-conflict");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "old"}),
    )
    .unwrap();
    let id = text(
        &call(&vault, "vault_delete", json!({"path": "a.txt"})).unwrap(),
        "trash_id",
    );
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "new"}),
    )
    .unwrap();

    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": id})),
        ErrorCode::Conflict,
        "ふさがっている",
    );
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "new");

    let r = call(
        &vault,
        "vault_restore",
        json!({"trash_id": id, "replace": true}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "old");
    let replaced = text(&r, "replaced_trash_id");
    let item = items(&vault)
        .into_iter()
        .find(|i| i["trash_id"] == replaced.as_str())
        .unwrap();
    assert_eq!(item["reason"], "restore_replace");
    assert_eq!(
        fs::read_to_string(trash_dir(&path).join(&replaced).join("data")).unwrap(),
        "new"
    );
    let tail: Vec<(Op, Option<String>)> = journal(&path)
        .into_iter()
        .rev()
        .take(2)
        .map(|e| (e.op, e.trash_id))
        .collect();
    assert_eq!(
        tail,
        [
            (Op::Restore, Some(id.clone())),
            (Op::Delete, Some(replaced.clone()))
        ]
    );
    // 入れ替えた中身も戻せる
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": replaced, "to": "a-new.txt"}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path.join("a-new.txt")).unwrap(), "new");
}

/// 戻す先の親のフォルダがなければ作り、履歴に mkdir を書く。to なら、中の深さと長さを確かめる。
#[test]
fn restore_creates_missing_parents_and_checks_to() {
    let dir = TestDir::new("tp");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "p/q/f.txt", "content": "f"}),
    )
    .unwrap();
    let id = text(
        &call(&vault, "vault_delete", json!({"path": "p/q/f.txt"})).unwrap(),
        "trash_id",
    );
    call(
        &vault,
        "vault_delete",
        json!({"path": "p", "recursive": true}),
    )
    .unwrap();
    assert!(!path.join("p").exists());
    call(&vault, "vault_restore", json!({"trash_id": id})).unwrap();
    assert_eq!(
        fs::read_to_string(path.join("p").join("q").join("f.txt")).unwrap(),
        "f"
    );
    let tail: Vec<(Op, Option<String>)> = journal(&path)
        .into_iter()
        .rev()
        .take(3)
        .map(|e| (e.op, e.path))
        .collect();
    assert_eq!(
        tail,
        [
            (Op::Restore, Some("p/q/f.txt".to_owned())),
            (Op::Mkdir, Some("p/q".to_owned())),
            (Op::Mkdir, Some("p".to_owned())),
        ]
    );

    // to で別の場所へ。中の深さが上限を超える場所へは戻さない
    let chain = |n: usize| vec!["d"; n].join("/");
    call(
        &vault,
        "vault_write",
        json!({"path": format!("t/{}/f", chain(30)), "content": "deep"}),
    )
    .unwrap();
    let id = text(
        &call(
            &vault,
            "vault_delete",
            json!({"path": "t", "recursive": true}),
        )
        .unwrap(),
        "trash_id",
    );
    // t の中は、t からの深さ 31。y/d×32/t へ戻すと 34 + 31 = 65 段で拒否、y/d×31/t ならぴったり 64 段。
    // 64 段のパスは、名前が1字でも 127 字あるので、テストの場所が長すぎるときは、ここから先を飛ばす
    let deepest_len = format!("y/{}/t/{}/f", chain(31), chain(30)).len();
    if common::root_len(&path) + 1 + deepest_len >= vault_core::MAX_FULL_PATH_UTF16 {
        common::skip(
            "restore_creates_missing_parents_and_checks_to",
            "テストの場所のパスが長く、64 段のパスが 260 字に収まらない",
        );
        return;
    }
    call(
        &vault,
        "vault_mkdir",
        json!({"path": format!("y/{}", chain(32))}),
    )
    .unwrap();
    let err = call(
        &vault,
        "vault_restore",
        json!({"trash_id": id, "to": format!("y/{}/t", chain(32))}),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidPath, "{err}");
    assert!(err.message().contains("深すぎる"), "{err}");
    assert!(
        trash_dir(&path).join(&id).join("data").is_dir(),
        "ゴミ箱に残る"
    );
    let to = format!("y/{}/t", chain(31));
    call(&vault, "vault_restore", json!({"trash_id": id, "to": to})).unwrap();
    assert_eq!(
        call(
            &vault,
            "vault_read",
            json!({"path": format!("{to}/{}/f", chain(30))})
        )
        .unwrap()["content"],
        "deep"
    );
}

/// open（書ける状態）で、.vault/tmp に残った一時ファイルをゴミ箱へ移す（消さない）。
#[test]
fn leftover_temporary_files_move_to_the_trash_on_open() {
    let dir = TestDir::new("trash-orphan");
    let (path, _vault) = new_vault(&dir, "v");
    let tmp = path.join(".vault").join("tmp");
    fs::write(tmp.join("9999-1.tmp"), b"half written").unwrap();

    // 読み取り専用では動かさない
    let ro = Vault::open(&path, true, &env()).unwrap();
    assert!(tmp.join("9999-1.tmp").exists());
    assert!(items(&ro).is_empty());

    let vault = Vault::open(&path, false, &env()).unwrap();
    assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0);
    let list = items(&vault);
    assert_eq!(list.len(), 1);
    let item = &list[0];
    assert_eq!(item["reason"], "orphan_tmp");
    assert_eq!(item["original_path"], "", "もとの場所はない");
    assert_eq!(item["size"], 12);
    assert_eq!(item["actor"]["kind"], "external");
    let id = text(item, "trash_id");
    // もとの場所がないので、to を指定して戻す
    let err = call(&vault, "vault_restore", json!({"trash_id": id})).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidPath, "{err}");
    assert!(err.message().contains("to を指定"), "{err}");
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": id, "to": "recovered.bin"}),
    )
    .unwrap();
    assert_eq!(
        fs::read(path.join("recovered.bin")).unwrap(),
        b"half written"
    );
    assert!(
        journal(&path)
            .iter()
            .all(|e| e.actor.kind() != ActorKind::External || e.op != Op::Delete)
    );
}

/// 不変条件 (d)：外を指すジャンクションを含むフォルダを、recursive で一覧・削除しても、
/// 外のファイルは一覧に出ず、消えもしない（削除はジャンクションそのものをゴミ箱へ移し、先はたどらない）。
#[cfg(windows)]
#[test]
fn folders_with_junctions_are_listed_and_deleted_without_following_them() {
    use common::junction;

    let dir = TestDir::new("trash-junction-inside");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside-place");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "f/own.txt", "content": "own"}),
    )
    .unwrap();
    if !junction(&path.join("f").join("j"), &outside) {
        return;
    }

    let all = call(&vault, "vault_list", json!({"recursive": true})).unwrap();
    let paths: Vec<String> = all["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| text(e, "path"))
        .collect();
    assert_eq!(paths, ["f", "f/j", "f/own.txt"]);
    assert!(!all.to_string().contains("secret"));

    let d = call(
        &vault,
        "vault_delete",
        json!({"path": "f", "recursive": true}),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "secret"
    );
    let moved_link = trash_dir(&path)
        .join(text(&d, "trash_id"))
        .join("data")
        .join("j");
    assert!(
        fs::symlink_metadata(&moved_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // 大きさの合計にも、外のファイルは入らない
    let item = items(&vault).pop().unwrap();
    assert_eq!(item["size"], 3);

    // 戻せる（中のジャンクションはそのまま）。外は変わらない
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": text(&d, "trash_id")}),
    )
    .unwrap();
    assert!(
        fs::symlink_metadata(path.join("f").join("j"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "secret"
    );
    // ジャンクションそのものは、ほかの操作と同じく拒否
    expect_code(
        call(&vault, "vault_delete", json!({"path": "f/j"})),
        ErrorCode::OutsideVault,
        "ジャンクションを直接消す",
    );
}

/// 外で作られた、上限（64段・260字）を超える木：vault_list は出さずに数を返し、祖先を消して戻せる。
#[test]
fn trees_beyond_the_limits_are_hidden_but_can_be_deleted_and_restored() {
    let dir = TestDir::new("trash-over");
    let (path, vault) = new_vault(&dir, "v");
    // 外から（\\?\ の付いた形で）、70段の木と、長い名前のファイルを作る
    let root = fs::canonicalize(&path).unwrap();
    let mut deep = root.join("deep");
    for _ in 0..70 {
        deep.push("a");
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("bottom.txt"), b"bottom").unwrap();
    let long_name = "n".repeat(200);
    fs::create_dir_all(root.join("long")).unwrap();
    fs::write(root.join("long").join(&long_name), b"long").unwrap();

    let all = call(&vault, "vault_list", json!({"recursive": true})).unwrap();
    let entries = all["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .all(|e| text(e, "path").split('/').count() <= 64)
    );
    assert!(entries.iter().any(|e| e["path"] == "deep"));
    assert!(entries.iter().any(|e| e["path"] == "long"));
    assert!(!entries.iter().any(|e| text(e, "path").contains(&long_name)));
    assert_eq!(
        all["skipped_over_limit"], 2,
        "深すぎる1つと、長すぎる1つ（その中は数えない）"
    );

    // 祖先を消せば、1回の rename でゴミ箱へ。戻すのも1回
    let d = call(
        &vault,
        "vault_delete",
        json!({"path": "deep", "recursive": true}),
    )
    .unwrap();
    assert!(!root.join("deep").exists());
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": text(&d, "trash_id")}),
    )
    .unwrap();
    assert_eq!(fs::read(deep.join("bottom.txt")).unwrap(), b"bottom");
    let d = call(
        &vault,
        "vault_delete",
        json!({"path": "long", "recursive": true}),
    )
    .unwrap();
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": text(&d, "trash_id")}),
    )
    .unwrap();
    assert_eq!(
        fs::read(root.join("long").join(&long_name)).unwrap(),
        b"long"
    );
    // 索引には、上限の中のものだけを入れる
    let idx = index(&path);
    assert!(
        idx.as_object()
            .unwrap()
            .keys()
            .all(|k| k.split('/').count() <= 64)
    );
    assert!(idx.get("deep").is_some());
}

/// Windows で大文字小文字を変えて消して戻しても、名前はもとのままで、索引のキーは1つだけ。
#[cfg(windows)]
#[test]
fn delete_and_restore_keep_the_real_name_and_one_index_key() {
    let dir = TestDir::new("trash-case");
    let (path, vault) = new_vault(&dir, "v");
    let names_on_disk = |path: &Path| -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".vault")
            .collect();
        names.sort();
        names
    };
    let keys_like = |name: &str| -> Vec<String> {
        index(&path)
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| k.eq_ignore_ascii_case(name))
            .cloned()
            .collect()
    };

    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let d = call(&vault, "vault_delete", json!({"path": "A.TXT"})).unwrap();
    assert_eq!(d["path"], "a.txt", "実際の名前で返す");
    let item = items(&vault).pop().unwrap();
    assert_eq!(item["original_path"], "a.txt", "entry.json にも実際の名前");
    assert_eq!(journal(&path).pop().unwrap().path.as_deref(), Some("a.txt"));
    assert!(keys_like("a.txt").is_empty());
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": text(&d, "trash_id")}),
    )
    .unwrap();
    assert_eq!(names_on_disk(&path), ["a.txt"], "名前はもとのまま");
    assert_eq!(keys_like("a.txt"), ["a.txt"]);

    // replace：今ある「C.TXT」を移して「c.txt」を戻す。移したものは実際の名前で記録し、索引のキーは1つ
    call(
        &vault,
        "vault_write",
        json!({"path": "c.txt", "content": "old"}),
    )
    .unwrap();
    let id = text(
        &call(&vault, "vault_delete", json!({"path": "c.txt"})).unwrap(),
        "trash_id",
    );
    call(
        &vault,
        "vault_write",
        json!({"path": "C.TXT", "content": "new"}),
    )
    .unwrap();
    let r = call(
        &vault,
        "vault_restore",
        json!({"trash_id": id, "replace": true}),
    )
    .unwrap();
    let replaced = text(&r, "replaced_trash_id");
    let entry = items(&vault)
        .into_iter()
        .find(|i| i["trash_id"] == replaced.as_str())
        .unwrap();
    assert_eq!(entry["original_path"], "C.TXT");
    assert_eq!(names_on_disk(&path), ["a.txt", "c.txt"]);
    assert_eq!(fs::read_to_string(path.join("c.txt")).unwrap(), "old");
    assert_eq!(keys_like("c.txt"), ["c.txt"]);
}

/// replace の途中で失敗したら（ゴミ箱の中身をほかのアプリが開いていて戻せない）、
/// 今あった中身はもとに戻り、ゴミ箱にも履歴にも余計なものが残らない。
#[cfg(windows)]
#[test]
fn a_failed_replace_puts_the_current_content_back() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = TestDir::new("trash-replace-busy");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "old"}),
    )
    .unwrap();
    let id = text(
        &call(&vault, "vault_delete", json!({"path": "a.txt"})).unwrap(),
        "trash_id",
    );
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "new"}),
    )
    .unwrap();
    let before = journal(&path).len();

    let data = trash_dir(&path).join(&id).join("data");
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&data)
        .unwrap();
    let result = call(
        &vault,
        "vault_restore",
        json!({"trash_id": id, "replace": true}),
    );
    drop(holder);
    expect_code(result, ErrorCode::Busy, "ゴミ箱の中身が使用中");
    assert_eq!(
        fs::read_to_string(path.join("a.txt")).unwrap(),
        "new",
        "今あった中身はもとに戻る"
    );
    let ids: Vec<String> = items(&vault).iter().map(|i| text(i, "trash_id")).collect();
    assert_eq!(
        ids,
        std::slice::from_ref(&id),
        "入れ替えようとした中身の entry は残らない"
    );
    assert_eq!(journal(&path).len(), before, "履歴にも残らない");
    assert_eq!(index(&path)["a.txt"]["kind"], "file");

    // 閉じたあとは入れ替えられる
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": id, "replace": true}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "old");
}

/// orphan_tmp でフォルダを移したときの size は、中のファイルの合計（vault_delete とそろえる）。
#[test]
fn leftover_temporary_folders_record_the_total_size() {
    let dir = TestDir::new("trash-orphan-dir");
    let (path, _vault) = new_vault(&dir, "v");
    let left = path.join(".vault").join("tmp").join("1-1.d");
    fs::create_dir_all(left.join("sub")).unwrap();
    fs::write(left.join("a"), b"12345").unwrap();
    fs::write(left.join("sub").join("b"), b"678").unwrap();
    let vault = Vault::open(&path, false, &env()).unwrap();
    let item = items(&vault).pop().unwrap();
    assert_eq!(item["reason"], "orphan_tmp");
    assert_eq!(item["kind"], "dir");
    assert_eq!(item["size"], 8);
}

/// 開いたあとで .vault/trash がジャンクションに差し替えられても、
/// ゴミ箱の一覧と復元は、その先を読まない（NOT_A_VAULT）。
#[cfg(windows)]
#[test]
fn a_trash_folder_swapped_for_a_junction_is_rejected() {
    use common::junction;

    let dir = TestDir::new("trash-swapped");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let id = text(
        &call(&vault, "vault_delete", json!({"path": "a.txt"})).unwrap(),
        "trash_id",
    );
    let outside = dir.join("outside-trash");
    fs::rename(trash_dir(&path), &outside).unwrap();
    if !junction(&trash_dir(&path), &outside) {
        return;
    }
    expect_code(
        call(&vault, "vault_trash_list", json!({})),
        ErrorCode::NotAVault,
        "ゴミ箱の一覧",
    );
    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": id})),
        ErrorCode::NotAVault,
        "復元",
    );
    assert!(!path.join("a.txt").exists());
    assert!(
        outside.join(&id).join("data").exists(),
        "外の中身は動かさない"
    );
}
