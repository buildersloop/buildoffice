//! 最終のパスの確かめ（「起きないはず」の分かれ道。F1・F2・F4・F5・F7・F7b・F9）。
//! 最終のパスを外の値に差し替えて（`test_support::fake_final_path`）、守りの層ごとに止まることを確かめる。

#![cfg(windows)]

mod common;

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::{TestDir, call, expect_code, journal, new_vault};
use serde_json::json;
use vault_core::test_support::{FinalAt, Step, fake_final_path, on_next};
use vault_core::{ErrorCode, Op};

/// 外の場所（どこかのボリュームの、保管庫の外のパス）。
fn elsewhere(_: PathBuf) -> PathBuf {
    PathBuf::from(r"\\?\C:\elsewhere\not-the-vault\x")
}

fn tmp_count(vault: &Path) -> usize {
    fs::read_dir(vault.join(".vault").join("tmp"))
        .map(|d| d.count())
        .unwrap_or(0)
}

/// F1：一時ファイルを作った直後の確かめで外と判定させる → NOT_A_VAULT。中身を書く前に止まる
/// （AfterTmpCreated の hook は呼ばれない）。.vault/tmp は空。
#[test]
fn f1_a_tmp_file_outside_stops_before_writing() {
    let dir = TestDir::new("f1");
    let (path, vault) = new_vault(&dir, "v");
    let called = Rc::new(Cell::new(false));
    let c = called.clone();
    on_next(Step::AfterTmpCreated, move || c.set(true));
    fake_final_path(FinalAt::Created, elsewhere);
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "x"}),
        ),
        ErrorCode::NotAVault,
        "一時ファイルが外",
    );
    assert!(!called.get(), "中身を書く前に止まっていない");
    assert_eq!(tmp_count(&path), 0);
    assert!(!path.join("f.txt").exists());
}

/// F2：vault_mkdir で作ったフォルダを外と判定させる → OUTSIDE_VAULT。作った空のフォルダは作ったハンドルで消え、
/// 履歴に mkdir がない。
#[test]
fn f2_a_created_folder_outside_is_removed() {
    let dir = TestDir::new("f2");
    let (path, vault) = new_vault(&dir, "v");
    let before = journal(&path).len();
    fake_final_path(FinalAt::Created, elsewhere);
    expect_code(
        call(&vault, "vault_mkdir", json!({"path": "m"})),
        ErrorCode::OutsideVault,
        "作ったフォルダが外",
    );
    assert!(!path.join("m").exists(), "作ったフォルダが残った");
    assert_eq!(journal(&path).len(), before);
}

/// F4：操作の根の最終のパスを外の値にする → NOT_A_VAULT。
#[test]
fn f4_a_root_outside_is_not_a_vault() {
    let dir = TestDir::new("f4");
    let (_path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "x"}),
    )
    .unwrap();
    fake_final_path(FinalAt::Root, elsewhere);
    expect_code(
        call(&vault, "vault_read", json!({"path": "f.txt"})),
        ErrorCode::NotAVault,
        "根が外",
    );
}

/// F5：vault_read の対象の最終のパスを外の値にする → OUTSIDE_VAULT。中身を返さない。
#[test]
fn f5_a_read_target_outside_is_not_read() {
    let dir = TestDir::new("f5");
    let (_path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "secret"}),
    )
    .unwrap();
    fake_final_path(FinalAt::Target, elsewhere);
    let result = call(&vault, "vault_read", json!({"path": "f.txt"}));
    if let Ok(v) = &result {
        assert!(!v.to_string().contains("secret"));
    }
    expect_code(result, ErrorCode::OutsideVault, "読む対象が外");
}

/// F7：vault_write の最後の rename のあとの確かめで外と判定させる → 一時ファイルを tmp へ戻して作ったハンドルで
/// 消す。OUTSIDE_VAULT。上書きなら、古い中身が元の場所に戻る。
#[test]
fn f7_a_renamed_file_outside_is_moved_back() {
    let dir = TestDir::new("f7");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    fake_final_path(FinalAt::Renamed, elsewhere);
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "new"}),
        ),
        ErrorCode::OutsideVault,
        "上書きの rename のあとが外",
    );
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "old");
    assert_eq!(tmp_count(&path), 0);

    fake_final_path(FinalAt::Renamed, elsewhere);
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "g.txt", "content": "new"}),
        ),
        ErrorCode::OutsideVault,
        "新しく作る rename のあとが外",
    );
    assert!(!path.join("g.txt").exists());
    assert_eq!(tmp_count(&path), 0);
}

/// F7b：F7 を create_dirs（"new/f.txt"）でも。
#[test]
fn f7b_a_renamed_file_outside_with_created_parents() {
    let dir = TestDir::new("f7b");
    let (path, vault) = new_vault(&dir, "v");
    fake_final_path(FinalAt::Renamed, elsewhere);
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "new/f.txt", "content": "x"}),
        ),
        ErrorCode::OutsideVault,
        "作った親の下の rename のあとが外",
    );
    assert!(!path.join("new").join("f.txt").exists());
    assert_eq!(tmp_count(&path), 0);
}

/// F9：vault_mkdir "p/q" で、作った p を開き直したときの確かめで外と判定させる → OUTSIDE_VAULT。
/// p は消えない。履歴には mkdir p だけ。q はできない。
#[test]
fn f9_a_reopened_parent_outside_stops_without_removing() {
    let dir = TestDir::new("f9");
    let (path, vault) = new_vault(&dir, "v");
    let before = journal(&path).len();
    fake_final_path(FinalAt::Reopened, elsewhere);
    expect_code(
        call(&vault, "vault_mkdir", json!({"path": "p/q"})),
        ErrorCode::OutsideVault,
        "開き直した親が外",
    );
    assert!(path.join("p").is_dir(), "作った p を消した");
    assert!(!path.join("p").join("q").exists());
    let added: Vec<_> = journal(&path).into_iter().skip(before).collect();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].op, Op::Mkdir);
    assert_eq!(added[0].path.as_deref(), Some("p"));
}
