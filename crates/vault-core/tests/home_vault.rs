//! 既定の保管庫：`open_or_create_in`。偽のホームは `TestDir` の下に置く。
//!
//! 本物のホームでは、どのテストも作らず、開かない。本物の場所を置き場所に使うのは、根の判定で断られる「調べるだけ」の
//! 道（%LOCALAPPDATA% と、ホームの直下の `.` から始まる名前）だけ。
//!
//! - 作る・開く・断る場合ごとのテスト（関数名は `h1_…`〜`h11_…`）。どのエラーでも、文に絶対パス・ユーザー名・`\\?\`
//!   が入らないことを確かめる（名前は対象外）
//! - 確かめのあとに場所を差し替える（差し込み口 `Step::AfterDefaultVaultCheck`。関数名は `j1_…`）
//! - ハンドルの最終のパスで比べる（8.3 の名前の偽のホーム、ホームそのものがジャンクション。関数名は `j2_…`）
//! - 2つのスレッドが同時に呼ぶ（関数名は `j7_…`）
//! - 消したあとにまた作る、`.vault` だけ、UNC のホーム、空白と日本語のホーム（関数名は `j8_…`）
//! - 権限で作れない道（init の片づけ）は、init のテスト（tests/init_undo.rs・tests/acl.rs）で見ている

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use common::{TestDir, actor, assert_no_absolute_path, call, env, journal, new_vault, skip};
use serde_json::json;
use vault_core::{ActorKind, ErrorCode, HomeVault, Op, Vault, VaultError, open_or_create_in};

/// 既定の保管庫の名前（アプリと同じ名前。vault-core は名前を持たないので、テストで渡す）。
const NAME: &str = "Buildoffice";

/// 偽のホーム（TestDir の下の空のフォルダ）。
fn fake_home(dir: &TestDir) -> PathBuf {
    let home = dir.join("home");
    fs::create_dir(&home).unwrap();
    home
}

fn open_default(home: &Path) -> Result<HomeVault, VaultError> {
    open_or_create_in(home, NAME, &env(), &actor())
}

/// フォルダの中の名前（並べ替えたもの）。ないフォルダなら空。
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// 文に、偽のホームの絶対パス・テストの場所・ユーザー名・`\\?\` が入らない（保管庫の名前は対象外）。
fn assert_private(e: &VaultError, home: &Path) {
    assert_no_absolute_path(e);
    let text = e.to_string();
    let mut needles = vec![
        home.display().to_string(),
        home.display().to_string().replace('\\', "/"),
        r"\\?\".to_owned(),
    ];
    if let Ok(canon) = fs::canonicalize(home) {
        needles.push(canon.display().to_string());
    }
    for var in ["USERNAME", "USER"] {
        if let Ok(name) = std::env::var(var)
            && name.len() >= 3
        {
            needles.push(name);
        }
    }
    for needle in needles {
        assert!(
            !text.contains(&needle),
            "エラーの文に出てはいけないもの：{text}"
        );
    }
}

/// エラーのコードを確かめ、文に絶対パスなどがないことも確かめる。
fn expect_err(
    result: Result<HomeVault, VaultError>,
    code: ErrorCode,
    home: &Path,
    what: &str,
) -> VaultError {
    let e = match result {
        Ok(opened) => panic!(
            "{what}: {code} になるべきところが成功した（created {}）",
            opened.created
        ),
        Err(e) => e,
    };
    assert_eq!(e.code(), code, "{what}: {e}");
    assert_private(&e, home);
    e
}

/// リンクの判定で断った（INVALID_ROOT で、リンクをたどらないという文）。期待する根の確かめでも INVALID_ROOT になるので、
/// どちらで断ったかを文で見分ける（リンクの判定を外すと、このテストが落ちる）。
fn expect_link_refused(result: Result<HomeVault, VaultError>, home: &Path, what: &str) {
    let e = expect_err(result, ErrorCode::InvalidRoot, home, what);
    assert!(e.message().contains("たどらない"), "{what}: {e}");
}

