//! vault_write の最後は置き換えない renameと、2つめの門で決める値。
//! T12・T12b-1〜3・T23・T23b。

#![cfg(windows)]

mod common;

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use common::{TestDir, call, expect_code, journal, new_vault, set_read_only};
use serde_json::json;
use vault_core::test_support::{Rename, Step, on_next};
use vault_core::{ErrorCode, Op};

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

fn trash_ids(vault: &Path) -> Vec<String> {
    fs::read_dir(vault.join(".vault").join("trash"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn index_entry(vault: &Path, key: &str) -> Option<serde_json::Value> {
    let text = fs::read_to_string(vault.join(".vault").join("index.json")).ok()?;
    let index: serde_json::Value = serde_json::from_str(&text).ok()?;
    index.get(key).cloned()
}

/// T12：上書きの最後の rename の直前に、書く先にフォルダを作る → CONFLICT。古い中身はゴミ箱にあり、
/// エラーの文と履歴（write_failed）に previous_trash_id がある。
#[test]
fn t12_a_folder_created_at_the_target_is_a_conflict() {
    let dir = TestDir::new("t12");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    let target = path.join("f.txt");
    on_next(Step::BeforeRename(Rename::WriteFinal), move || {
        fs::create_dir(&target).unwrap();
    });
    let err = call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "new"}),
    )
    .unwrap_err();
    let ids = trash_ids(&path);
    assert_eq!(ids.len(), 1, "古い中身はゴミ箱にある");
    assert!(
        err.message().contains(&ids[0]),
        "エラーの文に previous_trash_id：{err}"
    );
    let last = journal(&path).pop().unwrap();
    assert_eq!(last.op, Op::WriteFailed);
    assert_eq!(last.trash_id.as_deref(), Some(ids[0].as_str()));
    expect_code(Err::<(), _>(err), ErrorCode::Conflict, "書く先にフォルダ");
    assert!(path.join("f.txt").is_dir());
}

/// T12b-1：新しく作る書き込みの最後の rename の直前に、書く先にファイル X を作り、あわせて .vault/tmp の名前を
/// 変えようとする → 名前の変更は Err、CONFLICT。X は変わらず、一時ファイルは作ったハンドルで消え、履歴は増えない。
#[test]
fn t12b1_a_file_created_at_a_new_target_is_a_conflict() {
    let dir = TestDir::new("t12b1");
    let (path, vault) = new_vault(&dir, "v");
    let before = journal(&path).len();
    let renamed = Rc::new(RefCell::new(None));
    let out = renamed.clone();
    let target = path.join("x.txt");
    let tmp = path.join(".vault").join("tmp");
    on_next(Step::BeforeRename(Rename::WriteFinal), move || {
        fs::write(&target, "OTHER").unwrap();
        *out.borrow_mut() = Some(fs::rename(&tmp, tmp.with_file_name("tmp2")).is_ok());
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "x.txt", "content": "new"}),
        ),
        ErrorCode::Conflict,
        "書く先にほかのアプリのファイル",
    );
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        ".vault/tmp の名前を変えられた"
    );
    assert_eq!(fs::read_to_string(path.join("x.txt")).unwrap(), "OTHER");
    assert_eq!(
        count(&path.join(".vault").join("tmp")),
        0,
        "一時ファイルが残った"
    );
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// T12b-2：上書きの最後の rename の直前に、書く先にファイル X を作る → CONFLICT。X は変わらない。
/// 古い中身はゴミ箱に残る（previous_trash_id と write_failed）。索引は「新しい版がある」と書き換わらず、
/// エラーの文の previous_trash_id は相対の id だけ。
#[test]
fn t12b2_a_file_created_at_an_overwritten_target_is_a_conflict() {
    let dir = TestDir::new("t12b2");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    let target = path.join("f.txt");
    on_next(Step::BeforeRename(Rename::WriteFinal), move || {
        fs::write(&target, "OTHER").unwrap();
    });
    let new_content = "the new content";
    let err = call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": new_content}),
    )
    .unwrap_err();
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "OTHER");
    let ids = trash_ids(&path);
    assert_eq!(ids.len(), 1);
    let data = path.join(".vault").join("trash").join(&ids[0]).join("data");
    assert_eq!(fs::read_to_string(data).unwrap(), "old");
    assert!(
        err.message()
            .contains(&format!("previous_trash_id: {}", ids[0])),
        "{err}"
    );
    let last = journal(&path).pop().unwrap();
    assert_eq!(last.op, Op::WriteFailed);
    assert_eq!(last.trash_id.as_deref(), Some(ids[0].as_str()));
    if let Some(entry) = index_entry(&path, "f.txt") {
        assert_ne!(
            entry["size"].as_u64(),
            Some(new_content.len() as u64),
            "索引が新しい版になった"
        );
    }
    expect_code(
        Err::<(), _>(err),
        ErrorCode::Conflict,
        "上書きの書く先にほかのアプリのファイル",
    );
}

