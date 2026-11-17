//! 確かめてから使うまでの隙（TOCTOU）に、決まった順で差し替えを起こすテスト（T1・T4〜T8・T19・T19b・T20・T21・T22・T25・T26・T30・T31・T32）。
//!
//! 差し替えは `test_support::on_next` で、決まった所（Step）で1回だけ起こす。保管庫と「外」のフォルダは、
//! どれも `CARGO_TARGET_TMPDIR` の下（`TestDir`）。

#![cfg(windows)]

mod common;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::{
    DenyAce, TestDir, call, env, expect_code, journal, junction, new_vault, set_read_only,
};
use serde_json::json;
use vault_core::test_support::{Place, Rename, Step, on_next};
use vault_core::{ErrorCode, Op, Vault};

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

fn tmp_count(vault: &Path) -> usize {
    count(&vault.join(".vault").join("tmp"))
}

/// hook の中の結果を、あとで確かめるための入れ物。
fn slot<T: 'static>() -> Rc<RefCell<Option<T>>> {
    Rc::new(RefCell::new(None))
}

/// hook の中で、`from` を `to` へ移そうとした結果（Ok なら true）。
fn try_rename(from: PathBuf, to: PathBuf, out: Rc<RefCell<Option<bool>>>) -> impl FnOnce() {
    move || {
        let ok = fs::rename(&from, &to).is_ok();
        *out.borrow_mut() = Some(ok);
    }
}

/// T1：vault_read の途中の要素を、門のあとで外へのジャンクションに替える → OUTSIDE_VAULT。外の中身を返さない。
#[test]
fn t1_read_does_not_follow_a_swapped_middle_folder() {
    let dir = TestDir::new("t1");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a/f.txt", "content": "inside"}),
    )
    .unwrap();
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("f.txt"), "SECRET").unwrap();
    let a = path.join("a");
    let target = outside.clone();
    on_next(Step::AfterGate, move || swap_for_junction(&a, &target));
    let result = call(&vault, "vault_read", json!({"path": "a/f.txt"}));
    if let Ok(v) = &result {
        assert!(!v.to_string().contains("SECRET"), "外の中身を返した");
    }
    expect_code(result, ErrorCode::OutsideVault, "途中がジャンクション");
}

/// T4：vault_write の書く先の親を、門のあとで外へのジャンクションに替える → OUTSIDE_VAULT。
/// 外の同じ名前のファイルは変わらず、外に新しい項目もできない。.vault/tmp は空、履歴は増えない。
#[test]
fn t4_write_does_not_follow_a_swapped_parent() {
    let dir = TestDir::new("t4");
    let (path, vault) = new_vault(&dir, "v");
    call(&vault, "vault_mkdir", json!({"path": "d"})).unwrap();
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("f.txt"), "OUTSIDE").unwrap();
    let before = journal(&path).len();
    let d = path.join("d");
    let target = outside.clone();
    on_next(Step::AfterGate, move || swap_for_junction(&d, &target));
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "d/f.txt", "content": "new"}),
        ),
        ErrorCode::OutsideVault,
        "書く先の親がジャンクション",
    );
    assert_eq!(
        fs::read_to_string(outside.join("f.txt")).unwrap(),
        "OUTSIDE"
    );
    assert_eq!(count(&outside), 1, "外に新しい項目ができた");
    assert_eq!(tmp_count(&path), 0, ".vault/tmp に残った");
    assert_eq!(journal(&path).len(), before, "履歴が増えた");
}