/// 置き場所の確かめ（check_parent_dir）で断った（INVALID_ROOT「置き場所のフォルダがない」）。
fn expect_missing_parent(result: Result<HomeVault, VaultError>, home: &Path, what: &str) {
    let e = expect_err(result, ErrorCode::InvalidRoot, home, what);
    assert!(
        e.message().contains("置き場所のフォルダがない"),
        "{what}: {e}"
    );
}

/// 確かめたあとの差し替えに気づいた（INVALID_ROOT「確かめたあとで変わった」）。
fn expect_moved(result: Result<HomeVault, VaultError>, home: &Path, what: &str) {
    let e = expect_err(result, ErrorCode::InvalidRoot, home, what);
    assert!(
        e.message().contains("確かめたあとで変わった"),
        "{what}: {e}"
    );
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// ない → 作って書ける状態で開く（created:true）。.vault の形・履歴の1行目が init・actor・名前。
#[test]
fn h1_a_missing_folder_is_created_and_opened_for_writing() {
    let dir = TestDir::new("hv-h1");
    let home = fake_home(&dir);
    let opened = open_default(&home).unwrap();
    assert!(opened.created);
    let vault = &opened.vault;
    assert_eq!(vault.name(), NAME);
    assert!(!vault.is_read_only());
    let path = home.join(NAME);
    assert_eq!(
        names_in(&path.join(".vault")),
        [
            "index.json",
            "journal",
            "lock",
            "tmp",
            "trash",
            "vault.json"
        ]
    );
    assert_eq!(names_in(&path), [".vault"]);
    let entries = journal(&path);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].op, Op::Init);
    assert_eq!(entries[0].actor.kind(), ActorKind::User);
    assert_eq!(entries[0].actor.id(), "test");
    // 書ける
    call(
        vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    assert_eq!(fs::read(path.join("a.txt")).unwrap(), b"a");
    // ホームには、保管庫のフォルダのほかに何も作らない
    assert_eq!(names_in(&home), [NAME]);
}

/// 2回目は作らずに開く（created:false）。vault_id は同じ、init の行は増えない。
#[test]
fn h2_the_second_call_opens_without_creating() {
    let dir = TestDir::new("hv-h2");
    let home = fake_home(&dir);
    let first = open_default(&home).unwrap();
    assert!(first.created);
    let id = first.vault.vault_id().to_owned();
    drop(first);
    let second = open_default(&home).unwrap();
    assert!(!second.created);
    assert_eq!(second.vault.vault_id(), id);
    assert!(!second.vault.is_read_only());
    let inits = journal(&home.join(NAME))
        .iter()
        .filter(|e| e.op == Op::Init)
        .count();
    assert_eq!(inits, 1);
}

/// 空のフォルダ（保管庫でない）→ NOT_A_VAULT。.vault を作らない。
#[test]
fn h3_an_empty_folder_is_not_a_vault_and_stays_empty() {
    let dir = TestDir::new("hv-h3");
    let home = fake_home(&dir);
    fs::create_dir(home.join(NAME)).unwrap();
    expect_err(
        open_default(&home),
        ErrorCode::NotAVault,
        &home,
        "空のフォルダ",
    );
    assert_eq!(names_in(&home.join(NAME)), Vec::<String>::new());
}

/// 中身のあるフォルダ（保管庫でない）→ NOT_A_VAULT。中身は変わらない。
#[test]
fn h4_a_folder_with_contents_is_not_a_vault_and_is_not_changed() {
    let dir = TestDir::new("hv-h4");
    let home = fake_home(&dir);
    let path = home.join(NAME);
    fs::create_dir_all(path.join("sub")).unwrap();
    fs::write(path.join("note.txt"), b"keep").unwrap();
    expect_err(
        open_default(&home),
        ErrorCode::NotAVault,
        &home,
        "中身のあるフォルダ",
    );
    assert_eq!(names_in(&path), ["note.txt", "sub"]);
    assert_eq!(fs::read(path.join("note.txt")).unwrap(), b"keep");
    assert_eq!(names_in(&path.join("sub")), Vec::<String>::new());
}

