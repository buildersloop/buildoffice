//! init の作る処理と片づけ（I1〜I11・F10・F11）。
//!
//! - 片づけで消すのは、この init が作ったもので、場所と ID が記録と一致するものだけ（guard の InitScope）。
//!   ほかの処理が差し替えたもの・中に置いたものは消さず、もとのエラーに「片づけで残したもの」の注を足す。
//! - s1_…〜s8_… のテストは、片づけの最中に、外のもの・ほかの処理のものを差し替えられた場合を試す。
//! - init を、索引・履歴・lock を作ったあとで失敗させるには、2回目の `.vault/tmp` を開く所（vault.json の一時
//!   ファイル）で `.vault/tmp` に (WD)（ファイルを作れない）を付ける。

#![cfg(windows)]

mod common;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Barrier;

use common::{DenyAce, TestDir, actor, assert_no_absolute_path, env, junction};
use vault_core::test_support::{
    FinalAt, Place, Removing, Step, fake_final_path, on_next, root_env_from_vars,
};
use vault_core::{ErrorCode, Vault, VaultError};

/// 2回目の `.vault/tmp` を開く所（vault.json の一時ファイル）で、`.vault/tmp` に (WD) を付けて init を失敗させる。
/// 返した入れ物が、付けた拒否を持つ（テストの終わりで外れる）。
fn fail_at_vault_json(root: &Path) -> Rc<RefCell<Option<DenyAce>>> {
    let deny: Rc<RefCell<Option<DenyAce>>> = Rc::new(RefCell::new(None));
    on_next(Step::Open(Place::Tmp), || {});
    let slot = deny.clone();
    let tmp = root.join(".vault").join("tmp");
    on_next(Step::Open(Place::Tmp), move || {
        *slot.borrow_mut() = Some(DenyAce::new(&tmp, "(WD)"));
    });
    deny
}

/// フォルダの中の名前（並べ替えたもの）。ないフォルダなら None。
fn names(dir: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .ok()?
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Some(names)
}

/// 外のフォルダ（中に、init が作るものと同じ名前のもの）。
fn outside_like_a_vault(dir: &TestDir, name: &str) -> PathBuf {
    let outside = dir.join(name);
    for sub in ["trash", "journal", "tmp"] {
        fs::create_dir_all(outside.join(sub)).unwrap();
    }
    for file in ["lock", "index.json", "vault.json"] {
        fs::write(outside.join(file), "OUTSIDE").unwrap();
    }
    outside
}

fn expect_err(result: Result<(), VaultError>, code: ErrorCode, what: &str) -> VaultError {
    let e = result.expect_err(what);
    assert_eq!(e.code(), code, "{what}：{}", e.message());
    assert_no_absolute_path(&e);
    e
}

/// I1（緑）：あるフォルダ p への init を vault.json の一時ファイルで失敗させる → p は残り、中は空。
#[test]
fn i1_failing_init_into_an_existing_folder_leaves_it_empty() {
    let dir = TestDir::new("i1");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let _deny = fail_at_vault_json(&p);
    expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::IoError,
        "vault.json を書けない",
    );
    assert_eq!(names(&p), Some(vec![]), "p の中が空でない");
}

/// I1b（緑）：p/new/v への init を同じく失敗させる → new と v は消え、p は残る。
#[test]
fn i1b_failing_init_removes_the_folders_it_created() {
    let dir = TestDir::new("i1b");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let target = p.join("new").join("v");
    let _deny = fail_at_vault_json(&target);
    expect_err(
        Vault::init(&target, &env(), &actor()),
        ErrorCode::IoError,
        "vault.json を書けない",
    );
    assert_eq!(names(&p), Some(vec![]), "new が残った");
}

