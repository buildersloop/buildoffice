//! 列挙の道で、確かめてから使うまでの隙（TOCTOU）に差し替えを起こすテスト（T2・T3・T3b・T17（ゴミ箱の一覧）・T24・V2・F6、走査で使用中の下のフォルダは
//! unknown、削除の recursive:false の隙、一覧を拒否したフォルダの一覧）。
//!
//! 差し替えは `test_support::on_next` で、決まった所（Step）で1回だけ起こす。保管庫と「外」のフォルダは、
//! どれも `CARGO_TARGET_TMPDIR` の下（`TestDir`）。

#![cfg(windows)]

mod common;

use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::{DenyAce, TestDir, call, env, expect_code, journal, junction, new_vault};
use serde_json::{Value, json};
use vault_core::test_support::{FinalAt, Place, Rename, Step, fake_final_path, on_next};
use vault_core::{ErrorCode, Op, Vault};

/// `path` を `<path>-moved` へ移し、`path` に `target` を指すジャンクションを置く。
fn swap_for_junction(path: &Path, target: &Path) {
    let moved = moved_path(path);
    fs::rename(path, &moved).unwrap_or_else(|e| panic!("差し替えのための rename：{e}"));
    assert!(junction(path, target), "ジャンクションを作れない");
}

/// `swap_for_junction` を元に戻す（ジャンクションを消し、移したものを戻す）。
fn swap_back(path: &Path) {
    fs::remove_dir(path).unwrap_or_else(|e| panic!("ジャンクションを消せない：{e}"));
    fs::rename(moved_path(path), path).unwrap_or_else(|e| panic!("戻せない：{e}"));
}

fn moved_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push("-moved");
    path.with_file_name(name)
}

/// hook の中の結果を、あとで確かめるための入れ物。
fn slot<T: 'static>() -> Rc<RefCell<Option<T>>> {
    Rc::new(RefCell::new(None))
}

fn write(vault: &Vault, path: &str, content: &str) {
    call(
        vault,
        "vault_write",
        json!({"path": path, "content": content}),
    )
    .unwrap();
}

/// 外のフォルダ（中に、外にしかない名前のファイル）。
fn outside_with_secret(dir: &TestDir) -> PathBuf {
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("SECRET-NAME.txt"), "OUTSIDE").unwrap();
    outside
}

fn assert_no_secret(what: &str, text: &str) {
    assert!(
        !text.contains("SECRET-NAME"),
        "{what}に外の名前が入った：{text}"
    );
}

