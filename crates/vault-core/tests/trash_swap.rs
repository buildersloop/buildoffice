//! 移す・消す・戻す道で、確かめてから使うまでの隙（TOCTOU）に差し替えを起こすテスト（T9a〜c・T10・T11a・T11b・T13・T14・T14b・T15・T15c・T17（移す部分）・
//! T18・T27・T28・T29・F3・F3b・F8・F10・V3、ゴミ箱の一覧で使用中の `<id>` を飛ばす、読む道の使用中）。
//!
//! 差し替えは `test_support::on_next` で、決まった所（Step）で1回だけ起こす。保管庫と「外」のフォルダは、
//! どれも `CARGO_TARGET_TMPDIR` の下（`TestDir`）。

#![cfg(windows)]

mod common;

use std::cell::RefCell;
use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use common::{TestDir, call, env, expect_code, journal, junction, new_vault, set_read_only};
use serde_json::{Value, json};
use vault_core::test_support::{FinalAt, Place, Rename, Step, fake_final_path, on_next};
use vault_core::{ErrorCode, Vault};

/// `path` を `<path>-moved` へ移し、`path` に `target` を指すジャンクションを置く。
fn swap_for_junction(path: &Path, target: &Path) {
    let moved = moved_path(path);
    fs::rename(path, &moved).unwrap_or_else(|e| panic!("差し替えのための rename：{e}"));
    assert!(junction(path, target), "ジャンクションを作れない");
}

fn moved_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push("-moved");
    path.with_file_name(name)
}

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

fn trash_dir(vault: &Path) -> PathBuf {
    vault.join(".vault").join("trash")
}

/// ゴミ箱の直下の名前（並べたもの）。
fn trash_ids(vault: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(trash_dir(vault))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// hook の中の結果を、あとで確かめるための入れ物。
fn slot<T: 'static>() -> Rc<RefCell<Option<T>>> {
    Rc::new(RefCell::new(None))
}

/// hook の中で、`from` を `to` へ移そうとした結果（Ok なら true）を入れる。
fn try_rename(from: PathBuf, to: PathBuf, out: Rc<RefCell<Option<bool>>>) -> impl FnOnce() {
    move || {
        let ok = fs::rename(&from, &to).is_ok();
        *out.borrow_mut() = Some(ok);
    }
}

fn text(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key}: {v}"))
        .to_owned()
}

fn write(vault: &Vault, path: &str, content: &str) {
    call(
        vault,
        "vault_write",
        json!({"path": path, "content": content}),
    )
    .unwrap();
}

/// 消して、trash_id を返す。
fn delete(vault: &Vault, path: &str) -> String {
    text(
        &call(
            vault,
            "vault_delete",
            json!({"path": path, "recursive": true}),
        )
        .unwrap(),
        "trash_id",
    )
}

fn list_ids(vault: &Vault) -> Vec<String> {
    call(vault, "vault_trash_list", json!({})).unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| text(i, "trash_id"))
        .collect()
}

/// ファイルを、同じ大きさの読み取り専用の印のファイルに差し替える（中身は `content`）。
fn swap_for_read_only(file: &Path, content: &str) {
    fs::remove_file(file).unwrap();
    fs::write(file, content).unwrap();
    set_read_only(file, true);
}

/// 更新時刻だけを1時間前にする。
fn touch_back(file: &Path) {
    let f = fs::OpenOptions::new().write(true).open(file).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
}

/// 3段のフォルダ（各段にファイル）を作る：`top/f1`・`top/b/f2`・`top/b/c/f3`。
fn three_levels(vault: &Vault, top: &str) {
    write(vault, &format!("{top}/f1"), "1");
    write(vault, &format!("{top}/b/f2"), "22");
    write(vault, &format!("{top}/b/c/f3"), "333");
}

/// 外のフォルダ（中に `keep.txt`）。
fn outside_with_keep(dir: &TestDir) -> PathBuf {
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "OUTSIDE").unwrap();
    outside
}

fn assert_outside_kept(outside: &Path) {
    assert_eq!(count(outside), 1, "外に新しい項目ができた・消えた");
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "OUTSIDE"
    );
}

