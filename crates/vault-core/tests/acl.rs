//! 書き込みか削除を拒否する設定（ACL）が付いたファイルを、上書き・削除・移動・置き換えしない（A1〜A8）。
//!
//! - ACL は Everyone への拒否の (WD)（ファイルにデータを書けない）を icacls で付ける（(W) は SYNCHRONIZE も拒否する
//!   ので使わない）。Drop で外す。
//! - 対象はファイルだけ（設計の決定）。フォルダは今までどおりゴミ箱へ移せる（A8）。

#![cfg(windows)]

mod common;

use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::rc::Rc;

use common::{DenyAce, TestDir, call, expect_code, journal, new_vault, set_read_only};
use serde_json::json;
use vault_core::test_support::{Step, on_next};
use vault_core::{ErrorCode, Vault};

/// 保管庫の様子（ファイルの中身・ゴミ箱の数・履歴の行の数・.vault/tmp の中の数）。変わらないことを確かめる。
#[derive(Debug, PartialEq)]
struct State {
    files: Vec<(String, String)>,
    trash: usize,
    journal: usize,
    tmp: usize,
}

fn state(vault: &Vault, path: &Path) -> State {
    let mut files: Vec<(String, String)> = fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    files.sort();
    let trash = call(vault, "vault_trash_list", json!({})).unwrap()["items"]
        .as_array()
        .unwrap()
        .len();
    let tmp = fs::read_dir(path.join(".vault").join("tmp"))
        .map(|d| d.count())
        .unwrap_or(0);
    State {
        files,
        trash,
        journal: journal(path).len(),
        tmp,
    }
}

fn write(vault: &Vault, rel: &str, content: &str) {
    call(
        vault,
        "vault_write",
        json!({"path": rel, "content": content}),
    )
    .unwrap();
}

/// ACL で断ったときの ACCESS_DENIED（文は ACL のもの）。
fn expect_acl_denied(result: Result<serde_json::Value, vault_core::VaultError>, what: &str) {
    match &result {
        Err(e) => assert!(
            e.message().contains("ACL"),
            "{what}：ACL の文でない：{}",
            e.message()
        ),
        Ok(v) => panic!("{what}：ACCESS_DENIED になるべきところが成功した（{v}）"),
    }
    expect_code(result, ErrorCode::AccessDenied, what);
}

/// A1（赤）：(WD) のファイルを vault_write で上書き → ACCESS_DENIED。ファイル・ゴミ箱・履歴・.vault/tmp が変わらない。
#[test]
fn a1_overwriting_a_write_denied_file_is_access_denied() {
    let dir = TestDir::new("a1");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "orig");
    let before = state(&vault, &path);
    let _deny = DenyAce::new(&path.join("f.txt"), "(WD)");
    expect_acl_denied(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "new"}),
        ),
        "(WD) のファイルを上書き",
    );
    assert_eq!(state(&vault, &path), before, "何かが変わった");
}

/// A2（赤）：(WD) のファイルを vault_delete → ACCESS_DENIED。ファイルとゴミ箱が変わらない。
#[test]
fn a2_deleting_a_write_denied_file_is_access_denied() {
    let dir = TestDir::new("a2");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "orig");
    let before = state(&vault, &path);
    let _deny = DenyAce::new(&path.join("f.txt"), "(WD)");
    expect_acl_denied(
        call(&vault, "vault_delete", json!({"path": "f.txt"})),
        "(WD) のファイルを消す",
    );
    assert_eq!(state(&vault, &path), before, "何かが変わった");
}

/// A3（赤）：(WD) のファイルを vault_move → ACCESS_DENIED。移す先はできない。
#[test]
fn a3_moving_a_write_denied_file_is_access_denied() {
    let dir = TestDir::new("a3");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "orig");
    let before = state(&vault, &path);
    let _deny = DenyAce::new(&path.join("f.txt"), "(WD)");
    expect_acl_denied(
        call(
            &vault,
            "vault_move",
            json!({"from": "f.txt", "to": "g.txt"}),
        ),
        "(WD) のファイルを移す",
    );
    assert_eq!(state(&vault, &path), before, "何かが変わった");
    assert!(!path.join("g.txt").exists());
}

/// A4（赤）：vault_restore の replace:true で、戻す先に (WD) のファイルがある → ACCESS_DENIED。entry は残り、
/// 置き換えたもの（replaced）の項目はできない。戻す先のファイルは変わらない。
#[test]
fn a4_restoring_over_a_write_denied_file_is_access_denied() {
    let dir = TestDir::new("a4");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "v1");
    let overwritten = call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "v2"}),
    )
    .unwrap();
    let trash_id = overwritten["previous_trash_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = state(&vault, &path);
    let _deny = DenyAce::new(&path.join("f.txt"), "(WD)");
    expect_acl_denied(
        call(
            &vault,
            "vault_restore",
            json!({"trash_id": trash_id, "replace": true}),
        ),
        "(WD) のファイルを置き換えて戻す",
    );
    assert_eq!(state(&vault, &path), before, "何かが変わった");
    let items = call(&vault, "vault_trash_list", json!({})).unwrap();
    assert!(
        items["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["trash_id"] == trash_id),
        "entry が残っていない"
    );
}

/// A5（緑）：(WD) のファイルも、read・list はできる。
#[test]
fn a5_a_write_denied_file_can_still_be_read_and_listed() {
    let dir = TestDir::new("a5");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "orig");
    let _deny = DenyAce::new(&path.join("f.txt"), "(WD)");
    let r = call(&vault, "vault_read", json!({"path": "f.txt"})).unwrap();
    assert_eq!(r["content"], "orig");
    let l = call(&vault, "vault_list", json!({})).unwrap();
    assert!(
        l["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "f.txt")
    );
}

/// A6（赤）：ほかのアプリが READ・DELETE の共有で（WRITE を共有せずに）開いているファイルを vault_delete → BUSY
/// （3回試してから）。閉じたあとは消せる（設計の決定で受け入れる副作用）。
#[test]
fn a6_a_file_opened_without_write_sharing_is_busy() {
    let dir = TestDir::new("a6");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "orig");
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_DELETE: u32 = 0x4;
    let held = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(path.join("f.txt"))
        .unwrap();
    expect_code(
        call(&vault, "vault_delete", json!({"path": "f.txt"})),
        ErrorCode::Busy,
        "WRITE を共有せずに開かれたファイルを消す",
    );
    assert!(path.join("f.txt").exists());
    drop(held);
    call(&vault, "vault_delete", json!({"path": "f.txt"})).unwrap();
    assert!(!path.join("f.txt").exists());
}