/// .vault だけがあり vault.json がない → NOT_A_VAULT。init しない（履歴などを作らない）。
#[test]
fn j8_a_folder_with_only_dot_vault_is_not_a_vault_and_is_not_initialized() {
    let dir = TestDir::new("hv-dotvault");
    let home = fake_home(&dir);
    let path = home.join(NAME);
    fs::create_dir_all(path.join(".vault")).unwrap();
    expect_err(
        open_default(&home),
        ErrorCode::NotAVault,
        &home,
        ".vault だけ",
    );
    assert_eq!(names_in(&path), [".vault"]);
    assert_eq!(names_in(&path.join(".vault")), Vec::<String>::new());
}

/// ファイル → INVALID_ROOT。ファイルは変わらない。
#[test]
fn h5_a_file_is_an_invalid_root_and_is_not_changed() {
    let dir = TestDir::new("hv-h5");
    let home = fake_home(&dir);
    fs::write(home.join(NAME), b"file").unwrap();
    expect_err(
        open_default(&home),
        ErrorCode::InvalidRoot,
        &home,
        "ファイル",
    );
    assert_eq!(fs::read(home.join(NAME)).unwrap(), b"file");
    assert_eq!(names_in(&home), [NAME]);
}

/// ジャンクションの3通り（空のフォルダ・保管庫・先がない）→ INVALID_ROOT。先に .vault ができない、先の保管庫を
/// 開かない（残った一時ファイルが動かない）、リンクは残る。
#[cfg(windows)]
#[test]
fn h6_junctions_are_not_followed() {
    use common::junction;

    let dir = TestDir::new("hv-h6");
    // 空のフォルダを指す
    let home = fake_home(&dir);
    let empty = dir.join("empty");
    fs::create_dir(&empty).unwrap();
    let link = home.join(NAME);
    if !junction(&link, &empty) {
        return;
    }
    expect_link_refused(
        open_default(&home),
        &home,
        "空のフォルダを指すジャンクション",
    );
    assert_eq!(names_in(&empty), Vec::<String>::new(), "先に何も作らない");
    assert!(is_link(&link), "リンクは残る");
    fs::remove_dir(&link).unwrap();

    // 保管庫を指す（指す先が保管庫でも開かない）
    let (vault_path, vault) = new_vault(&dir, "a-vault");
    drop(vault);
    let left = vault_path.join(".vault").join("tmp").join("left.tmp");
    fs::write(&left, b"left").unwrap();
    let before = journal(&vault_path).len();
    assert!(junction(&link, &vault_path));
    expect_link_refused(open_default(&home), &home, "保管庫を指すジャンクション");
    assert!(
        left.is_file(),
        "先の保管庫を開かない（残った一時ファイルが動かない）"
    );
    assert_eq!(
        names_in(&vault_path.join(".vault").join("trash")),
        Vec::<String>::new()
    );
    assert_eq!(journal(&vault_path).len(), before);
    assert!(is_link(&link));
    fs::remove_dir(&link).unwrap();

    // 先がない（宙ぶらりん）
    let gone = dir.join("gone");
    fs::create_dir(&gone).unwrap();
    assert!(junction(&link, &gone));
    fs::remove_dir(&gone).unwrap();
    expect_link_refused(open_default(&home), &home, "先のないジャンクション");
    assert!(!gone.exists(), "先を作らない");
    assert!(is_link(&link));
    assert_eq!(names_in(&home), [NAME]);
}