// ---------------------------------------------------------------------------
// vault_move
// ---------------------------------------------------------------------------

/// T9a：移す元の親を、門のあとで外へのジャンクションに替える → OUTSIDE_VAULT。外の同じ名前のものは移らない。
#[test]
fn t9a_move_does_not_follow_a_swapped_source_parent() {
    let dir = TestDir::new("t9a");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a/keep.txt", "inside");
    let outside = outside_with_keep(&dir);
    let before = journal(&path).len();
    let a = path.join("a");
    let target = outside.clone();
    on_next(Step::AfterGate, move || swap_for_junction(&a, &target));
    expect_code(
        call(
            &vault,
            "vault_move",
            json!({"from": "a/keep.txt", "to": "g.txt"}),
        ),
        ErrorCode::OutsideVault,
        "移す元の親がジャンクション",
    );
    assert_outside_kept(&outside);
    assert!(!path.join("g.txt").exists(), "外のものが中へ移った");
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// T9b：移す先の親を、門のあとで外へのジャンクションに替える → OUTSIDE_VAULT。移すものは外へ出ない。
#[test]
fn t9b_move_does_not_follow_a_swapped_destination_parent() {
    let dir = TestDir::new("t9b");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "inside");
    call(&vault, "vault_mkdir", json!({"path": "d"})).unwrap();
    let outside = outside_with_keep(&dir);
    let before = journal(&path).len();
    let d = path.join("d");
    let target = outside.clone();
    on_next(Step::AfterGate, move || swap_for_junction(&d, &target));
    expect_code(
        call(
            &vault,
            "vault_move",
            json!({"from": "f.txt", "to": "d/f.txt"}),
        ),
        ErrorCode::OutsideVault,
        "移す先の親がジャンクション",
    );
    assert_outside_kept(&outside);
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "inside");
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// T9c：rename の直前に、移す先にファイルを作る → ALREADY_EXISTS。どちらも変わらない。
#[test]
fn t9c_a_file_created_at_the_destination_is_not_replaced() {
    let dir = TestDir::new("t9c");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "from");
    let before = journal(&path).len();
    let to = path.join("g.txt");
    on_next(Step::BeforeRename(Rename::Move), move || {
        fs::write(&to, "OTHER").unwrap();
    });
    expect_code(
        call(
            &vault,
            "vault_move",
            json!({"from": "f.txt", "to": "g.txt"}),
        ),
        ErrorCode::AlreadyExists,
        "移す先に、ほかのアプリのファイル",
    );
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "from");
    assert_eq!(fs::read_to_string(path.join("g.txt")).unwrap(), "OTHER");
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// T28：移すものを、門のあとで読み取り専用の印のファイルに差し替える → CONFLICT。
#[test]
fn t28_a_source_swapped_after_the_gate_is_a_conflict() {
    let dir = TestDir::new("t28");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "abc");
    let f = path.join("f.txt");
    on_next(Step::AfterGate, move || swap_for_read_only(&f, "XYZ"));
    let result = call(
        &vault,
        "vault_move",
        json!({"from": "f.txt", "to": "g.txt"}),
    );
    set_read_only(&path.join("f.txt"), false);
    expect_code(result, ErrorCode::Conflict, "移すものが差し替えられた");
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "XYZ");
    assert!(!path.join("g.txt").exists());
}