/// A7b（赤）：上書きの1つめの門のあと（AfterGate）で、書く先に (WD) を付ける → ACCESS_DENIED（2つめの門で断る）。
/// ファイル・ゴミ箱・履歴・.vault/tmp が変わらない。
#[test]
fn a7b_a_write_deny_added_after_the_gate_is_access_denied() {
    let dir = TestDir::new("a7b");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "orig");
    let before = state(&vault, &path);
    let deny: Rc<RefCell<Option<DenyAce>>> = Rc::new(RefCell::new(None));
    let slot = deny.clone();
    let file = path.join("f.txt");
    on_next(Step::AfterGate, move || {
        *slot.borrow_mut() = Some(DenyAce::new(&file, "(WD)"));
    });
    expect_acl_denied(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "new"}),
        ),
        "門のあとで (WD) を付けたファイルを上書き",
    );
    assert!(deny.borrow().is_some(), "差し込みが呼ばれていない");
    assert_eq!(state(&vault, &path), before, "何かが変わった");
}

/// A7c（(a)）：上書きの1つめの門のあと（AfterGate）で、書く先に読み取り専用の印だけを付ける
/// → CONFLICT（門のときと印が違う＝門のあとで変わった。ACL の文にしない）。ファイル・ゴミ箱・履歴・.vault/tmp が
/// 変わらない。
#[test]
fn a7c_a_read_only_mark_added_after_the_gate_is_conflict() {
    let dir = TestDir::new("a7c");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "orig");
    let before = state(&vault, &path);
    let file = path.join("f.txt");
    let marked = Rc::new(RefCell::new(false));
    let slot = marked.clone();
    let target = file.clone();
    on_next(Step::AfterGate, move || {
        set_read_only(&target, true);
        *slot.borrow_mut() = true;
    });
    let result = call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "new"}),
    );
    assert!(*marked.borrow(), "差し込みが呼ばれていない");
    let after = state(&vault, &path);
    set_read_only(&file, false);
    if let Err(e) = &result {
        assert!(!e.message().contains("ACL"), "ACL の文になった");
    }
    expect_code(
        result,
        ErrorCode::Conflict,
        "門のあとで読み取り専用の印を付けた",
    );
    assert_eq!(after, before, "何かが変わった");
}

/// A7d（(b)）：削除の1つめの門のあと（AfterGate）で、大きさの違う中身に書き換えてから (WD) を
/// 付ける → CONFLICT（門のあとで変わったことを、拒否より先に見る）。ファイルは書き換えた中身のまま、ゴミ箱・履歴・
/// .vault/tmp が変わらない。
#[test]
fn a7d_a_changed_and_write_denied_file_after_the_gate_is_conflict() {
    let dir = TestDir::new("a7d");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "orig");
    let mut expected = state(&vault, &path);
    expected.files = vec![("f.txt".to_owned(), "changed after the gate".to_owned())];
    let deny: Rc<RefCell<Option<DenyAce>>> = Rc::new(RefCell::new(None));
    let slot = deny.clone();
    let file = path.join("f.txt");
    on_next(Step::AfterGate, move || {
        fs::write(&file, "changed after the gate").unwrap();
        *slot.borrow_mut() = Some(DenyAce::new(&file, "(WD)"));
    });
    let result = call(&vault, "vault_delete", json!({"path": "f.txt"}));
    assert!(deny.borrow().is_some(), "差し込みが呼ばれていない");
    if let Err(e) = &result {
        assert!(!e.message().contains("ACL"), "ACL の文になった");
    }
    expect_code(
        result,
        ErrorCode::Conflict,
        "門のあとで書き換えて (WD) を付けた",
    );
    assert_eq!(state(&vault, &path), expected, "何かが変わった");
}

/// A8（緑）：(AD,WD) を付けたフォルダ（中にファイルを作れない）を vault_delete recursive → ゴミ箱へ移る（対象は
/// ファイルだけ。設計の決定）。
#[test]
fn a8_a_write_denied_folder_can_still_be_deleted() {
    let dir = TestDir::new("a8");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "d/x.txt", "x");
    let deny = DenyAce::new(&path.join("d"), "(AD,WD)");
    let deleted = call(
        &vault,
        "vault_delete",
        json!({"path": "d", "recursive": true}),
    )
    .unwrap();
    assert!(!path.join("d").exists());
    // ゴミ箱へ移ったフォルダの拒否を外す（片づけのため）
    let data = path
        .join(".vault")
        .join("trash")
        .join(deleted["trash_id"].as_str().unwrap())
        .join("data");
    let _ = std::process::Command::new("icacls")
        .arg(&data)
        .arg("/remove:d")
        .arg("*S-1-1-0")
        .output();
    drop(deny);
}