/// シンボリックリンク（フォルダを指す・保管庫を指す・先がない）→ INVALID_ROOT。作れない PC では飛ばす。
#[cfg(windows)]
#[test]
fn h7_symbolic_links_are_not_followed() {
    let dir = TestDir::new("hv-h7");
    let home = fake_home(&dir);
    let empty = dir.join("empty");
    fs::create_dir(&empty).unwrap();
    let link = home.join(NAME);
    if let Err(e) = std::os::windows::fs::symlink_dir(&empty, &link) {
        // 権限がない（ERROR_PRIVILEGE_NOT_HELD = 1314）ときだけ飛ばす。ほかの失敗はテストの失敗
        if e.raw_os_error() == Some(1314) {
            skip(
                "h7_symbolic_links_are_not_followed",
                "シンボリックリンクを作る権限がない（OS のエラー 1314。開発者モードか管理者の権限が要る）",
            );
            return;
        }
        panic!("シンボリックリンクを作れない：{e}");
    }
    expect_link_refused(
        open_default(&home),
        &home,
        "フォルダを指すシンボリックリンク",
    );
    assert_eq!(names_in(&empty), Vec::<String>::new());
    assert!(is_link(&link));
    fs::remove_dir(&link).unwrap();

    let (vault_path, vault) = new_vault(&dir, "a-vault");
    drop(vault);
    let left = vault_path.join(".vault").join("tmp").join("left.tmp");
    fs::write(&left, b"left").unwrap();
    std::os::windows::fs::symlink_dir(&vault_path, &link).unwrap();
    expect_link_refused(open_default(&home), &home, "保管庫を指すシンボリックリンク");
    assert!(left.is_file());
    assert!(is_link(&link));
    fs::remove_dir(&link).unwrap();

    std::os::windows::fs::symlink_dir(dir.join("gone"), &link).unwrap();
    expect_link_refused(open_default(&home), &home, "先のないシンボリックリンク");
    assert!(!dir.join("gone").exists());
    assert!(is_link(&link));
}

/// ホームがない（何段も）・ホームがファイル・途中がファイル → INVALID_ROOT。祖先も作らない。
#[test]
fn h8_a_missing_or_file_home_is_an_invalid_root_and_nothing_is_made() {
    let dir = TestDir::new("hv-h8");
    let missing = dir.join("missing").join("a").join("b");
    expect_missing_parent(open_default(&missing), &missing, "ないホーム");
    assert!(!dir.join("missing").exists(), "祖先も作らない");

    let file = dir.join("home.txt");
    fs::write(&file, b"file").unwrap();
    expect_missing_parent(open_default(&file), &file, "ファイルのホーム");
    let below = file.join("x");
    expect_missing_parent(open_default(&below), &below, "途中がファイル");
    assert_eq!(fs::read(&file).unwrap(), b"file");
    assert_eq!(names_in(dir.path()), ["home.txt"]);
}

/// 名前の誤り → INVALID_ROOT（「保管庫の名前：…」）。何も作らない。
#[test]
fn h9_bad_names_are_invalid_roots_and_nothing_is_made() {
    let dir = TestDir::new("hv-h9");
    let home = fake_home(&dir);
    for name in [
        "", "a/b", r"a\b", ".", "..", "CON", "x.", "x ", ".vault", "a:b",
    ] {
        let e = open_or_create_in(&home, name, &env(), &actor()).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidRoot, "{name:?}");
        assert!(e.message().starts_with("保管庫の名前："), "{name:?}: {e}");
        assert_private(&e, &home);
    }
    assert_eq!(names_in(&home), Vec::<String>::new());
}