/// T4b：最後の rename の直前に、書く先の親の名前を変えようとする → Err（持っているので変えられない）。書き込みは中で成功する。
#[test]
fn t4b_the_parent_cannot_be_renamed_before_the_final_rename() {
    let dir = TestDir::new("t4b");
    let (path, vault) = new_vault(&dir, "v");
    call(&vault, "vault_mkdir", json!({"path": "d"})).unwrap();
    let renamed = slot();
    on_next(
        Step::BeforeRename(Rename::WriteFinal),
        try_rename(path.join("d"), path.join("d2"), renamed.clone()),
    );
    call(
        &vault,
        "vault_write",
        json!({"path": "d/f.txt", "content": "new"}),
    )
    .unwrap();
    assert_eq!(*renamed.borrow(), Some(false), "親の名前を変えられた");
    assert_eq!(
        fs::read_to_string(path.join("d").join("f.txt")).unwrap(),
        "new"
    );
}

/// T5：.vault/tmp を、開く直前に外へのジャンクションに替える → NOT_A_VAULT。外に何もできない。
#[test]
fn t5_a_swapped_tmp_folder_is_not_a_vault() {
    let dir = TestDir::new("t5");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    let tmp = path.join(".vault").join("tmp");
    let target = outside.clone();
    on_next(Step::Open(Place::Tmp), move || {
        swap_for_junction(&tmp, &target)
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "x"}),
        ),
        ErrorCode::NotAVault,
        ".vault/tmp がジャンクション",
    );
    assert_eq!(count(&outside), 0, "外に一時ファイルができた");
    assert!(!path.join("f.txt").exists());
}

/// T6：一時ファイルを作ったあと（中身を書く前）に .vault/tmp の名前を変えようとする → Err。中身は中に書かれる。
#[test]
fn t6_the_tmp_folder_cannot_be_renamed_while_writing() {
    let dir = TestDir::new("t6");
    let (path, vault) = new_vault(&dir, "v");
    let renamed = slot();
    let tmp = path.join(".vault").join("tmp");
    on_next(
        Step::AfterTmpCreated,
        try_rename(tmp.clone(), tmp.with_file_name("tmp2"), renamed.clone()),
    );
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "inside"}),
    )
    .unwrap();
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        ".vault/tmp の名前を変えられた"
    );
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "inside");
    assert_eq!(tmp_count(&path), 0);
}

/// T7：vault_mkdir "p/new" の門のあとで、p を外へのジャンクションに替える → OUTSIDE_VAULT。
/// 外に中身を書かない・外にもとからあった keep とその中は消えない・外に new（空の項目）もできない。
#[test]
fn t7_mkdir_does_not_create_or_remove_outside() {
    let dir = TestDir::new("t7");
    let (path, vault) = new_vault(&dir, "v");
    call(&vault, "vault_mkdir", json!({"path": "p"})).unwrap();
    let outside = dir.join("outside");
    fs::create_dir_all(outside.join("keep")).unwrap();
    fs::write(outside.join("keep").join("k.txt"), "keep").unwrap();
    let p = path.join("p");
    let target = outside.clone();
    on_next(Step::AfterGate, move || swap_for_junction(&p, &target));
    expect_code(
        call(&vault, "vault_mkdir", json!({"path": "p/new"})),
        ErrorCode::OutsideVault,
        "親がジャンクション",
    );
    assert_eq!(
        fs::read_to_string(outside.join("keep").join("k.txt")).unwrap(),
        "keep"
    );
    assert!(!outside.join("new").exists(), "外に new ができた");
    assert_eq!(count(&outside), 1);
}

/// T8：create_dirs で a・b を作るとき、b を作る直前に a の名前を変えようとする → Err。全部が中にできる。
#[test]
fn t8_created_parents_cannot_be_renamed_while_creating_children() {
    let dir = TestDir::new("t8");
    let (path, vault) = new_vault(&dir, "v");
    let renamed = slot();
    // 1回目（a を作る前）は何もしない。2回目（b を作る前）に a の名前を変えようとする
    on_next(Step::BeforeCreate, || {});
    on_next(
        Step::BeforeCreate,
        try_rename(path.join("a"), path.join("a2"), renamed.clone()),
    );
    call(
        &vault,
        "vault_write",
        json!({"path": "a/b/c.txt", "content": "c"}),
    )
    .unwrap();
    assert_eq!(*renamed.borrow(), Some(false), "作った親の名前を変えられた");
    assert_eq!(
        fs::read_to_string(path.join("a").join("b").join("c.txt")).unwrap(),
        "c"
    );
    assert!(!path.join("a2").exists());
}