/// 一覧の (path, kind)。
fn listed(result: &Value) -> Vec<(String, String)> {
    result["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["path"].as_str().unwrap().to_owned(),
                e["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// 走査のテスト用の保管庫（走査の最短の間隔を 0 にする）。
fn scan_vault(dir: &TestDir) -> (PathBuf, Vault) {
    let (path, mut vault) = new_vault(dir, "v");
    vault.set_min_rescan_interval(Duration::ZERO);
    vault.set_rescan_interval_factor(0); // 前の走査の時間で伸ばさない（続けて走査する）
    (path, vault)
}

fn rescan(vault: &Vault) -> Value {
    call(
        vault,
        "vault_changes",
        json!({"since_seq": 0, "limit": 10000}),
    )
    .unwrap()
}

fn index_text(vault_path: &Path) -> String {
    fs::read_to_string(vault_path.join(".vault").join("index.json")).unwrap()
}

fn journal_text(vault_path: &Path) -> String {
    format!("{:?}", journal(vault_path))
}

// ---------------------------------------------------------------------------
// vault_list（T2・F6・V2・一覧を拒否したフォルダ）
// ---------------------------------------------------------------------------

/// T2 (a)：一覧するフォルダを、門のあとで外へのジャンクションに替える → OUTSIDE_VAULT。外の名前を返さない。
#[test]
fn t2a_list_does_not_follow_a_swapped_folder() {
    let dir = TestDir::new("t2a");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "d/in.txt", "x");
    let outside = outside_with_secret(&dir);
    let d = path.join("d");
    on_next(Step::AfterGate, move || swap_for_junction(&d, &outside));
    let result = call(&vault, "vault_list", json!({"path": "d"}));
    if let Ok(v) = &result {
        assert_no_secret("一覧", &v.to_string());
    }
    expect_code(
        result,
        ErrorCode::OutsideVault,
        "一覧するフォルダがジャンクション",
    );
}

/// T2 (b)：一覧するフォルダを開いて確かめたあと、列挙の前に名前を変えようとする → Err（持っているので変えられない）。
/// 一覧は正しい。
#[test]
fn t2b_the_listed_folder_cannot_be_renamed_while_listing() {
    let dir = TestDir::new("t2b");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "d/in.txt", "x");
    let renamed = slot();
    let out = renamed.clone();
    let d = path.join("d");
    on_next(Step::BeforeList, move || {
        *out.borrow_mut() = Some(fs::rename(&d, d.with_file_name("d2")).is_ok());
    });
    let result = call(&vault, "vault_list", json!({"path": "d"})).unwrap();
    assert_eq!(
        *renamed.borrow(),
        Some(false),
        "一覧するフォルダの名前を変えられた"
    );
    assert_eq!(
        listed(&result),
        [("d/in.txt".to_owned(), "file".to_owned())]
    );
}

/// T2 (c)：recursive で、下のフォルダを開く直前に外へのジャンクションに替える → その項目は kind:link に直り、
/// 中は並ばない。外の名前を返さない。
#[test]
fn t2c_a_swapped_child_folder_is_listed_as_a_link() {
    let dir = TestDir::new("t2c");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "d/s/in.txt", "x");
    let outside = outside_with_secret(&dir);
    let s = path.join("d").join("s");
    on_next(Step::BeforeChildOpen, move || {
        swap_for_junction(&s, &outside)
    });
    let result = call(
        &vault,
        "vault_list",
        json!({"path": "d", "recursive": true}),
    )
    .unwrap();
    assert_no_secret("一覧", &result.to_string());
    assert_eq!(listed(&result), [("d/s".to_owned(), "link".to_owned())]);
}

/// F6：一覧するフォルダの最終のパスを外と判定させる → OUTSIDE_VAULT。
#[test]
fn f6_a_listed_folder_found_outside_is_rejected() {
    let dir = TestDir::new("f6");
    let (_path, vault) = new_vault(&dir, "v");
    write(&vault, "d/in.txt", "x");
    let elsewhere = dir.join("elsewhere").join("d");
    fake_final_path(FinalAt::Target, move |_| elsewhere);
    expect_code(
        call(&vault, "vault_list", json!({"path": "d"})),
        ErrorCode::OutsideVault,
        "一覧するフォルダが外",
    );
}

/// V2：ほかのハンドルが書き込みのために開いたまま書いているファイルで、vault_list の version と
/// vault_read の version が一致する（一覧の値は、項目を開いて読む）。
#[test]
fn v2_list_and_read_agree_while_another_handle_writes() {
    let dir = TestDir::new("v2");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "f.txt", "abc");
    let mut writer = fs::OpenOptions::new()
        .append(true)
        .share_mode(7)
        .open(path.join("f.txt"))
        .unwrap();
    for _ in 0..20 {
        writer.write_all(b"more bytes ").unwrap();
        let list = call(&vault, "vault_list", json!({"path": ""})).unwrap();
        let from_list = list["entries"][0]["version"].as_str().unwrap().to_owned();
        let read = call(&vault, "vault_read", json!({"path": "f.txt"})).unwrap();
        assert_eq!(
            from_list,
            read["version"].as_str().unwrap(),
            "一覧と読むの版が違う"
        );
        assert_eq!(list["entries"][0]["size"], read["size"]);
    }
    drop(writer);
}

/// 一覧を拒否（RD）したフォルダを一覧する → ACCESS_DENIED（HOLD_DIR が一覧の権利を求めるため。今までは IO_ERROR）。recursive で、その下へ進むときも同じ。
#[test]
fn listing_a_list_denied_folder_is_access_denied() {
    let dir = TestDir::new("list-rd");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "d/f.txt", "x");
    let deny = DenyAce::new(&path.join("d"), "(RD)");
    let direct = call(&vault, "vault_list", json!({"path": "d"}));
    let below = call(&vault, "vault_list", json!({"path": "", "recursive": true}));
    drop(deny);
    expect_code(direct, ErrorCode::AccessDenied, "一覧を拒否したフォルダ");
    expect_code(
        below,
        ErrorCode::AccessDenied,
        "recursive で一覧を拒否したフォルダの下へ",
    );
}