/// 置き場所が %LOCALAPPDATA% → INVALID_ROOT。置き場所が本物のホームで名前が「.」から始まる → INVALID_ROOT。
/// どちらも根の判定で断られる「調べるだけ」の道で、何も作らない（本物の場所では、これより先の道を通さない）。
///
/// 万一このテストが失敗して、本物の場所にフォルダが残ったときは、失敗の文に出る相対の名前（vault-core-default-test- で
/// 始まる名前）のフォルダを、エクスプローラーで %LOCALAPPDATA% かホーム（%USERPROFILE%）を開いて、手で消す
/// （テストは自動では消さない。本物の場所のものを消す処理をテストに置かないため）。
#[test]
fn h10_protected_places_are_refused_without_making_anything() {
    let unique = format!(
        "vault-core-default-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        let e = open_or_create_in(&local, &unique, &env(), &actor()).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidRoot, "{e}");
        assert_private(&e, &local);
        assert!(
            fs::symlink_metadata(local.join(&unique)).is_err(),
            "%LOCALAPPDATA% の下に {unique} が残った（手で消す）"
        );
    } else {
        skip("h10", "LOCALAPPDATA がない");
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .expect("ホームがある");
    let dotted = format!(".{unique}");
    let e = open_or_create_in(&home, &dotted, &env(), &actor()).unwrap_err();
    assert_eq!(e.code(), ErrorCode::InvalidRoot, "{e}");
    assert_private(&e, &home);
    assert!(
        fs::symlink_metadata(home.join(&dotted)).is_err(),
        "ホームの下に {dotted} が残った（手で消す）"
    );
}

/// 大文字小文字の違う既存の保管庫 → 開く（created:false）。
#[test]
fn h11_an_existing_vault_with_a_different_case_is_opened() {
    let dir = TestDir::new("hv-h11");
    let home = fake_home(&dir);
    let upper = home.join("BUILDOFFICE");
    Vault::init(&upper, &env(), &actor()).unwrap();
    let id = Vault::open(&upper, true, &env())
        .unwrap()
        .vault_id()
        .to_owned();
    if cfg!(windows) {
        let opened = open_default(&home).unwrap();
        assert!(!opened.created);
        assert_eq!(opened.vault.vault_id(), id);
        assert_eq!(opened.vault.name(), "BUILDOFFICE");
        assert_eq!(names_in(&home), ["BUILDOFFICE"]);
    }
}

/// 消したあとにまた呼ぶと、新しい空の保管庫を作る（created:true、vault_id が変わる）。
#[test]
fn j8_a_deleted_default_vault_is_made_again() {
    let dir = TestDir::new("hv-again");
    let home = fake_home(&dir);
    let first = open_default(&home).unwrap();
    let id = first.vault.vault_id().to_owned();
    drop(first);
    fs::remove_dir_all(home.join(NAME)).unwrap();
    // vault_id はミリ秒の時刻とプロセスの番号なので、同じミリ秒にならないように少し待つ
    thread::sleep(Duration::from_millis(5));
    let again = open_default(&home).unwrap();
    assert!(again.created);
    assert_ne!(again.vault.vault_id(), id);
    assert_eq!(journal(&home.join(NAME)).len(), 1);
}

/// ホームが UNC → INVALID_ROOT（文字列の段階で断る。ネットワークに触れない）。
#[cfg(windows)]
#[test]
fn j8_a_unc_home_is_an_invalid_root() {
    let home = PathBuf::from(r"\\vault-core-test.invalid\share\u");
    let e = open_or_create_in(&home, NAME, &env(), &actor()).unwrap_err();
    assert_eq!(e.code(), ErrorCode::InvalidRoot);
    assert!(!e.message().contains("vault-core-test"), "{e}");
    assert_private(&e, &home);
}

/// ホームのパスに空白と日本語があっても、作って開ける。
#[test]
fn j8_a_home_with_spaces_and_japanese() {
    let dir = TestDir::new("hv-jp");
    let home = dir.join("ホーム の フォルダ");
    fs::create_dir(&home).unwrap();
    let first = open_default(&home).unwrap();
    assert!(first.created);
    assert_eq!(first.vault.name(), NAME);
    drop(first);
    assert!(!open_default(&home).unwrap().created);
}

/// ホームを 8.3 の短い名前で渡しても、ハンドルの最終のパスで比べるので、作って開ける。長い名前で渡しても同じ保管庫。
/// 短い名前が作られないボリュームでは飛ばす。
#[cfg(windows)]
#[test]
fn j2_a_home_given_by_its_short_name() {
    use common::{ShortName, short_name};

    let dir = TestDir::new("hv-short");
    let long = "home folder with a long name";
    fs::create_dir(dir.join(long)).unwrap();
    let ShortName::Name(short) = short_name(dir.path(), long) else {
        skip(
            "j2_a_home_given_by_its_short_name",
            "このボリュームでは 8.3 の短い名前が作られない",
        );
        return;
    };
    let home_short = dir.join(&short);
    let first = open_default(&home_short).unwrap();
    assert!(first.created);
    assert_eq!(first.vault.name(), NAME);
    let id = first.vault.vault_id().to_owned();
    drop(first);
    assert!(
        dir.join(long)
            .join(NAME)
            .join(".vault")
            .join("vault.json")
            .is_file()
    );
    for home in [dir.join(long), home_short] {
        let again = open_default(&home).unwrap();
        assert!(!again.created);
        assert_eq!(again.vault.vault_id(), id);
    }
}

/// ホームそのものがジャンクションでも（ホームを移した PC など）、たどった先の直下に作って開ける。
#[cfg(windows)]
#[test]
fn j2_a_home_that_is_a_junction() {
    use common::junction;

    let dir = TestDir::new("hv-home-link");
    let real = dir.join("real-home");
    fs::create_dir(&real).unwrap();
    let link = dir.join("home-link");
    if !junction(&link, &real) {
        return;
    }
    let first = open_default(&link).unwrap();
    assert!(first.created);
    let id = first.vault.vault_id().to_owned();
    drop(first);
    assert!(real.join(NAME).join(".vault").join("vault.json").is_file());
    for home in [link.clone(), real.clone()] {
        let again = open_default(&home).unwrap();
        assert!(!again.created);
        assert_eq!(again.vault.vault_id(), id);
    }
    fs::remove_dir(&link).unwrap();
}

/// init の側：調べたあと、作る前に、場所を空のフォルダを指すジャンクションに差し替える → 何も作らずに
/// INVALID_ROOT。行き先に .vault ができない。
#[cfg(windows)]
#[test]
fn j1_a_junction_put_in_after_the_check_gets_nothing_made() {
    use common::junction;
    use std::cell::Cell;
    use std::rc::Rc;
    use vault_core::test_support::{Step, on_next};

    let dir = TestDir::new("hv-j1-init");
    let home = fake_home(&dir);
    let elsewhere = dir.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let link = home.join(NAME);
    let made = Rc::new(Cell::new(false));
    let (l, t, m) = (link.clone(), elsewhere.clone(), made.clone());
    on_next(Step::AfterDefaultVaultCheck, move || {
        m.set(junction(&l, &t))
    });
    let result = open_default(&home);
    if !made.get() {
        return;
    }
    expect_moved(result, &home, "差し替えたジャンクション");
    assert_eq!(
        names_in(&elsewhere),
        Vec::<String>::new(),
        "行き先に .vault ができない"
    );
    assert!(is_link(&link), "リンクは残る");
    fs::remove_dir(&link).unwrap();
}

/// init の側：調べたあと、作る前に、ホームそのものを別のフォルダを指すジャンクションに差し替える → 何も作らずに
/// INVALID_ROOT。
#[cfg(windows)]
#[test]
fn j1_a_home_swapped_after_the_check_gets_nothing_made() {
    use common::junction;
    use std::cell::Cell;
    use std::rc::Rc;
    use vault_core::test_support::{Step, on_next};

    let dir = TestDir::new("hv-j1-home");
    let home = fake_home(&dir);
    let elsewhere = dir.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let moved = dir.join("home-moved");
    let made = Rc::new(Cell::new(false));
    let (h, t, mv, m) = (home.clone(), elsewhere.clone(), moved.clone(), made.clone());
    on_next(Step::AfterDefaultVaultCheck, move || {
        fs::rename(&h, &mv).unwrap();
        m.set(junction(&h, &t));
    });
    let result = open_default(&home);
    if !made.get() {
        return;
    }
    expect_moved(result, &home, "差し替えたホーム");
    assert_eq!(
        names_in(&elsewhere),
        Vec::<String>::new(),
        "行き先に何も作らない"
    );
    assert_eq!(
        names_in(&moved),
        Vec::<String>::new(),
        "もとのホームにも作らない"
    );
    fs::remove_dir(&home).unwrap();
}

/// open の側：保管庫があると調べたあと、開く前に、場所を別の保管庫を指すジャンクションに差し替える →
/// INVALID_ROOT。行き先の保管庫の残った一時ファイルは動かない（ゴミ箱へ移さない）。
#[cfg(windows)]
#[test]
fn j1_a_vault_swapped_after_the_check_is_not_changed() {
    use common::junction;
    use std::cell::Cell;
    use std::rc::Rc;
    use vault_core::test_support::{Step, on_next};

    let dir = TestDir::new("hv-j1-open");
    let home = fake_home(&dir);
    drop(open_default(&home).unwrap());
    let (other, vault) = new_vault(&dir, "other");
    drop(vault);
    let left = other.join(".vault").join("tmp").join("left.tmp");
    fs::write(&left, b"left").unwrap();
    let before = journal(&other).len();
    let path = home.join(NAME);
    let moved = dir.join("moved-away");
    let made = Rc::new(Cell::new(false));
    let (p, o, mv, m) = (path.clone(), other.clone(), moved.clone(), made.clone());
    on_next(Step::AfterDefaultVaultCheck, move || {
        fs::rename(&p, &mv).unwrap();
        m.set(junction(&p, &o));
    });
    let result = open_default(&home);
    if !made.get() {
        return;
    }
    expect_moved(result, &home, "差し替えた保管庫");
    assert!(left.is_file(), "行き先の tmp が動かない");
    assert_eq!(
        names_in(&other.join(".vault").join("trash")),
        Vec::<String>::new()
    );
    assert_eq!(journal(&other).len(), before);
    assert!(is_link(&path));
    fs::remove_dir(&path).unwrap();
}

/// ないと調べたあと、作る前に、ほかが同じ場所に保管庫を作った（同時に起動したときの負けた側）→ init は「すでに
/// 保管庫」で失敗し、そのときだけ開くへ進む（created:false）。作り直さない（init の行は1つ、vault_id はほかが作ったもの）。
#[test]
fn j7_a_vault_made_by_someone_else_after_the_check_is_opened() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use vault_core::test_support::{Step, on_next};

    let dir = TestDir::new("hv-j7-other");
    let home = fake_home(&dir);
    let path = home.join(NAME);
    let made_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let (p, slot) = (path.clone(), made_id.clone());
    on_next(Step::AfterDefaultVaultCheck, move || {
        Vault::init(&p, &env(), &actor()).unwrap();
        let id = Vault::open(&p, true, &env()).unwrap().vault_id().to_owned();
        *slot.borrow_mut() = Some(id);
    });
    let opened = open_default(&home).unwrap();
    assert!(!opened.created);
    assert_eq!(
        Some(opened.vault.vault_id().to_owned()),
        *made_id.borrow(),
        "ほかが作った保管庫を開く"
    );
    let inits = journal(&path).iter().filter(|e| e.op == Op::Init).count();
    assert_eq!(inits, 1);
}