/// F3：rename のあとで最終のパスを外と判定させる → 置き換えずに元の場所へ戻し、OUTSIDE_VAULT。
#[test]
fn f3_a_moved_item_found_outside_is_moved_back() {
    let dir = TestDir::new("f3");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "x");
    let before = journal(&path).len();
    let elsewhere = dir.join("elsewhere").join("g.txt");
    fake_final_path(FinalAt::Renamed, move |_| elsewhere);
    expect_code(
        call(
            &vault,
            "vault_move",
            json!({"from": "f.txt", "to": "g.txt"}),
        ),
        ErrorCode::OutsideVault,
        "移した先が外",
    );
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "x");
    assert!(!path.join("g.txt").exists());
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// F3b：F3 に加えて、元へ戻す直前に元の場所にファイルを作る → OUTSIDE_VAULT。文に「元の場所へ戻せなかった」と
/// 移した先の相対パス。元の場所のファイルは置き換えない。
#[test]
fn f3b_a_failed_move_back_is_reported() {
    let dir = TestDir::new("f3b");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "x");
    let elsewhere = dir.join("elsewhere").join("g.txt");
    fake_final_path(FinalAt::Renamed, move |_| elsewhere);
    let back = path.join("f.txt");
    on_next(Step::BeforeRenameBack, move || {
        fs::write(&back, "OTHER").unwrap();
    });
    let err = call(
        &vault,
        "vault_move",
        json!({"from": "f.txt", "to": "g.txt"}),
    )
    .unwrap_err();
    assert!(
        err.message().contains("元の場所へ戻せなかった") && err.message().contains("g.txt"),
        "{err}"
    );
    expect_code(Err::<(), _>(err), ErrorCode::OutsideVault, "移した先が外");
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "OTHER");
    assert_eq!(fs::read_to_string(path.join("g.txt")).unwrap(), "x");
}

// ---------------------------------------------------------------------------
// vault_delete（ゴミ箱へ移す）
// ---------------------------------------------------------------------------

/// T10：消すものの親を、門のあとで外へのジャンクションに替える → OUTSIDE_VAULT。外のものはゴミ箱へ移らない。
#[test]
fn t10_delete_does_not_follow_a_swapped_parent() {
    let dir = TestDir::new("t10");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a/keep.txt", "inside");
    let outside = outside_with_keep(&dir);
    let a = path.join("a");
    let target = outside.clone();
    on_next(Step::AfterGate, move || swap_for_junction(&a, &target));
    expect_code(
        call(&vault, "vault_delete", json!({"path": "a/keep.txt"})),
        ErrorCode::OutsideVault,
        "消すものの親がジャンクション",
    );
    assert_outside_kept(&outside);
    assert!(trash_ids(&path).is_empty(), "ゴミ箱に何か入った");
}

/// T27：消すものを、門のあとで読み取り専用の印のファイルに差し替える → CONFLICT。ゴミ箱へ移らない。
#[test]
fn t27_a_target_swapped_after_the_gate_is_not_deleted() {
    let dir = TestDir::new("t27");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "abc");
    let f = path.join("f.txt");
    on_next(Step::AfterGate, move || swap_for_read_only(&f, "XYZ"));
    let result = call(&vault, "vault_delete", json!({"path": "f.txt"}));
    set_read_only(&path.join("f.txt"), false);
    expect_code(result, ErrorCode::Conflict, "消すものが差し替えられた");
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "XYZ");
    assert!(trash_ids(&path).is_empty(), "ゴミ箱に何か入った");
}