/// I2（赤）：片づけの最初（BeforeInitUndo）に、.vault を .vault-real に替えて、外へのジャンクションを置こうとする
/// → 名前の変更は Err（init が .vault を持っている）。中は全部消え、外はそのまま（以前は、パスで消すので、
/// ジャンクションの先の外の lock・index.json などを消していた）。
#[test]
fn i2_swapping_dot_vault_before_undo_does_not_delete_outside() {
    let dir = TestDir::new("i2");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let outside = outside_like_a_vault(&dir, "outside");
    let before = names(&outside);
    let _deny = fail_at_vault_json(&p);
    let renamed = Rc::new(RefCell::new(None));
    let out = renamed.clone();
    let (vault_dir, real, target) = (p.join(".vault"), p.join(".vault-real"), outside.clone());
    on_next(Step::BeforeInitUndo, move || {
        let ok = fs::rename(&vault_dir, &real).is_ok();
        if ok {
            assert!(junction(&vault_dir, &target), "ジャンクションを作れない");
        }
        *out.borrow_mut() = Some(ok);
    });
    expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::IoError,
        "vault.json を書けない",
    );
    assert_eq!(names(&outside), before, "外のものが消えた");
    for file in ["lock", "index.json", "vault.json"] {
        assert_eq!(fs::read_to_string(outside.join(file)).unwrap(), "OUTSIDE");
    }
    assert_eq!(*renamed.borrow(), Some(false), ".vault の名前を変えられた");
    assert_eq!(names(&p), Some(vec![]), "p の中が空でない");
}

/// I2b：差し替えが成功する時点の版。片づけで .vault を消す直前（BeforeInitRemove(VaultDir)）に .vault を
/// 外へのジャンクションに替える、p/new/v の2回目の CreatedDir（new を消す直前）に new を外へのジャンクションに替える
/// → どちらもリンクなので消さず、外は変わらない。
#[test]
fn i2b_links_swapped_in_just_before_removal_are_not_followed() {
    let dir = TestDir::new("i2b");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let outside = outside_like_a_vault(&dir, "outside");
    let outside2 = dir.join("outside2");
    fs::create_dir_all(outside2.join("v")).unwrap();
    let before = (names(&outside), names(&outside2));
    let target = p.join("new").join("v");
    let _deny = fail_at_vault_json(&target);
    let (vault_dir, real, out1) = (
        target.join(".vault"),
        target.join(".vault-real"),
        outside.clone(),
    );
    on_next(Step::BeforeInitRemove(Removing::VaultDir), move || {
        if fs::rename(&vault_dir, &real).is_ok() {
            assert!(junction(&vault_dir, &out1), "ジャンクションを作れない");
        }
    });
    on_next(Step::BeforeInitRemove(Removing::CreatedDir), || {});
    let (new, new_real, out2) = (p.join("new"), p.join("new-real"), outside2.clone());
    on_next(Step::BeforeInitRemove(Removing::CreatedDir), move || {
        if fs::rename(&new, &new_real).is_ok() {
            assert!(junction(&new, &out2), "ジャンクションを作れない");
        }
    });
    let _ = Vault::init(&target, &env(), &actor()).expect_err("vault.json を書けない");
    assert_eq!((names(&outside), names(&outside2)), before, "外が変わった");
}

/// I3（赤）：.vault を消す直前（BeforeInitRemove(VaultDir)）に、.vault を .vault-ours に替え、同じ名前の空のフォルダを
/// 置く → そのフォルダ（ほかの処理のもの）は消えない。.vault-ours も残り、注に出る。もとの IO_ERROR が残る。
#[test]
fn i3_a_dot_vault_put_by_another_process_is_not_removed() {
    let dir = TestDir::new("i3");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let _deny = fail_at_vault_json(&p);
    let (vault_dir, ours) = (p.join(".vault"), p.join(".vault-ours"));
    on_next(Step::BeforeInitRemove(Removing::VaultDir), move || {
        fs::rename(&vault_dir, &ours).unwrap();
        // ほかの処理の、空のフォルダ（以前は、パスで消すので消していた）
        fs::create_dir(&vault_dir).unwrap();
    });
    let e = expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert!(p.join(".vault").is_dir(), "ほかの処理の .vault を消した");
    assert!(p.join(".vault-ours").is_dir(), ".vault-ours は残る");
    assert!(
        e.message().contains("片づけで残したもの"),
        "注がない：{}",
        e.message()
    );
}