/// 2つのスレッドが同じ偽のホームで同時に呼ぶ。作るのは1つだけ（init の行は1つ）で、保管庫は壊れない。負けた側は
/// 決まったエラーか、開くへ進む（created:false）。そのあと開ける。
#[test]
fn j7_two_callers_at_once_make_one_vault() {
    for round in 0..8 {
        let dir = TestDir::new("hv-race");
        let home = fake_home(&dir);
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let (home, barrier) = (home.clone(), barrier.clone());
                thread::spawn(move || {
                    barrier.wait();
                    open_default(&home)
                        .map(|opened| (opened.created, opened.vault.vault_id().to_owned()))
                })
            })
            .collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let created = results
            .iter()
            .filter(|r| matches!(r, Ok((true, _))))
            .count();
        assert_eq!(created, 1, "{round} 回目：作ったのが1つでない：{results:?}");
        for result in &results {
            if let Err(e) = result {
                assert!(
                    matches!(
                        e.code(),
                        ErrorCode::IoError
                            | ErrorCode::NotAVault
                            | ErrorCode::Busy
                            | ErrorCode::AccessDenied
                            | ErrorCode::Locked
                    ),
                    "{round} 回目：{e}"
                );
                assert_private(e, &home);
            }
        }
        let after = open_default(&home).unwrap();
        assert!(!after.created);
        for (_, id) in results.iter().flatten() {
            assert_eq!(id, after.vault.vault_id());
        }
        let entries = journal(&home.join(NAME));
        assert_eq!(
            entries.iter().filter(|e| e.op == Op::Init).count(),
            1,
            "{round} 回目"
        );
        assert_eq!(names_in(&home), [NAME]);
    }
}