/// T14：ゴミ箱へ移す rename をぶつからせ（`<id>/data` に空でないフォルダ）、`<id>` の名前も変えようとする
/// （持っているので Err）→ IO_ERROR。消すものは元の場所。entry.json は作ったハンドルで消え、`<id>` は開き直して
/// ID が一致するので消しにいくが、data が残るので残る。外の entry.json は変わらない。
#[test]
fn t14_cleanup_after_a_failed_move_to_the_trash() {
    let dir = TestDir::new("t14");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let before = journal(&path).len();
    let outside = dir.join("outside");
    fs::create_dir_all(outside.join("data")).unwrap();
    fs::write(outside.join("data").join("x"), "x").unwrap();
    fs::write(outside.join("entry.json"), "OUTSIDE-ENTRY").unwrap();
    let renamed = slot();
    let out = renamed.clone();
    let trash = trash_dir(&path);
    let target = outside.clone();
    on_next(Step::BeforeRename(Rename::ToTrash), move || {
        let id = fs::read_dir(&trash)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::create_dir(id.join("data")).unwrap();
        fs::write(id.join("data").join("x"), "x").unwrap();
        let ok = fs::rename(&id, moved_path(&id)).is_ok();
        *out.borrow_mut() = Some(ok);
        if ok {
            assert!(junction(&id, &target), "ジャンクションを作れない");
        }
    });
    let result = call(&vault, "vault_delete", json!({"path": "a.txt"}));
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        "ゴミ箱の <id> の名前を変えられた"
    );
    expect_code(result, ErrorCode::IoError, "ゴミ箱の data がぶつかった");
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
    assert_eq!(
        fs::read_to_string(outside.join("entry.json")).unwrap(),
        "OUTSIDE-ENTRY"
    );
    let ids = trash_ids(&path);
    assert_eq!(ids.len(), 1, "{ids:?}");
    let id = trash_dir(&path).join(&ids[0]);
    assert!(!id.join("entry.json").exists(), "entry.json が残った");
    assert!(id.join("data").join("x").exists(), "data が残る（145）");
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// T14b：`<id>` を消す直前（閉じたあと・開き直す前）に差し替える → 差し替えたものは消さない。
/// (a) `<id>` を別の名前へ移し、同じ名前の空のフォルダを置く → 新しい `<id>` も移したほうも残る。
/// (b) 同じ名前で、外の空のフォルダを指すジャンクションを置く → ジャンクションは消えない。
#[test]
fn t14b_the_trash_entry_folder_is_removed_only_when_the_id_matches() {
    for case in ["a", "b"] {
        let dir = TestDir::new(&format!("t14b-{case}"));
        let (path, vault) = new_vault(&dir, "v");
        write(&vault, "a.txt", "a");
        let outside = dir.join("outside-empty");
        fs::create_dir(&outside).unwrap();
        let trash = trash_dir(&path);
        let id_slot: Rc<RefCell<Option<PathBuf>>> = slot();
        let out = id_slot.clone();
        on_next(Step::BeforeRename(Rename::ToTrash), move || {
            let id = fs::read_dir(&trash)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            fs::create_dir(id.join("data")).unwrap();
            *out.borrow_mut() = Some(id);
        });
        let ids = id_slot.clone();
        let target = outside.clone();
        on_next(Step::BeforeTrashCleanup, move || {
            let id = ids.borrow().clone().unwrap();
            fs::rename(&id, moved_path(&id)).unwrap();
            if case == "a" {
                fs::create_dir(&id).unwrap();
            } else {
                assert!(junction(&id, &target), "ジャンクションを作れない");
            }
        });
        expect_code(
            call(&vault, "vault_delete", json!({"path": "a.txt"})),
            ErrorCode::IoError,
            "ゴミ箱の data がぶつかった",
        );
        assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
        let id = id_slot.borrow().clone().unwrap();
        let meta = fs::symlink_metadata(&id)
            .unwrap_or_else(|_| panic!("({case}) 差し替えた <id> を消した"));
        assert_eq!(meta.file_type().is_symlink(), case == "b", "({case})");
        assert!(
            moved_path(&id).join("data").exists(),
            "({case}) 移したほうが消えた"
        );
        assert!(outside.exists(), "({case}) 外のフォルダが消えた");
    }
}

/// T15 (a)：ゴミ箱へ移す直前に、.vault/trash を外へのジャンクションに替える → NOT_A_VAULT。外に何もできない。
#[test]
fn t15a_a_trash_folder_swapped_before_use_is_not_a_vault() {
    let dir = TestDir::new("t15a");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let outside = outside_with_keep(&dir);
    let trash = trash_dir(&path);
    let target = outside.clone();
    on_next(Step::Open(Place::Trash), move || {
        swap_for_junction(&trash, &target);
    });
    expect_code(
        call(&vault, "vault_delete", json!({"path": "a.txt"})),
        ErrorCode::NotAVault,
        ".vault/trash がジャンクション",
    );
    assert_outside_kept(&outside);
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
}

/// T15 (b)：ゴミ箱へ移す rename の直前に、.vault/trash の名前を変えようとする → Err。削除は成功する。
#[test]
fn t15b_the_trash_folder_cannot_be_renamed_while_moving() {
    let dir = TestDir::new("t15b");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let renamed = slot();
    let trash = trash_dir(&path);
    on_next(
        Step::BeforeRename(Rename::ToTrash),
        try_rename(
            trash.clone(),
            trash.with_file_name("trash2"),
            renamed.clone(),
        ),
    );
    let id = delete(&vault, "a.txt");
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        ".vault/trash の名前を変えられた"
    );
    assert!(trash.join(&id).join("data").exists());
    assert!(!path.join("a.txt").exists());
}