/// T19：履歴のフォルダを、開く直前に外へのジャンクションに替える → NOT_A_VAULT。外に履歴の行がない。
#[test]
fn t19_a_swapped_journal_folder_is_not_a_vault() {
    let dir = TestDir::new("t19");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    let journal_dir = path.join(".vault").join("journal");
    let target = outside.clone();
    on_next(Step::Open(Place::Journal), move || {
        swap_for_junction(&journal_dir, &target);
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "x"}),
        ),
        ErrorCode::NotAVault,
        "履歴のフォルダがジャンクション",
    );
    assert_eq!(count(&outside), 0, "外に履歴ができた");
}

/// T19b：履歴のフォルダがない状態で書き、作った履歴のフォルダを閉じたあと（開き直す前）に外へのジャンクションに替える
/// → NOT_A_VAULT。外に履歴の行がない。
#[test]
fn t19b_a_created_journal_folder_swapped_before_reopening() {
    let dir = TestDir::new("t19b");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    let journal_dir = path.join(".vault").join("journal");
    fs::rename(&journal_dir, path.join(".vault").join("journal-old")).unwrap();
    let target = outside.clone();
    let jd = journal_dir.clone();
    on_next(Step::AfterCreatedClosed, move || {
        swap_for_junction(&jd, &target)
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "x"}),
        ),
        ErrorCode::NotAVault,
        "作った履歴のフォルダが差し替えられた",
    );
    assert_eq!(count(&outside), 0, "外に履歴ができた");
}

/// T20：開いたまま保管庫のフォルダの名前を変える → 読む・書くツールが NOT_A_VAULT。中身は変わらない。
/// 名前を戻すと、また使える。
#[test]
fn t20_a_renamed_vault_folder_is_not_a_vault_until_renamed_back() {
    let dir = TestDir::new("t20");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    let renamed = dir.join("v-renamed");
    fs::rename(&path, &renamed).unwrap();
    expect_code(
        call(&vault, "vault_read", json!({"path": "f.txt"})),
        ErrorCode::NotAVault,
        "名前を変えた保管庫を読む",
    );
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "new"}),
        ),
        ErrorCode::NotAVault,
        "名前を変えた保管庫に書く",
    );
    assert_eq!(fs::read_to_string(renamed.join("f.txt")).unwrap(), "old");
    fs::rename(&renamed, &path).unwrap();
    let r = call(&vault, "vault_read", json!({"path": "f.txt"})).unwrap();
    assert_eq!(r["content"], "old");
}

/// T21：操作の根を開く直前に、根を外へのジャンクションに替える（外のフォルダ・本物の保管庫を移した先の2通り）
/// → どちらも NOT_A_VAULT。外は変わらない。
#[test]
fn t21_a_root_swapped_for_a_junction_is_not_a_vault() {
    let dir = TestDir::new("t21");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "inside"}),
    )
    .unwrap();
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("f.txt"), "OUTSIDE").unwrap();

    for (label, to_moved) in [("外のフォルダ", false), ("本物の保管庫を移した先", true)]
    {
        let root = path.clone();
        let target = if to_moved {
            moved_path(&path)
        } else {
            outside.clone()
        };
        on_next(Step::BeforeRoot, move || swap_for_junction(&root, &target));
        let result = call(&vault, "vault_read", json!({"path": "f.txt"}));
        if let Ok(v) = &result {
            assert!(
                !v.to_string().contains("OUTSIDE"),
                "{label}：外の中身を返した"
            );
        }
        expect_code(result, ErrorCode::NotAVault, label);
        // もとに戻す（ジャンクションを外し、移したものを戻す）
        fs::remove_dir(&path).unwrap();
        fs::rename(moved_path(&path), &path).unwrap();
    }
    assert_eq!(
        fs::read_to_string(outside.join("f.txt")).unwrap(),
        "OUTSIDE"
    );
    assert_eq!(count(&outside), 1);
    let r = call(&vault, "vault_read", json!({"path": "f.txt"})).unwrap();
    assert_eq!(r["content"], "inside");
}