// ---------------------------------------------------------------------------
// 走査（T3・T3b・T24・使用中の下のフォルダ）
// ---------------------------------------------------------------------------

/// T3：走査で、下のフォルダを開く直前に外へのジャンクションに替える → link として記録する。外の名前は、
/// 履歴にも索引にも入らない。
#[test]
fn t3_scan_records_a_swapped_folder_as_a_link() {
    let dir = TestDir::new("t3");
    let (path, vault) = scan_vault(&dir);
    fs::create_dir(path.join("d")).unwrap();
    fs::write(path.join("d").join("in.txt"), "x").unwrap();
    rescan(&vault);
    let outside = outside_with_secret(&dir);
    let d = path.join("d");
    on_next(Step::BeforeChildOpen, move || {
        swap_for_junction(&d, &outside)
    });
    let result = rescan(&vault);
    assert_eq!(result["scanned"], true);
    assert_no_secret("履歴", &journal_text(&path));
    assert_no_secret("索引", &index_text(&path));
    let index: Value = serde_json::from_str(&index_text(&path)).unwrap();
    assert_eq!(index["d"]["kind"], "link", "{index}");
}

/// T3b：走査の早業：下のフォルダを開く直前にジャンクションに替え、列挙したあと（確かめる前）に元へ戻す →
/// 外の名前を記録せず、実在する d/in.txt を削除とも記録しない（開くときにリンクと見つけるので、列挙も、元へ戻す
/// 早業も起きない。直す前の形は、外を読んだあとに戻されると、d/in.txt を ExternalDelete と記録した）。
#[test]
fn t3b_scan_does_not_record_names_read_through_a_quick_swap() {
    let dir = TestDir::new("t3b");
    let (path, vault) = scan_vault(&dir);
    fs::create_dir(path.join("d")).unwrap();
    fs::write(path.join("d").join("in.txt"), "x").unwrap();
    rescan(&vault);
    let outside = outside_with_secret(&dir);
    let d = path.join("d");
    let back = d.clone();
    on_next(Step::BeforeChildOpen, move || {
        swap_for_junction(&d, &outside)
    });
    on_next(Step::AfterChildList, move || swap_back(&back));
    rescan(&vault);
    assert_no_secret("履歴", &journal_text(&path));
    assert_no_secret("索引", &index_text(&path));
    // 直す前の形の本当の不具合：外を読んだあとに戻されると、実在する d/in.txt を「消えた」と記録した
    let deleted = journal(&path)
        .iter()
        .any(|e| e.op == Op::ExternalDelete && e.path.as_deref() == Some("d/in.txt"));
    assert!(!deleted, "早業で、実在する d/in.txt を削除と記録した");
    // 新しい形は、開くときにリンクと見つけて列挙しないので、元へ戻す早業（AfterChildList）は起きない：
    // 走査のあとも d はジャンクションのままで、索引はその状態（d は link）と一致する
    let d_meta = fs::symlink_metadata(path.join("d")).unwrap();
    assert!(
        d_meta.file_type().is_symlink(),
        "外を列挙したあとに、元へ戻す早業が起きた"
    );
    let index: Value = serde_json::from_str(&index_text(&path)).unwrap();
    assert_eq!(index["d"]["kind"], "link", "{index}");
}