/// T15c：.vault/trash がない状態で消し、作ったゴミ箱のフォルダを閉じたあと（開き直す前）に外へのジャンクションに
/// 替える → NOT_A_VAULT。外に何もできない。
#[test]
fn t15c_a_created_trash_folder_swapped_before_reopening() {
    let dir = TestDir::new("t15c");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let trash = trash_dir(&path);
    fs::remove_dir_all(&trash).unwrap();
    let outside = outside_with_keep(&dir);
    let target = outside.clone();
    on_next(Step::AfterCreatedClosed, move || {
        swap_for_junction(&trash, &target);
    });
    expect_code(
        call(&vault, "vault_delete", json!({"path": "a.txt"})),
        ErrorCode::NotAVault,
        "作った .vault/trash がジャンクションに替わった",
    );
    assert_outside_kept(&outside);
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
}

/// T15d：.vault/trash がある状態で消し、作った `<id>` を閉じたあと（開き直す前）に、別の名前へ移して
/// 同じ名前の空のフォルダを置く → 開き直したものの ID が作ったときと違うので NOT_A_VAULT。どちらも消えず、
/// 消すものは元の場所にある。
#[test]
fn t15d_a_created_trash_entry_folder_replaced_before_reopening() {
    let dir = TestDir::new("t15d");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let trash = trash_dir(&path);
    assert!(trash.is_dir());
    let id_slot: Rc<RefCell<Option<PathBuf>>> = slot();
    let out = id_slot.clone();
    on_next(Step::AfterCreatedClosed, move || {
        let id = fs::read_dir(&trash)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::rename(&id, moved_path(&id)).unwrap();
        fs::create_dir(&id).unwrap();
        *out.borrow_mut() = Some(id);
    });
    expect_code(
        call(&vault, "vault_delete", json!({"path": "a.txt"})),
        ErrorCode::NotAVault,
        "作った <id> が、開き直す前に別のフォルダに替えられた",
    );
    let id = id_slot
        .borrow()
        .clone()
        .expect("差し込み口が呼ばれていない");
    assert!(id.is_dir(), "置いた空のフォルダを消した");
    assert!(moved_path(&id).is_dir(), "移したほうを消した");
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
}

/// F8：作った trash/<id> の最終のパスを外と判定させる → NOT_A_VAULT。<id> は作ったハンドルで消え、
/// 消すものは元の場所にある。
#[test]
fn f8_a_created_trash_entry_folder_found_outside_is_removed() {
    let dir = TestDir::new("f8");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let elsewhere = dir.join("elsewhere");
    fake_final_path(FinalAt::Created, move |_| elsewhere);
    expect_code(
        call(&vault, "vault_delete", json!({"path": "a.txt"})),
        ErrorCode::NotAVault,
        "作った <id> が外",
    );
    assert!(trash_ids(&path).is_empty(), "<id> が残った");
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
}

/// F10：ゴミ箱へ移した rename のあとで最終のパスを外と判定させる → 元の場所へ戻し、NOT_A_VAULT。
/// ゴミ箱に entry は残らない。
#[test]
fn f10_an_item_moved_to_the_trash_found_elsewhere_is_moved_back() {
    let dir = TestDir::new("f10");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let before = journal(&path).len();
    let elsewhere = dir.join("elsewhere").join("data");
    fake_final_path(FinalAt::Trashed, move |_| elsewhere);
    expect_code(
        call(&vault, "vault_delete", json!({"path": "a.txt"})),
        ErrorCode::NotAVault,
        "ゴミ箱へ移した先が違う",
    );
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
    assert!(list_ids(&vault).is_empty(), "ゴミ箱に entry が残った");
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

// ---------------------------------------------------------------------------
// vault_restore
// ---------------------------------------------------------------------------

/// T11a：戻す先の親を、門のあとで外へのジャンクションに替える → OUTSIDE_VAULT。中身と entry はゴミ箱に残る。
#[test]
fn t11a_restore_does_not_follow_a_swapped_destination_parent() {
    let dir = TestDir::new("t11a");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a/f.txt", "x");
    let id = delete(&vault, "a/f.txt");
    let outside = outside_with_keep(&dir);
    let a = path.join("a");
    let target = outside.clone();
    on_next(Step::AfterGate, move || swap_for_junction(&a, &target));
    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": id})),
        ErrorCode::OutsideVault,
        "戻す先の親がジャンクション",
    );
    assert_outside_kept(&outside);
    let entry = trash_dir(&path).join(&id);
    assert_eq!(fs::read_to_string(entry.join("data")).unwrap(), "x");
    assert!(entry.join("entry.json").exists());
}