/// T22 (a)：索引を読む直前に index.json を外へのジャンクションに替える → vault_write は成功し、索引は直さない。
/// 外は変わらない。次の vault_changes は NOT_A_VAULT。
#[test]
fn t22a_a_swapped_index_is_not_followed_when_read() {
    let dir = TestDir::new("t22a");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    let index = path.join(".vault").join("index.json");
    let target = outside.clone();
    on_next(Step::Open(Place::Index), move || {
        swap_for_junction(&index, &target)
    });
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "x"}),
    )
    .unwrap();
    assert_eq!(count(&outside), 0, "外に何かができた");
    assert!(
        fs::symlink_metadata(path.join(".vault").join("index.json"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "ジャンクションのまま（索引は直さない）"
    );
    expect_code(
        call(&vault, "vault_changes", json!({})),
        ErrorCode::NotAVault,
        "索引がジャンクション",
    );
}

/// T22 (b)：索引を読み取り専用にして書けなくし、消すために開く直前に index.json をジャンクションに替える
/// → ジャンクションは消さず、外も変わらない。
#[test]
fn t22b_a_swapped_index_is_not_removed() {
    let dir = TestDir::new("t22b");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "keep").unwrap();
    let index = path.join(".vault").join("index.json");
    set_read_only(&index, true);
    let target = outside.clone();
    let idx = index.clone();
    // 1回目（読む）は何もしない。2回目（書けなかった索引を消すために開く直前）に差し替える
    on_next(Step::Open(Place::Index), || {});
    on_next(Step::Open(Place::Index), move || {
        let moved = moved_path(&idx);
        fs::rename(&idx, &moved).unwrap();
        set_read_only(&moved, false);
        assert!(junction(&idx, &target));
    });
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "x"}),
    )
    .unwrap();
    assert!(
        fs::symlink_metadata(&index)
            .unwrap()
            .file_type()
            .is_symlink(),
        "ジャンクションを消した"
    );
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "keep"
    );
    assert_eq!(count(&outside), 1);
}

/// T25：.vault/lock を、開く直前に外へのジャンクションに替える → NOT_A_VAULT。外に何もできない。
#[test]
fn t25_a_swapped_lock_file_is_not_a_vault() {
    let dir = TestDir::new("t25");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    let lock = path.join(".vault").join("lock");
    let target = outside.clone();
    on_next(Step::Open(Place::Lock), move || {
        swap_for_junction(&lock, &target)
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "x"}),
        ),
        ErrorCode::NotAVault,
        "lock がジャンクション",
    );
    assert_eq!(count(&outside), 0);
}

/// T26：Vault::open で、vault.json を開く直前に外へのジャンクションに替える → NOT_A_VAULT。
#[test]
fn t26_a_swapped_vault_json_is_not_a_vault() {
    let dir = TestDir::new("t26");
    let (path, vault) = new_vault(&dir, "v");
    drop(vault);
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    let meta = path.join(".vault").join("vault.json");
    let target = outside.clone();
    on_next(Step::Open(Place::VaultJson), move || {
        swap_for_junction(&meta, &target);
    });
    expect_code(
        Vault::open(&path, false, &env()),
        ErrorCode::NotAVault,
        "vault.json がジャンクション",
    );
    assert_eq!(count(&outside), 0);
}