/// I4（赤）：p/new/v で、片づけの最初の CreatedDir（v を消す直前）に、new を外へのジャンクションに替えようとする
/// → 名前の変更は Err（init が new を持っている）。外の v は残る（以前は、パスで消すので外の v を消していた）。
#[test]
fn i4_swapping_a_created_parent_does_not_delete_outside() {
    let dir = TestDir::new("i4");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let outside = dir.join("outside");
    fs::create_dir_all(outside.join("v")).unwrap();
    let target = p.join("new").join("v");
    let _deny = fail_at_vault_json(&target);
    let renamed = Rc::new(RefCell::new(None));
    let out = renamed.clone();
    let (new, moved, target_outside) = (p.join("new"), p.join("new-moved"), outside.clone());
    on_next(Step::BeforeInitRemove(Removing::CreatedDir), move || {
        let ok = fs::rename(&new, &moved).is_ok();
        if ok {
            assert!(junction(&new, &target_outside), "ジャンクションを作れない");
        }
        *out.borrow_mut() = Some(ok);
    });
    let _ = Vault::init(&target, &env(), &actor()).expect_err("vault.json を書けない");
    assert!(outside.join("v").is_dir(), "外の v を消した");
    assert_eq!(*renamed.borrow(), Some(false), "new の名前を変えられた");
}