/// T11b：戻す rename の直前に、戻す先にファイルを作る → CONFLICT。entry は残り、そのファイルは変わらない。
#[test]
fn t11b_a_file_created_at_the_restore_target_is_not_replaced() {
    let dir = TestDir::new("t11b");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "old");
    let id = delete(&vault, "f.txt");
    let before = journal(&path).len();
    let f = path.join("f.txt");
    on_next(Step::BeforeRename(Rename::Restore), move || {
        fs::write(&f, "OTHER").unwrap();
    });
    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": id})),
        ErrorCode::Conflict,
        "戻す先に、ほかのアプリのファイル",
    );
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "OTHER");
    let entry = trash_dir(&path).join(&id);
    assert_eq!(fs::read_to_string(entry.join("data")).unwrap(), "old");
    assert!(entry.join("entry.json").exists());
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// T13：replace:true で、戻す先の今あるものを、門のあとで読み取り専用の印のファイルに差し替える → CONFLICT。
/// 差し替えたファイルはゴミ箱へ移らず、entry は残る。
#[test]
fn t13_a_replaced_target_swapped_after_the_gate_is_a_conflict() {
    let dir = TestDir::new("t13");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "old");
    let id = delete(&vault, "f.txt");
    write(&vault, "f.txt", "current");
    let f = path.join("f.txt");
    on_next(Step::AfterGate, move || swap_for_read_only(&f, "SWAPPED"));
    let result = call(
        &vault,
        "vault_restore",
        json!({"trash_id": id, "replace": true}),
    );
    set_read_only(&path.join("f.txt"), false);
    expect_code(result, ErrorCode::Conflict, "今あるものが差し替えられた");
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "SWAPPED");
    assert_eq!(
        trash_ids(&path),
        std::slice::from_ref(&id),
        "差し替えたものがゴミ箱へ移った"
    );
    assert!(trash_dir(&path).join(&id).join("entry.json").exists());
}

/// T29：復元で読む直前に entry.json を外へのジャンクションに替える → INVALID_TRASH_ENTRY。
/// 一覧はその項目を出さない。外は変わらない。
#[test]
fn t29_an_entry_json_swapped_for_a_junction_is_invalid() {
    let dir = TestDir::new("t29");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let id = delete(&vault, "a.txt");
    let outside = outside_with_keep(&dir);
    let entry = trash_dir(&path).join(&id).join("entry.json");
    let target = outside.clone();
    on_next(Step::Open(Place::Entry), move || {
        swap_for_junction(&entry, &target)
    });
    expect_code(
        call(&vault, "vault_restore", json!({"trash_id": id})),
        ErrorCode::InvalidTrashEntry,
        "entry.json がジャンクション",
    );
    assert!(!list_ids(&vault).contains(&id), "一覧に出た");
    assert_outside_kept(&outside);
    assert!(!path.join("a.txt").exists());
}

// ---------------------------------------------------------------------------
// 自分で自分の共有の違反にならない（T18）・時刻だけの違い（V3）
// ---------------------------------------------------------------------------