/// T30：書く操作のあいだ（門のあと）、今の履歴のファイルと .vault/lock は、ほかから名前を変える・消すことが
/// できない（共有に DELETE を含めない）。終わったあと、mkdir の行は中のファイルにあり、外には何もない。
#[test]
fn t30_journal_and_lock_files_cannot_be_moved_or_removed_while_writing() {
    let dir = TestDir::new("t30");
    let (path, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    let journal_dir = path.join(".vault").join("journal");
    let journal_file = fs::read_dir(&journal_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .max()
        .unwrap();
    let lock = path.join(".vault").join("lock");
    let results = slot::<Vec<(String, bool)>>();
    let out = results.clone();
    let out_dir = outside.clone();
    on_next(Step::AfterGate, move || {
        let mut r = Vec::new();
        for (label, file) in [("履歴", &journal_file), ("lock", &lock)] {
            let moved = fs::rename(file, out_dir.join("moved")).is_ok();
            r.push((format!("{label} の rename"), moved));
            if moved {
                let _ = fs::rename(out_dir.join("moved"), file);
            }
            let removed = fs::remove_file(file).is_ok();
            r.push((format!("{label} の remove_file"), removed));
        }
        *out.borrow_mut() = Some(r);
    });
    call(&vault, "vault_mkdir", json!({"path": "d"})).unwrap();
    for (label, ok) in results.borrow().as_ref().unwrap() {
        assert!(!ok, "{label} ができてしまった");
    }
    assert_eq!(count(&outside), 0);
    assert_eq!(journal(&path).last().unwrap().op, Op::Mkdir);
}

/// T31：create_dirs で作った n を、閉じたあと（開き直す前）に差し替える。
/// (a) n を n2 へ移して空の n を作る → CONFLICT。(b) n を外へのジャンクションに → OUTSIDE_VAULT。
/// (c) n をファイルに替える → CONFLICT。どれも x.txt はどこにもなく、.vault/tmp は空で、作ったものは消さない。
#[test]
fn t31_a_created_parent_swapped_before_reopening() {
    for case in ["a", "b", "c"] {
        let dir = TestDir::new(&format!("t31{case}"));
        let (path, vault) = new_vault(&dir, "v");
        let outside = dir.join("outside");
        fs::create_dir(&outside).unwrap();
        let n = path.join("n");
        let target = outside.clone();
        on_next(Step::AfterCreatedClosed, move || match case {
            "a" => {
                fs::rename(&n, n.with_file_name("n2")).unwrap();
                fs::create_dir(&n).unwrap();
            }
            "b" => swap_for_junction(&n, &target),
            _ => {
                fs::rename(&n, n.with_file_name("n2")).unwrap();
                fs::write(&n, "file").unwrap();
            }
        });
        let result = call(
            &vault,
            "vault_write",
            json!({"path": "n/x.txt", "content": "x"}),
        );
        let expected = if case == "b" {
            ErrorCode::OutsideVault
        } else {
            ErrorCode::Conflict
        };
        expect_code(result, expected, &format!("T31 ({case})"));
        assert!(
            !path.join("n").join("x.txt").exists(),
            "({case}) 中に書いた"
        );
        assert!(
            !path.join("n2").join("x.txt").exists(),
            "({case}) 移したほうに書いた"
        );
        assert!(!outside.join("x.txt").exists(), "({case}) 外に書いた");
        assert_eq!(count(&outside), 0, "({case}) 外が変わった");
        assert_eq!(tmp_count(&path), 0, "({case}) .vault/tmp に残った");
        if case == "b" {
            assert!(
                fs::symlink_metadata(path.join("n"))
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "ジャンクションを消した"
            );
        } else {
            assert!(path.join("n2").is_dir(), "({case}) 作ったフォルダを消した");
        }
    }
}

/// T32（形2）：一覧を拒否（RD）したフォルダの下のファイルは、今と同じく読め、上書きできる（通り抜けるだけの
/// フォルダは PassDir で持つ）。
#[test]
fn t32_files_below_a_list_denied_folder_can_be_read_and_written() {
    let dir = TestDir::new("t32");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "d/f.txt", "content": "old"}),
    )
    .unwrap();
    let deny = DenyAce::new(&path.join("d"), "(RD)");
    let read = call(&vault, "vault_read", json!({"path": "d/f.txt"}));
    let write = call(
        &vault,
        "vault_write",
        json!({"path": "d/f.txt", "content": "new"}),
    );
    drop(deny);
    assert_eq!(read.unwrap()["content"], "old");
    write.unwrap();
    assert_eq!(
        fs::read_to_string(path.join("d").join("f.txt")).unwrap(),
        "new"
    );
}