/// I5（緑。設計の決定：init_open.rs の undo_keeps_files_that_others_put_in_created_folders の Windows の版）：
/// 片づけの最初に p/new/foreign.txt を書く → v と .vault は消え、new と foreign.txt は残る。p は残る。エラーの
/// コードはもとのまま。注に new が相対の名前で出る。
#[test]
fn i5_files_put_by_others_in_created_folders_are_kept() {
    let dir = TestDir::new("i5");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let target = p.join("new").join("v");
    let _deny = fail_at_vault_json(&target);
    let foreign = p.join("new").join("foreign.txt");
    let f = foreign.clone();
    on_next(Step::BeforeInitUndo, move || fs::write(&f, "keep").unwrap());
    let e = expect_err(
        Vault::init(&target, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_eq!(fs::read_to_string(&foreign).unwrap(), "keep");
    assert!(!target.exists(), "v が残った");
    assert_eq!(names(&p.join("new")), Some(vec!["foreign.txt".to_owned()]));
    assert!(p.is_dir());
    assert!(
        e.message().contains("片づけで残したもの") && e.message().contains("new"),
        "注に new がない：{}",
        e.message()
    );
}

/// I7：片づけで残したものの注は、いちばん深いある祖先（p）から見た相対の名前だけで書く。p/new/v の片づけの最初に
/// v の中へファイルを置く → v と new が残り、注は「片づけで残したもの：new/v・new（…）」。絶対パス・p の名前・
/// 置いたファイルの名前を書かない。エラーのコードはもとのまま。
#[test]
fn i7_the_undo_note_has_only_relative_names() {
    let dir = TestDir::new("i7");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let target = p.join("new").join("v");
    let _deny = fail_at_vault_json(&target);
    let foreign = target.join("foreign-i7.txt");
    let f = foreign.clone();
    on_next(Step::BeforeInitUndo, move || fs::write(&f, "keep").unwrap());
    let e = expect_err(
        Vault::init(&target, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_eq!(fs::read_to_string(&foreign).unwrap(), "keep");
    assert_eq!(names(&target), Some(vec!["foreign-i7.txt".to_owned()]));
    let message = e.message();
    assert!(
        message.contains(
            "片づけで残したもの：new/v・new（ほかの処理が変えた・中に何かある。中を確かめてから消す）"
        ),
        "注の形が違う：{message}"
    );
    let p_name = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for not_expected in [p_name.as_str(), "foreign-i7", ":\\", "\\\\?\\"] {
        assert!(
            !message.contains(not_expected),
            "注に {not_expected} がある：{message}"
        );
    }
}

/// I6（赤）：片づけの最初に .vault/lock を lock-ours に替え、新しい lock（ほかの処理のもの）を作る → 新しい lock は
/// 消えない。もとの IO_ERROR が残り、注に出る。
#[test]
fn i6_a_lock_put_by_another_process_is_not_removed() {
    let dir = TestDir::new("i6");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let _deny = fail_at_vault_json(&p);
    let (lock, ours) = (
        p.join(".vault").join("lock"),
        p.join(".vault").join("lock-ours"),
    );
    on_next(Step::BeforeInitUndo, move || {
        fs::rename(&lock, &ours).unwrap();
        fs::write(&lock, "theirs").unwrap();
    });
    let e = expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_eq!(
        fs::read_to_string(p.join(".vault").join("lock")).unwrap(),
        "theirs",
        "ほかの処理の lock を消した"
    );
    assert!(
        e.message().contains("片づけで残したもの"),
        "注がない：{}",
        e.message()
    );
}

/// I8（緑）：2つのスレッドで、同じ p/new/v に 20 回 init する → どの回も成功は1つだけで、負けたほうの片づけが
/// 勝ったほうのものを消さない（勝ったほうの保管庫は開ける）。
#[test]
fn i8_concurrent_inits_never_remove_the_winner() {
    let dir = TestDir::new("i8");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let target = p.join("new").join("v");
    for round in 0..20 {
        let barrier = Barrier::new(2);
        let results: Vec<Result<(), VaultError>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        barrier.wait();
                        Vault::init(&target, &env(), &actor())
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let ok = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok, 1, "{round} 回目：成功が1つでない：{results:?}");
        Vault::open(&target, false, &env())
            .unwrap_or_else(|e| panic!("{round} 回目：勝ったほうの保管庫を開けない：{e}"));
        fs::remove_dir_all(p.join("new")).unwrap();
    }
}

/// 偽のホーム（テスト用のフォルダの下）だけを知る RootEnv。`<ホーム>\AppData\Roaming` などが守る場所になる。
fn fake_home_env(home: &Path) -> vault_core::RootEnv {
    root_env_from_vars(&[("USERPROFILE", home)]).expect("偽のホームで RootEnv を作れない")
}

/// I12：守る場所の中で、途中がファイルのパス（`<ホーム>\AppData\Roaming\f.txt\v`）への
/// init は、根の判定の INVALID_ROOT になり、何も作らない（「途中がファイル」のエラーは、根の判定のあとで返す。
#[test]
fn i12_a_file_ancestor_in_a_protected_place_is_invalid_root() {
    let dir = TestDir::new("i12");
    let home = dir.join("home");
    let roaming = home.join("AppData").join("Roaming");
    fs::create_dir_all(&roaming).unwrap();
    fs::write(roaming.join("f.txt"), "file").unwrap();
    let e = expect_err(
        Vault::init(
            &roaming.join("f.txt").join("v"),
            &fake_home_env(&home),
            &actor(),
        ),
        ErrorCode::InvalidRoot,
        "守る場所の中の、途中がファイルのパス",
    );
    assert!(
        !e.message().contains("フォルダでないもの") && !e.message().contains("作れない"),
        "根の判定の文でない：{}",
        e.message()
    );
    assert_eq!(names(&roaming), Some(vec!["f.txt".to_owned()]));
    assert_eq!(fs::read_to_string(roaming.join("f.txt")).unwrap(), "file");
}

/// I13：守る場所でない所で、途中がファイルのパス（`p\f.txt\sub\v`）への init は、今までどおり
/// IO_ERROR で、何も作らない。
#[test]
fn i13_a_file_ancestor_elsewhere_is_io_error() {
    let dir = TestDir::new("i13");
    let home = dir.join("home");
    fs::create_dir(&home).unwrap();
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    fs::write(p.join("f.txt"), "file").unwrap();
    expect_err(
        Vault::init(
            &p.join("f.txt").join("sub").join("v"),
            &fake_home_env(&home),
            &actor(),
        ),
        ErrorCode::IoError,
        "途中がファイルのパス",
    );
    assert_eq!(names(&p), Some(vec!["f.txt".to_owned()]));
    assert_eq!(fs::read_to_string(p.join("f.txt")).unwrap(), "file");
}

/// I14：保管庫にするパスそのものが、先のないジャンクション（宙ぶらりんのリンク）なら、
/// 今までどおり INVALID_ROOT「フォルダでないものは保管庫にできない」。リンクも、リンクの先も作らない・消さない。
#[test]
fn i14_a_dangling_junction_is_not_a_folder() {
    let dir = TestDir::new("i14");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let gone = dir.join("gone");
    fs::create_dir(&gone).unwrap();
    let v = p.join("v");
    assert!(junction(&v, &gone));
    fs::remove_dir(&gone).unwrap();
    let e = expect_err(
        Vault::init(&v, &env(), &actor()),
        ErrorCode::InvalidRoot,
        "先のないジャンクション",
    );
    assert!(
        e.message().contains("フォルダでないものは保管庫にできない"),
        "文が違う：{}",
        e.message()
    );
    assert!(fs::symlink_metadata(&v).unwrap().file_type().is_symlink());
    assert!(!gone.exists(), "リンクの先ができた");
    assert_eq!(names(&p), Some(vec!["v".to_owned()]));
}

/// I9：init のパスの名前に `:` などがあると INVALID_ROOT で、何も作らない。
#[test]
fn i9_names_with_invalid_characters_are_invalid_root() {
    let dir = TestDir::new("i9");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    for bad in ["new:x", "a*b", "q?"] {
        expect_err(
            Vault::init(&p.join(bad).join("v"), &env(), &actor()),
            ErrorCode::InvalidRoot,
            bad,
        );
    }
    assert_eq!(names(&p), Some(vec![]), "何かを作った");
}

/// I10：init の途中で .vault を作って閉じたあと（AfterCreatedClosed）に、本当に差し替える（作った
/// .vault を .vault-ours に替え、同じ名前の別のフォルダを置く）→ NOT_A_VAULT。差し替えたフォルダは消えない。
#[test]
fn i10_a_swapped_dot_vault_is_not_a_vault_and_is_kept() {
    let dir = TestDir::new("i10");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let (vault_dir, ours) = (p.join(".vault"), p.join(".vault-ours"));
    on_next(Step::AfterCreatedClosed, move || {
        fs::rename(&vault_dir, &ours).unwrap();
        // ほかの処理の、空のフォルダ
        fs::create_dir(&vault_dir).unwrap();
    });
    expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::NotAVault,
        ".vault を差し替えた",
    );
    assert!(p.join(".vault").is_dir(), "差し替えたフォルダを消した");
}

/// I11：init が作った保管庫のフォルダ（tail）を閉じたあと（AfterCreatedClosed）に、本当に差し替える
/// → CONFLICT。差し替えたフォルダは消えない。
#[test]
fn i11_a_swapped_created_folder_is_a_conflict_and_is_kept() {
    let dir = TestDir::new("i11");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let target = p.join("v");
    let (v, ours) = (target.clone(), p.join("v-ours"));
    on_next(Step::AfterCreatedClosed, move || {
        fs::rename(&v, &ours).unwrap();
        // ほかの処理の、空のフォルダ
        fs::create_dir(&v).unwrap();
    });
    expect_err(
        Vault::init(&target, &env(), &actor()),
        ErrorCode::Conflict,
        "作ったフォルダを差し替えた",
    );
    assert!(target.is_dir(), "差し替えたフォルダを消した");
}

/// F10：作ったフォルダ（tail の最初の段）の最終のパスを外と判定させる → CONFLICT。作った段は消え、p は残る。
#[test]
fn f10_a_created_folder_found_outside_is_a_conflict() {
    let dir = TestDir::new("f10");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let elsewhere = dir.join("elsewhere").join("new");
    fake_final_path(FinalAt::Created, move |_| elsewhere);
    expect_err(
        Vault::init(&p.join("new").join("v"), &env(), &actor()),
        ErrorCode::Conflict,
        "作ったフォルダが外",
    );
    assert_eq!(names(&p), Some(vec![]), "作った段が残った");
}

/// F11：作って開き直したフォルダ（tail の最初の段）の最終のパスを外と判定させる → CONFLICT。片づけで消える。
#[test]
fn f11_a_reopened_folder_found_outside_is_a_conflict() {
    let dir = TestDir::new("f11");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let elsewhere = dir.join("elsewhere").join("new");
    fake_final_path(FinalAt::Reopened, move |_| elsewhere);
    expect_err(
        Vault::init(&p.join("new").join("v"), &env(), &actor()),
        ErrorCode::Conflict,
        "開き直したフォルダが外",
    );
    assert_eq!(names(&p), Some(vec![]), "作った段が残った");
}

// ---------------------------------------------------------------------------
// 差し替えのテスト（s1_…〜s8_…）。差し替えの時点と種類を変えても、外のもの・ほかの
// 処理のものは消えず、注に出る。どの差し替えも、本当に起きたことを確かめる（起きなければテストが落ちる）。
// ---------------------------------------------------------------------------

/// 注の「片づけで残したもの：…（」の中身が `left` であること。
fn assert_left(e: &VaultError, left: &str) {
    assert!(
        e.message()
            .contains(&format!("片づけで残したもの：{left}（")),
        "注が違う（{left} のはず）：{}",
        e.message()
    );
}

/// 片づけで何も残さなかった（注がない）こと。
fn assert_no_note(e: &VaultError) {
    assert!(
        !e.message().contains("片づけで残したもの"),
        "注がある：{}",
        e.message()
    );
}

/// フォルダ `dir` の中の `name` を `keep_as` に名前を変える（ほかの処理が、こちらのものを横へ動かす）。
fn move_aside(dir: &Path, name: &str, keep_as: &str) {
    fs::rename(dir.join(name), dir.join(keep_as))
        .unwrap_or_else(|e| panic!("{name} の名前を変えられない：{e}"));
}

/// 片づけの最初（BeforeInitUndo）に、.vault/tmp を外の空のフォルダへのジャンクションに、.vault/trash をほかの
/// 空のフォルダに差し替える → 外のフォルダもほかのフォルダも消えない。もとの IO_ERROR に、tmp・trash・.vault が出る。
#[test]
fn s1_swapped_tmp_and_trash_are_kept() {
    let dir = TestDir::new("s1");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let outside = dir.join("outside-empty");
    fs::create_dir(&outside).unwrap();
    let _deny = fail_at_vault_json(&p);
    let (v, o) = (p.join(".vault"), outside.clone());
    on_next(Step::BeforeInitUndo, move || {
        move_aside(&v, "tmp", "tmp-ours");
        assert!(junction(&v.join("tmp"), &o));
        move_aside(&v, "trash", "trash-ours");
        fs::create_dir(v.join("trash")).unwrap();
    });
    let e = expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_left(&e, ".vault/tmp・.vault/trash・.vault");
    assert_eq!(names(&outside), Some(vec![]), "外のフォルダが変わった");
    let vault = p.join(".vault");
    assert!(
        fs::symlink_metadata(vault.join("tmp"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(vault.join("trash").is_dir(), "ほかの trash が消えた");
}

/// 片づけの最初に、.vault/journal を、同じ名前の履歴のファイルを持つ外のフォルダへのジャンクションに差し替える
/// → 外の履歴のファイルは消えない。注に、履歴のファイル・journal・.vault が出る。
#[test]
fn s2_a_journal_swapped_to_outside_is_kept() {
    let dir = TestDir::new("s2");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let outside = dir.join("outside-j");
    fs::create_dir(&outside).unwrap();
    let _deny = fail_at_vault_json(&p);
    let (v, o) = (p.join(".vault"), outside.clone());
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let slot = seen.clone();
    on_next(Step::BeforeInitUndo, move || {
        let files = names(&v.join("journal")).unwrap();
        for f in &files {
            fs::write(o.join(f), "OUTSIDE").unwrap();
        }
        move_aside(&v, "journal", "journal-ours");
        assert!(junction(&v.join("journal"), &o));
        *slot.borrow_mut() = files;
    });
    let e = expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    let files = seen.borrow().clone();
    assert_eq!(files.len(), 1, "履歴のファイルが1つでない：{files:?}");
    assert_left(
        &e,
        &format!(".vault/journal/{}・.vault/journal・.vault", files[0]),
    );
    assert_eq!(names(&outside), Some(files.clone()));
    assert_eq!(
        fs::read_to_string(outside.join(&files[0])).unwrap(),
        "OUTSIDE"
    );
}

/// 片づけの最初に、.vault/index.json を外のファイルのハードリンクに、.vault/lock を外のフォルダへのジャンクションに
/// 差し替える → 外のファイルとフォルダは消えない。ハードリンクも消さない。注に index.json・lock・.vault が出る。
#[test]
fn s3_a_hard_linked_index_and_a_lock_junction_are_kept() {
    let dir = TestDir::new("s3");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let outside_file = dir.join("outside.txt");
    fs::write(&outside_file, "OUTSIDE").unwrap();
    let outside_dir = dir.join("outside-d");
    fs::create_dir(&outside_dir).unwrap();
    let _deny = fail_at_vault_json(&p);
    let (v, of, od) = (p.join(".vault"), outside_file.clone(), outside_dir.clone());
    on_next(Step::BeforeInitUndo, move || {
        move_aside(&v, "index.json", "index-ours");
        fs::hard_link(&of, v.join("index.json")).unwrap();
        move_aside(&v, "lock", "lock-ours");
        assert!(junction(&v.join("lock"), &od));
    });
    let e = expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_left(&e, ".vault/index.json・.vault/lock・.vault");
    assert_eq!(fs::read_to_string(&outside_file).unwrap(), "OUTSIDE");
    assert_eq!(
        fs::read_to_string(p.join(".vault").join("index.json")).unwrap(),
        "OUTSIDE",
        "ハードリンクが消えた"
    );
    assert_eq!(names(&outside_dir), Some(vec![]), "外のフォルダが変わった");
}

/// .vault を消す直前（BeforeInitRemove(VaultDir)）に、.vault をファイルに差し替える → ファイルは消えない。
/// 注に .vault が出る。
#[test]
fn s4_a_dot_vault_swapped_to_a_file_is_kept() {
    let dir = TestDir::new("s4");
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let _deny = fail_at_vault_json(&p);
    let pp = p.clone();
    on_next(Step::BeforeInitRemove(Removing::VaultDir), move || {
        move_aside(&pp, ".vault", ".vault-ours");
        fs::write(pp.join(".vault"), "THEIRS").unwrap();
    });
    let e = expect_err(
        Vault::init(&p, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_left(&e, ".vault");
    assert_eq!(fs::read_to_string(p.join(".vault")).unwrap(), "THEIRS");
    assert_eq!(names(&p.join(".vault-ours")), Some(vec![]));
}

/// s5_0〜s5_2 の本体：p/new/v で、片づけの最初の CreatedDir（v を消す直前）に、v を差し替える（0＝外の空のフォルダへの
/// ジャンクション、1＝別の空のフォルダ、2＝ファイル）→ 差し替えたものも外のフォルダも消えない。注に new/v・new。
fn s5_case(kind: u8) {
    let dir = TestDir::new(&format!("s5-{kind}"));
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let outside = dir.join("outside-e");
    fs::create_dir(&outside).unwrap();
    let target = p.join("new").join("v");
    let _deny = fail_at_vault_json(&target);
    let (new, o) = (p.join("new"), outside.clone());
    on_next(Step::BeforeInitRemove(Removing::CreatedDir), move || {
        move_aside(&new, "v", "v-ours");
        let v = new.join("v");
        match kind {
            0 => assert!(junction(&v, &o)),
            1 => fs::create_dir(&v).unwrap(),
            _ => fs::write(&v, "THEIRS").unwrap(),
        }
    });
    let e = expect_err(
        Vault::init(&target, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_left(&e, "new/v・new");
    assert_eq!(names(&outside), Some(vec![]), "外のフォルダが変わった");
    let meta = fs::symlink_metadata(&target).expect("差し替えた v が消えた");
    match kind {
        0 => assert!(meta.file_type().is_symlink()),
        1 => assert!(meta.is_dir() && names(&target) == Some(vec![])),
        _ => assert_eq!(fs::read_to_string(&target).unwrap(), "THEIRS"),
    }
}

#[test]
fn s5_0_a_created_folder_swapped_to_a_junction_is_kept() {
    s5_case(0)
}

#[test]
fn s5_1_a_created_folder_swapped_to_another_folder_is_kept() {
    s5_case(1)
}

#[test]
fn s5_2_a_created_folder_swapped_to_a_file_is_kept() {
    s5_case(2)
}

/// s6_0・s6_1 の本体：p/new/v で、片づけの2つめの CreatedDir（new を消す直前）に、new を差し替える（0＝別の空のフォルダ、
/// 1＝ファイル）→ 差し替えたものは消えない。注に new。
fn s6_case(kind: u8) {
    let dir = TestDir::new(&format!("s6-{kind}"));
    let p = dir.join("p");
    fs::create_dir(&p).unwrap();
    let target = p.join("new").join("v");
    let _deny = fail_at_vault_json(&target);
    on_next(Step::BeforeInitRemove(Removing::CreatedDir), || {});
    let pp = p.clone();
    on_next(Step::BeforeInitRemove(Removing::CreatedDir), move || {
        move_aside(&pp, "new", "new-ours");
        let new = pp.join("new");
        if kind == 0 {
            fs::create_dir(&new).unwrap();
        } else {
            fs::write(&new, "THEIRS").unwrap();
        }
    });
    let e = expect_err(
        Vault::init(&target, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_left(&e, "new");
    let new = p.join("new");
    if kind == 0 {
        assert_eq!(names(&new), Some(vec![]), "ほかの new が変わった");
    } else {
        assert_eq!(fs::read_to_string(&new).unwrap(), "THEIRS");
    }
    assert_eq!(names(&p.join("new-ours")), Some(vec![]));
}

#[test]
fn s6_0_a_created_parent_swapped_to_another_folder_is_kept() {
    s6_case(0)
}

#[test]
fn s6_1_a_created_parent_swapped_to_a_file_is_kept() {
    s6_case(1)
}

/// base（いちばん深いある祖先）がジャンクション（plink → real）で、片づけの最初に、その向き先を外（中に new/v が
/// ある）へ変える → 片づけは、持っている base（real）を基準に進むので、real の中は空に戻り、外の new/v は消えない。
/// 注はない。
#[test]
fn s7_a_repointed_base_junction_does_not_redirect_the_undo() {
    let dir = TestDir::new("s7");
    let real = dir.join("real");
    fs::create_dir(&real).unwrap();
    let outside = dir.join("outside");
    fs::create_dir_all(outside.join("new").join("v")).unwrap();
    let plink = dir.join("plink");
    assert!(junction(&plink, &real));
    let _deny = fail_at_vault_json(&real.join("new").join("v"));
    let (pl, o) = (plink.clone(), outside.clone());
    on_next(Step::BeforeInitUndo, move || {
        fs::remove_dir(&pl).unwrap();
        assert!(junction(&pl, &o));
    });
    let e = expect_err(
        Vault::init(&plink.join("new").join("v"), &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    assert_no_note(&e);
    // 差し替えが起きた（plink から外の new\v が見える）
    assert!(
        plink.join("new").join("v").is_dir(),
        "plink の向き先が変わっていない"
    );
    assert_eq!(names(&real), Some(vec![]), "real が片づいていない");
    assert_eq!(names(&outside.join("new").join("v")), Some(vec![]));
}

/// 片づけの最初に、base（p）の祖先（a）の名前を変えようとする → 持っているハンドルのため、名前の変更は拒否（5）で
/// 止まり、片づけは最後まで進む（p の中は空に戻り、注はない）。
#[test]
fn s8_an_ancestor_of_the_base_cannot_be_renamed_during_undo() {
    let dir = TestDir::new("s8");
    let a = dir.join("a");
    let p = a.join("p");
    fs::create_dir_all(&p).unwrap();
    let target = p.join("new").join("v");
    let _deny = fail_at_vault_json(&target);
    let renamed: Rc<RefCell<Option<std::io::Result<()>>>> = Rc::new(RefCell::new(None));
    let slot = renamed.clone();
    let (from, to) = (a.clone(), dir.join("a2"));
    on_next(Step::BeforeInitUndo, move || {
        *slot.borrow_mut() = Some(fs::rename(&from, &to));
    });
    let e = expect_err(
        Vault::init(&target, &env(), &actor()),
        ErrorCode::IoError,
        "もとの IO_ERROR",
    );
    let code = renamed
        .borrow()
        .as_ref()
        .expect("差し込みが呼ばれていない")
        .as_ref()
        .err()
        .and_then(std::io::Error::raw_os_error);
    assert_eq!(code, Some(5), "祖先の名前を変えられてしまった");
    assert_no_note(&e);
    assert_eq!(names(&p), Some(vec![]), "p が片づいていない");
}