/// T18：3段のフォルダを、削除（recursive）・移動・復元（replace でフォルダを置き換える・to を付けた戻し）・
/// open の orphan_tmp で移す → どれも BUSY にならない。rename の直前に、移すフォルダの名前は変えられない。
#[test]
fn t18_folders_are_moved_without_sharing_violations() {
    let dir = TestDir::new("t18");
    let (path, vault) = new_vault(&dir, "v");

    // 削除（recursive）
    three_levels(&vault, "del");
    let renamed = slot();
    on_next(
        Step::BeforeRename(Rename::ToTrash),
        try_rename(path.join("del"), path.join("del-x"), renamed.clone()),
    );
    let del_id = delete(&vault, "del");
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        "消すフォルダの名前を変えられた"
    );

    // 移動
    three_levels(&vault, "mv");
    let renamed = slot();
    on_next(
        Step::BeforeRename(Rename::Move),
        try_rename(path.join("mv"), path.join("mv-x"), renamed.clone()),
    );
    call(&vault, "vault_move", json!({"from": "mv", "to": "moved"})).unwrap();
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        "移すフォルダの名前を変えられた"
    );
    assert!(path.join("moved").join("b").join("c").join("f3").exists());

    // 復元（replace:true でフォルダを置き換える）
    three_levels(&vault, "del");
    let data = trash_dir(&path).join(&del_id).join("data");
    let renamed = slot();
    on_next(
        Step::BeforeRename(Rename::Restore),
        try_rename(data.clone(), data.with_file_name("data-x"), renamed.clone()),
    );
    let r = call(
        &vault,
        "vault_restore",
        json!({"trash_id": del_id, "replace": true}),
    )
    .unwrap();
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        "戻すフォルダの名前を変えられた"
    );
    let replaced = text(&r, "replaced_trash_id");

    // 復元（to を付けて戻す）
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": replaced, "to": "elsewhere/again"}),
    )
    .unwrap();
    assert!(
        path.join("elsewhere")
            .join("again")
            .join("b")
            .join("c")
            .join("f3")
            .exists()
    );

    // open の orphan_tmp
    drop(vault);
    let left = path.join(".vault").join("tmp").join("9-9.d");
    fs::create_dir_all(left.join("b").join("c")).unwrap();
    fs::write(left.join("f1"), "1").unwrap();
    fs::write(left.join("b").join("f2"), "22").unwrap();
    fs::write(left.join("b").join("c").join("f3"), "333").unwrap();
    let renamed = slot();
    on_next(
        Step::BeforeRename(Rename::ToTrash),
        try_rename(left.clone(), left.with_file_name("9-9.x"), renamed.clone()),
    );
    let vault = Vault::open(&path, false, &env()).unwrap();
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        "片づけるフォルダの名前を変えられた"
    );
    assert_eq!(
        count(&path.join(".vault").join("tmp")),
        0,
        "片づけが済んでいない"
    );
    let items = call(&vault, "vault_trash_list", json!({})).unwrap();
    let orphan = items["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["reason"] == "orphan_tmp")
        .cloned()
        .unwrap();
    assert_eq!(orphan["kind"], "dir");
    assert_eq!(orphan["size"], 6);
}

/// V3：削除・移動・replace:true の門のあとで、時刻だけを変える → 成功（時刻は比べない。ゴミ箱に入るので戻せる）。
#[test]
fn v3_only_a_changed_time_does_not_stop_delete_move_or_replace() {
    let dir = TestDir::new("v3");
    let (path, vault) = new_vault(&dir, "v");

    write(&vault, "d.txt", "d");
    let f = path.join("d.txt");
    on_next(Step::AfterGate, move || touch_back(&f));
    call(&vault, "vault_delete", json!({"path": "d.txt"})).unwrap();

    write(&vault, "m.txt", "m");
    let f = path.join("m.txt");
    on_next(Step::AfterGate, move || touch_back(&f));
    call(
        &vault,
        "vault_move",
        json!({"from": "m.txt", "to": "m2.txt"}),
    )
    .unwrap();

    write(&vault, "r.txt", "old");
    let id = delete(&vault, "r.txt");
    write(&vault, "r.txt", "cur");
    let f = path.join("r.txt");
    on_next(Step::AfterGate, move || touch_back(&f));
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": id, "replace": true}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path.join("r.txt")).unwrap(), "old");
}

// ---------------------------------------------------------------------------
// open の orphan_tmp（T17 の移す部分）・一覧の使用中・読む道の使用中
// ---------------------------------------------------------------------------