/// T19c：履歴のフォルダがない状態で書き、作った履歴のフォルダを閉じたあと（開き直す前）に、別の名前へ移して
/// 空の journal を作る → 開き直したものの ID が作ったときと違うので NOT_A_VAULT。どこにも履歴の行が増えない。
#[test]
fn t19c_a_created_journal_folder_replaced_by_another_folder() {
    let dir = TestDir::new("t19c");
    let (path, vault) = new_vault(&dir, "v");
    let vault_dir = path.join(".vault");
    let old = vault_dir.join("journal-old");
    fs::rename(vault_dir.join("journal"), &old).unwrap();
    let old_lines = journal_lines(&old);
    let jd = vault_dir.join("journal");
    let moved = vault_dir.join("journal-moved");
    let to = moved.clone();
    on_next(Step::AfterCreatedClosed, move || {
        fs::rename(&jd, &to).unwrap();
        fs::create_dir(&jd).unwrap();
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": "x"}),
        ),
        ErrorCode::NotAVault,
        "作った履歴のフォルダが、別のフォルダに替えられた",
    );
    assert_eq!(
        journal_lines(&vault_dir.join("journal")),
        0,
        "替えたほうに行が増えた"
    );
    assert_eq!(journal_lines(&moved), 0, "移したほうに行が増えた");
    assert_eq!(journal_lines(&old), old_lines);
    assert!(!path.join("f.txt").exists());
}

/// フォルダの中の履歴のファイルの行の数（ないなら 0）。
fn journal_lines(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|e| {
                    fs::read_to_string(e.unwrap().path())
                        .unwrap_or_default()
                        .lines()
                        .count()
                })
                .sum()
        })
        .unwrap_or(0)
}

/// T33（設計の決定で受け入れる）：通り抜けを拒否（X）したフォルダの下は、途中のフォルダを PassDir
/// （TRAVERSE を求める）で開くので、vault_read・vault_write・vault_mkdir が ACCESS_DENIED になる
/// （今までの std の道では、通り抜けの確かめを飛ばす特権で読めた）。中身は変わらない。
#[test]
fn t33_files_below_a_traverse_denied_folder_are_access_denied() {
    let dir = TestDir::new("t33");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "d/f.txt", "content": "old"}),
    )
    .unwrap();
    let deny = DenyAce::new(&path.join("d"), "(X)");
    let read = call(&vault, "vault_read", json!({"path": "d/f.txt"}));
    let write = call(
        &vault,
        "vault_write",
        json!({"path": "d/f.txt", "content": "new"}),
    );
    let mkdir = call(&vault, "vault_mkdir", json!({"path": "d/sub"}));
    drop(deny);
    expect_code(
        read,
        ErrorCode::AccessDenied,
        "通り抜けを拒否したフォルダの下を読む",
    );
    expect_code(
        write,
        ErrorCode::AccessDenied,
        "通り抜けを拒否したフォルダの下に書く",
    );
    expect_code(
        mkdir,
        ErrorCode::AccessDenied,
        "通り抜けを拒否したフォルダの下に作る",
    );
    assert_eq!(
        fs::read_to_string(path.join("d").join("f.txt")).unwrap(),
        "old"
    );
    assert!(!path.join("d").join("sub").exists());
}