/// T12b-3：上書きの最後の rename の直前に X を作り、取り戻す（take_back）直前に X を消す → CONFLICT。
/// 古い中身は元の場所に戻り、ゴミ箱の entry は残らず、履歴は増えない。
#[test]
fn t12b3_the_old_content_is_taken_back_when_the_target_is_free_again() {
    let dir = TestDir::new("t12b3");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    let before = journal(&path).len();
    let target = path.join("f.txt");
    let again = target.clone();
    on_next(Step::BeforeRename(Rename::WriteFinal), move || {
        fs::write(&target, "OTHER").unwrap();
    });
    on_next(Step::BeforeRename(Rename::TakeBack), move || {
        fs::remove_file(&again).unwrap();
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "new"}),
        ),
        ErrorCode::Conflict,
        "ぶつかったあと、取り戻せた",
    );
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "old");
    assert!(trash_ids(&path).is_empty(), "ゴミ箱に残った");
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// T23：上書きの門のあとで、書く先を別のもの（読み取り専用の印のファイル）に差し替える → CONFLICT。
/// 差し替えたファイルはゴミ箱へ移らず、変わらない。履歴は増えない。
#[test]
fn t23_a_target_swapped_after_the_gate_is_a_conflict() {
    let dir = TestDir::new("t23");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    let before = journal(&path).len();
    let target = path.join("f.txt");
    let swapped = target.clone();
    on_next(Step::AfterGate, move || {
        fs::remove_file(&swapped).unwrap();
        fs::write(&swapped, "swapped").unwrap();
        set_read_only(&swapped, true);
    });
    let result = call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "new"}),
    );
    let still_read_only = fs::metadata(&target).unwrap().permissions().readonly();
    set_read_only(&target, false);
    expect_code(result, ErrorCode::Conflict, "門のあとで差し替えた");
    assert!(still_read_only);
    assert_eq!(fs::read_to_string(&target).unwrap(), "swapped");
    assert!(trash_ids(&path).is_empty(), "ゴミ箱へ移った");
    assert_eq!(journal(&path).len(), before);
}

/// T23b：上書きの門のあとで、書く先の更新時刻だけを変える → CONFLICT。ゴミ箱へ移らない。
#[test]
fn t23b_a_changed_time_after_the_gate_is_a_conflict() {
    let dir = TestDir::new("t23b");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    let target = path.join("f.txt");
    let touched = target.clone();
    on_next(Step::AfterGate, move || {
        let file = fs::OpenOptions::new().write(true).open(&touched).unwrap();
        let t = SystemTime::now() - Duration::from_secs(3600);
        file.set_modified(t).unwrap();
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "new"}),
        ),
        ErrorCode::Conflict,
        "時刻だけが変わった",
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    assert!(trash_ids(&path).is_empty());
}