/// T24：走査が持っているフォルダを、同じプロセスの別の Vault で vault_move する → 3回試して BUSY。
/// 走査のあとは移せる。
#[test]
fn t24_a_folder_held_by_the_scan_cannot_be_moved_until_the_scan_ends() {
    let dir = TestDir::new("t24");
    let (path, vault) = scan_vault(&dir);
    fs::create_dir_all(path.join("d").join("e")).unwrap();
    rescan(&vault);
    let moved = slot();
    let out = moved.clone();
    let p = path.clone();
    on_next(Step::WalkHolding, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        let start = Instant::now();
        let result = call(&other, "vault_move", json!({"from": "d", "to": "d2"}));
        *out.borrow_mut() = Some((result.map_err(|e| e.code()), start.elapsed()));
    });
    rescan(&vault);
    let (result, elapsed) = moved
        .borrow_mut()
        .take()
        .expect("差し込み口が呼ばれていない");
    assert_eq!(
        result.err(),
        Some(ErrorCode::Busy),
        "走査が持っているフォルダを移せた"
    );
    assert!(
        elapsed >= Duration::from_millis(200),
        "3回試す：{elapsed:?}"
    );
    call(&vault, "vault_move", json!({"from": "d", "to": "d2"})).unwrap();
    assert!(path.join("d2").join("e").is_dir());
}

/// 走査で、下のフォルダがほかのハンドルに使用中（DELETE の権利で開かれている）なら、試し直さずに unknown にする
/// （中は索引の値をそのまま使い、削除と取り違えない）。閉じたあとの走査で見つかる。
#[test]
fn scan_treats_a_child_folder_in_use_as_unknown() {
    let dir = TestDir::new("scan-busy");
    let (path, vault) = scan_vault(&dir);
    fs::create_dir(path.join("d")).unwrap();
    fs::write(path.join("d").join("f.txt"), "x").unwrap();
    rescan(&vault);
    // 比べるための、変更のない走査の時間（同じ保管庫・同じ PC で、いちばん短いもの）
    let baseline = (0..3)
        .map(|_| {
            let start = Instant::now();
            rescan(&vault);
            start.elapsed()
        })
        .min()
        .unwrap();
    const DELETE: u32 = 0x0001_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let holder = fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(7)
        .custom_flags(BACKUP_SEMANTICS)
        .open(path.join("d"))
        .unwrap();
    fs::remove_file(path.join("d").join("f.txt")).unwrap();
    let start = Instant::now();
    rescan(&vault);
    let elapsed = start.elapsed();
    drop(holder);
    let deleted = |path: &Path| {
        journal(path)
            .iter()
            .any(|e| e.op == Op::ExternalDelete && e.path.as_deref() == Some("d/f.txt"))
    };
    assert!(!deleted(&path), "使用中のフォルダの中を、削除と記録した");
    assert!(
        index_text(&path).contains("d/f.txt"),
        "索引の値を引き継いでいない"
    );
    // 試し直すなら、使用中のフォルダで 100ms ×2 以上待つ。試し直さないので、変更のない走査の時間に近い
    assert!(
        elapsed < baseline + Duration::from_millis(150),
        "使用中のフォルダを試し直した：{elapsed:?}（変更のない走査 {baseline:?}）"
    );
    rescan(&vault);
    assert!(deleted(&path), "閉じたあとの走査で、削除が見つからない");
}

// ---------------------------------------------------------------------------
// 削除の recursive:false・ゴミ箱の一覧（T17）
// ---------------------------------------------------------------------------

/// 削除の recursive:false：門で空と確かめたあと、同じ名前の空でないフォルダに差し替える → DIR_NOT_EMPTY
/// （空かどうかは、移すハンドル（2つめの門）で列挙して決める）。何もゴミ箱へ移らない。
#[test]
fn delete_without_recursive_checks_emptiness_on_the_moved_handle() {
    let dir = TestDir::new("delete-gap");
    let (path, vault) = new_vault(&dir, "v");
    call(&vault, "vault_mkdir", json!({"path": "d"})).unwrap();
    let d = path.join("d");
    on_next(Step::AfterGate, move || {
        fs::rename(&d, d.with_file_name("d-old")).unwrap();
        fs::create_dir(&d).unwrap();
        fs::write(d.join("x.txt"), "x").unwrap();
    });
    expect_code(
        call(&vault, "vault_delete", json!({"path": "d"})),
        ErrorCode::DirNotEmpty,
        "空でないフォルダに差し替えた",
    );
    assert_eq!(
        fs::read_to_string(path.join("d").join("x.txt")).unwrap(),
        "x"
    );
    let trash = fs::read_dir(path.join(".vault").join("trash"))
        .unwrap()
        .count();
    assert_eq!(trash, 0, "ゴミ箱へ移った");
}