/// T17（移す部分）：open の片づけで、.vault/tmp を読む直前に外へのジャンクションに替える → 外のものはゴミ箱へ移らない。
#[test]
fn t17_orphan_cleanup_does_not_follow_a_swapped_tmp_folder() {
    let dir = TestDir::new("t17");
    let (path, vault) = new_vault(&dir, "v");
    drop(vault);
    let tmp = path.join(".vault").join("tmp");
    fs::write(tmp.join("1-1.tmp"), "left").unwrap();
    let outside = outside_with_keep(&dir);
    let target = outside.clone();
    let t = tmp.clone();
    on_next(Step::Open(Place::Tmp), move || {
        swap_for_junction(&t, &target)
    });
    let _vault = Vault::open(&path, false, &env()).unwrap();
    assert_outside_kept(&outside);
    assert!(trash_ids(&path).is_empty(), "外のものがゴミ箱へ移った");
}

/// ゴミ箱の一覧：`<id>` をほかのハンドルが DELETE の権利で開いている（使用中）なら、その項目を飛ばす
/// （一覧の全体は失敗させない）。閉じたあとは出る。
#[test]
fn trash_list_skips_an_entry_folder_in_use() {
    let dir = TestDir::new("trash-list-busy");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let busy = delete(&vault, "a.txt");
    write(&vault, "b.txt", "b");
    let free = delete(&vault, "b.txt");
    const DELETE: u32 = 0x0001_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let holder = fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(7)
        .custom_flags(BACKUP_SEMANTICS)
        .open(trash_dir(&path).join(&busy))
        .unwrap();
    let ids = list_ids(&vault);
    drop(holder);
    assert_eq!(ids, std::slice::from_ref(&free), "使用中の <id> を出した");
    let mut ids = list_ids(&vault);
    ids.sort();
    let mut both = vec![busy, free];
    both.sort();
    assert_eq!(ids, both);
}

/// 消すものがほかのアプリに使用中（共有なしで開かれている）なら、3回試してから BUSY。文はツールに合わせる
/// （「消すものを開けない（使用中。…）」）。何もゴミ箱へ移らない。
#[test]
fn deleting_a_file_in_use_is_busy_with_a_message_for_the_tool() {
    let dir = TestDir::new("delete-busy");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path.join("a.txt"))
        .unwrap();
    let result = call(&vault, "vault_delete", json!({"path": "a.txt"}));
    drop(holder);
    let err = result.unwrap_err();
    assert!(
        err.message().contains("消すものを開けない（使用中"),
        "文にツールの言葉がない：{err}"
    );
    expect_code(
        Err::<(), _>(err),
        ErrorCode::Busy,
        "ほかのアプリが開いている",
    );
    assert!(trash_ids(&path).is_empty(), "ゴミ箱に何か入った");
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
}

/// 読む道の使用中：ほかのアプリが共有なしで開いているファイルを読む → 試し直してから BUSY。
#[test]
fn reading_a_file_in_use_is_busy() {
    let dir = TestDir::new("read-busy");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path.join("a.txt"))
        .unwrap();
    let result = call(&vault, "vault_read", json!({"path": "a.txt"}));
    drop(holder);
    expect_code(result, ErrorCode::Busy, "ほかのアプリが開いている");
    call(&vault, "vault_read", json!({"path": "a.txt"})).unwrap();
}

/// 一覧を拒否（RD）したフォルダを中身ごと消す → ACCESS_DENIED（2つめの門の TARGET_DIR が一覧の権利を求めるため。
/// 今までは中の大きさを数えるところで IO_ERROR）。中身は変わらず、ゴミ箱にも入らない。
#[test]
fn deleting_a_list_denied_folder_is_access_denied() {
    let dir = TestDir::new("delete-rd");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "d/f.txt", "x");
    let deny = common::DenyAce::new(&path.join("d"), "(RD)");
    let result = call(
        &vault,
        "vault_delete",
        json!({"path": "d", "recursive": true}),
    );
    drop(deny);
    expect_code(result, ErrorCode::AccessDenied, "一覧を拒否したフォルダ");
    assert_eq!(
        fs::read_to_string(path.join("d").join("f.txt")).unwrap(),
        "x"
    );
    assert!(trash_ids(&path).is_empty(), "ゴミ箱に何か入った");
}
