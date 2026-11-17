//! 根の ID（RID1・RID2）。
//!
//! 開いたまま、保管庫のフォルダが同じ名前の別のフォルダに差し替えられたら、最終のパスは同じでも ID が違うので
//! NOT_A_VAULT にする。同じフォルダに戻せば、また使える。

#![cfg(windows)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{TestDir, call, env, expect_code, junction, new_vault};
use serde_json::json;
use vault_core::{ErrorCode, Vault};

/// フォルダの中身（相対パスと、ファイルなら中身）。
type Snapshot = Vec<(PathBuf, Option<Vec<u8>>)>;

/// フォルダの中のすべてのファイル（相対パスと中身。フォルダは中身なしで入れる）。
fn snapshot(root: &Path) -> Snapshot {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                out.push((rel, None));
                stack.push(path);
            } else {
                out.push((rel, Some(fs::read(&path).unwrap())));
            }
        }
    }
    out.sort();
    out
}

/// 保管庫 v を開いたまま、v を v-old に、別の保管庫 w を v に名前を変える（v と同じ名前の別のフォルダにする）。
/// 返り値は (v の場所, v-old の場所, 差し替える前の w の中身)。
fn swap_in_another_vault(dir: &TestDir, path: &Path) -> (PathBuf, Snapshot) {
    let (other, other_vault) = new_vault(dir, "w");
    call(
        &other_vault,
        "vault_write",
        json!({"path": "f.txt", "content": "w"}),
    )
    .unwrap();
    drop(other_vault);
    let before = snapshot(&other);
    let old = dir.join("v-old");
    fs::rename(path, &old).unwrap();
    fs::rename(&other, path).unwrap();
    (old, before)
}

/// RID1：開いたまま、保管庫のフォルダ v を v-old に名前を変え、別の保管庫 w を v に名前を変える（同じ名前の別の
/// フォルダに差し替える）→ vault_info を除くツールがすべて NOT_A_VAULT（read・write・list（根・ある名前・ない名前）・
/// changes・delete・move・mkdir・trash_list・restore）。w は変わらない（以前は、
/// w を保管庫として使っていた）。
#[test]
fn rid1_a_root_replaced_by_another_folder_with_the_same_name_is_not_a_vault() {
    let dir = TestDir::new("rid1");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "v"}),
    )
    .unwrap();
    // 戻すもの（v のゴミ箱の中身）を作っておく
    call(
        &vault,
        "vault_write",
        json!({"path": "t.txt", "content": "t"}),
    )
    .unwrap();
    let deleted = call(&vault, "vault_delete", json!({"path": "t.txt"})).unwrap();
    let trash_id = deleted["trash_id"].as_str().unwrap().to_owned();
    let (_, before) = swap_in_another_vault(&dir, &path);
    for (tool, args) in [
        ("vault_read", json!({"path": "f.txt"})),
        ("vault_write", json!({"path": "g.txt", "content": "x"})),
        ("vault_list", json!({})),
        // ない名前（w にも v にもない）と、ある名前（w に f.txt がある）。名前の有無・種類がエラーのコードで
        // 分からない（そのあとも、list だけ根を開く前にパスで門を通していたので、NOT_FOUND・NOT_A_DIRECTORY に
        // なっていた）
        ("vault_list", json!({"path": "no-such-folder"})),
        ("vault_list", json!({"path": "f.txt"})),
        ("vault_changes", json!({})),
        ("vault_delete", json!({"path": "f.txt"})),
        ("vault_move", json!({"from": "f.txt", "to": "moved.txt"})),
        ("vault_mkdir", json!({"path": "new-folder"})),
        ("vault_trash_list", json!({})),
        ("vault_restore", json!({"trash_id": trash_id})),
    ] {
        expect_code(
            call(&vault, tool, args),
            ErrorCode::NotAVault,
            &format!("差し替えた保管庫に {tool}"),
        );
    }
    assert_eq!(snapshot(&path), before, "差し替えたフォルダ（w）が変わった");
}

/// RID2：差し替えたあと、同じフォルダ（v）を元の名前に戻すと、また使える。ジャンクションの道から開いた保管庫も
/// 使える（根は、リンクをたどった先のフォルダの最終のパスと ID）。
#[test]
fn rid2_the_same_folder_is_usable_again_and_through_a_junction() {
    let dir = TestDir::new("rid2");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "v"}),
    )
    .unwrap();
    let (old, _) = swap_in_another_vault(&dir, &path);
    expect_code(
        call(&vault, "vault_read", json!({"path": "f.txt"})),
        ErrorCode::NotAVault,
        "差し替えた保管庫を読む",
    );
    // w をよけて、v を元の名前に戻す
    fs::rename(&path, dir.join("w-again")).unwrap();
    fs::rename(&old, &path).unwrap();
    let r = call(&vault, "vault_read", json!({"path": "f.txt"})).unwrap();
    assert_eq!(r["content"], "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "g.txt", "content": "back"}),
    )
    .unwrap();

    // ジャンクションの道から開く
    let link = dir.join("link-to-v");
    assert!(junction(&link, &path), "ジャンクションを作れない");
    let via = Vault::open(&link, false, &env()).unwrap();
    call(
        &via,
        "vault_write",
        json!({"path": "h.txt", "content": "via"}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path.join("h.txt")).unwrap(), "via");
    let r = call(&via, "vault_read", json!({"path": "g.txt"})).unwrap();
    assert_eq!(r["content"], "back");
    let listed = call(&via, "vault_list", json!({})).unwrap();
    assert!(listed["entries"].as_array().unwrap().len() >= 3);
}