/// T17（ゴミ箱の一覧）：.vault/trash を開く直前に外へのジャンクションに替える → NOT_A_VAULT。外の名前を返さない。
#[test]
fn t17_trash_list_does_not_follow_a_swapped_trash_folder() {
    let dir = TestDir::new("t17-list");
    let (path, vault) = new_vault(&dir, "v");
    write(&vault, "a.txt", "a");
    call(&vault, "vault_delete", json!({"path": "a.txt"})).unwrap();
    let outside = outside_with_secret(&dir);
    let trash = path.join(".vault").join("trash");
    on_next(Step::Open(Place::Trash), move || {
        swap_for_junction(&trash, &outside)
    });
    let result = call(&vault, "vault_trash_list", json!({}));
    if let Ok(v) = &result {
        assert_no_secret("ゴミ箱の一覧", &v.to_string());
    }
    expect_code(
        result,
        ErrorCode::NotAVault,
        ".vault/trash がジャンクション",
    );
}

// ---------------------------------------------------------------------------
// M1：移す道の索引づけ
// ---------------------------------------------------------------------------

/// M1：索引にない a（外で作った a/sub/x.txt）を vault_move で b へ移す。移したあとの索引づけ（b の下を歩く）の
/// あいだに、(a) b/sub を開く直前に外へのジャンクションに替える、(b) 門のあと（AfterGate）で a/sub を外への
/// ジャンクションに替える。どちらも移動は成功し、索引・履歴に外の名前が入らず、recursive の vault_list で b/sub は
/// kind:link、外は変わらず、そのあとの vault_changes にも外の名前が出ない（歩き始めのフォルダは Target の種類の
/// 親で、項目を開くことしかできない）。
#[test]
fn m1_moving_an_unindexed_folder_does_not_follow_a_swapped_child() {
    for case in ["(a) 索引づけで b/sub を開く直前", "(b) 門のあとで a/sub"] {
        let dir = TestDir::new("m1");
        let (path, vault) = scan_vault(&dir);
        let outside = outside_with_secret(&dir);
        // 索引にない a（外で作る）
        fs::create_dir_all(path.join("a").join("sub")).unwrap();
        fs::write(path.join("a").join("sub").join("x.txt"), "x").unwrap();
        let swapped = slot();
        if case.starts_with("(a)") {
            let b_sub = path.join("b").join("sub");
            let target = outside.clone();
            let out = swapped.clone();
            on_next(Step::BeforeRename(Rename::Move), move || {
                on_next(Step::BeforeChildOpen, move || {
                    swap_for_junction(&b_sub, &target);
                    *out.borrow_mut() = Some(true);
                });
            });
        } else {
            let a_sub = path.join("a").join("sub");
            let target = outside.clone();
            let out = swapped.clone();
            on_next(Step::AfterGate, move || {
                swap_for_junction(&a_sub, &target);
                *out.borrow_mut() = Some(true);
            });
        }
        call(&vault, "vault_move", json!({"from": "a", "to": "b"}))
            .unwrap_or_else(|e| panic!("{case}：移せない：{e}"));
        assert_eq!(*swapped.borrow(), Some(true), "{case}：差し替えていない");
        assert_no_secret(&format!("{case}：索引"), &index_text(&path));
        assert_no_secret(&format!("{case}：履歴"), &journal_text(&path));
        let list = call(
            &vault,
            "vault_list",
            json!({"path": "b", "recursive": true}),
        )
        .unwrap();
        assert_no_secret(&format!("{case}：一覧"), &list.to_string());
        assert!(
            listed(&list).contains(&("b/sub".to_owned(), "link".to_owned())),
            "{case}：b/sub がリンクでない：{:?}",
            listed(&list)
        );
        let names: Vec<String> = fs::read_dir(&outside)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["SECRET-NAME.txt"], "{case}：外が変わった");
        assert_eq!(
            fs::read_to_string(outside.join("SECRET-NAME.txt")).unwrap(),
            "OUTSIDE"
        );
        let changes = rescan(&vault);
        assert_no_secret(&format!("{case}：変更の一覧"), &changes.to_string());
        assert_no_secret(&format!("{case}：走査のあとの索引"), &index_text(&path));
    }
}
