//! `Vault::init` と `Vault::open`。

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{TestDir, actor, env, expect_code, new_vault};
use vault_core::{ErrorCode, ErrorDetail, FORMAT, Vault};

#[test]
fn init_creates_the_layout_and_open_reads_it() {
    let dir = TestDir::new("init-new");
    let path = dir.join("nested").join("v");
    Vault::init(&path, &env(), &actor()).unwrap();

    let dot = path.join(".vault");
    for sub in ["", "trash", "journal", "tmp"] {
        assert!(dot.join(sub).is_dir(), ".vault/{sub}");
    }
    assert!(dot.join("lock").is_file());
    assert!(dot.join("vault.json").is_file());
    // vault.json は一時ファイルから rename したので、tmp は空
    assert_eq!(fs::read_dir(dot.join("tmp")).unwrap().count(), 0);

    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(dot.join("vault.json")).unwrap()).unwrap();
    assert_eq!(json["format"], 1);
    let vault_id = json["vault_id"].as_str().unwrap();
    let (ms, pid) = vault_id.split_once('-').unwrap();
    assert!(
        ms.parse::<u64>().is_ok() && pid.parse::<u32>().is_ok(),
        "{vault_id}"
    );
    let created = json["created_at"].as_str().unwrap();
    assert_eq!(created.len(), 24, "{created}");
    assert!(
        created.ends_with('Z') && created.as_bytes()[10] == b'T',
        "{created}"
    );

    let vault = Vault::open(&path, false, &env()).unwrap();
    assert_eq!(vault.format(), FORMAT);
    assert_eq!(vault.vault_id(), vault_id);
    assert_eq!(vault.created_at(), created);
    assert_eq!(vault.name(), "v");
    assert!(!vault.is_read_only());

    let ro = Vault::open(&path, true, &env()).unwrap();
    assert!(ro.is_read_only());
}

#[test]
fn init_accepts_an_existing_empty_folder() {
    let dir = TestDir::new("init-empty");
    Vault::init(dir.path(), &env(), &actor()).unwrap();
    assert!(Vault::open(dir.path(), false, &env()).is_ok());
}

#[test]
fn init_rejects_a_non_empty_folder() {
    let dir = TestDir::new("init-non-empty");
    for name in [".hidden", "note.txt"] {
        let path = dir.join(format!("v-{name}"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(name), b"x").unwrap();
        expect_code(
            Vault::init(&path, &env(), &actor()),
            ErrorCode::InvalidRoot,
            name,
        );
        assert!(!path.join(".vault").exists());
    }
    // 空のフォルダが1つあるだけでも、空でない
    let path = dir.join("v-sub");
    fs::create_dir_all(path.join("sub")).unwrap();
    expect_code(
        Vault::init(&path, &env(), &actor()),
        ErrorCode::InvalidRoot,
        "sub",
    );
}

#[test]
fn init_rejects_a_file() {
    let dir = TestDir::new("init-file");
    let path = dir.join("file");
    fs::write(&path, b"x").unwrap();
    expect_code(
        Vault::init(&path, &env(), &actor()),
        ErrorCode::InvalidRoot,
        "file",
    );
}

#[test]
fn init_twice_is_rejected() {
    let dir = TestDir::new("init-twice");
    let (path, _vault) = new_vault(&dir, "v");
    expect_code(
        Vault::init(&path, &env(), &actor()),
        ErrorCode::InvalidRoot,
        "2回目",
    );
}

#[test]
fn init_rejects_a_folder_inside_a_vault() {
    let dir = TestDir::new("init-nested");
    let (path, _vault) = new_vault(&dir, "v");
    let inner = path.join("sub").join("v");
    expect_code(
        Vault::init(&inner, &env(), &actor()),
        ErrorCode::InvalidRoot,
        "入れ子",
    );
    // 判定より前にフォルダを作らない
    assert!(!path.join("sub").exists());
    // すでにある空のフォルダでも同じ
    fs::create_dir(path.join("empty")).unwrap();
    expect_code(
        Vault::init(&path.join("empty"), &env(), &actor()),
        ErrorCode::InvalidRoot,
        "入れ子（空のフォルダ）",
    );
    assert!(!path.join("empty").join(".vault").exists());
}

/// `.`・`..` を含む入力（文字列で作る。PathBuf に push すると消えることがあるため）。
fn dotted_inputs(base: &Path) -> Vec<PathBuf> {
    let b = base.display();
    let mut list = vec![
        PathBuf::from(format!("{b}/x/../v")),
        PathBuf::from(format!("{b}/./v")),
        PathBuf::from(format!("{b}/x/..")),
    ];
    if cfg!(windows) {
        list.push(PathBuf::from(format!(r"{b}\x\..\v")));
        list.push(PathBuf::from(format!(r"{b}\.\v")));
        list.push(PathBuf::from(format!(r"{b}\x\.. \v")));
    }
    list
}

#[test]
fn init_and_open_reject_dot_segments_without_creating_folders() {
    let dir = TestDir::new("dots");
    let base = dir.join("base");
    for input in dotted_inputs(&base) {
        let what = input.display().to_string();
        expect_code(
            Vault::init(&input, &env(), &actor()),
            ErrorCode::InvalidRoot,
            &what,
        );
        expect_code(
            Vault::open(&input, false, &env()),
            ErrorCode::InvalidRoot,
            &what,
        );
    }
    // どのフォルダも作られていない
    assert!(!base.exists());
}

/// 要素の末尾の `.`・空白（Windows）と、Windows の予約名の要素。
#[test]
fn init_and_open_reject_trailing_dots_spaces_and_reserved_names_without_creating_folders() {
    let dir = TestDir::new("names");
    let base = dir.join("base");
    let b = base.display();
    let mut inputs = vec![
        format!("{b}/CON/v"),
        format!("{b}/nul.txt"),
        format!("{b}/x/COM1"),
        format!("{b}/x/lpt²/v"),
    ];
    if cfg!(windows) {
        inputs.extend([
            format!(r"{b}\v."),
            format!(r"{b}\v "),
            format!(r"{b}\x.\v"),
            format!(r"{b}\x \v"),
            format!(r"{b}\x\v.."),
            format!(r"{b}\CON .txt"),
        ]);
    }
    for input in &inputs {
        let path = PathBuf::from(input);
        expect_code(
            Vault::init(&path, &env(), &actor()),
            ErrorCode::InvalidRoot,
            input,
        );
        expect_code(
            Vault::open(&path, false, &env()),
            ErrorCode::InvalidRoot,
            input,
        );
    }
    // どのフォルダも作られていない（Windows は末尾の . と空白を落として「base\v」「base\x」を作ってしまう）
    assert!(!base.exists());
}

#[test]
fn init_and_open_reject_relative_paths() {
    let rel = Path::new("vault-core-relative-test-vault");
    expect_code(
        Vault::init(rel, &env(), &actor()),
        ErrorCode::InvalidRoot,
        "相対パス",
    );
    expect_code(
        Vault::open(rel, false, &env()),
        ErrorCode::InvalidRoot,
        "相対パス",
    );
    assert!(!rel.exists());
}

#[test]
fn open_rejects_folders_that_are_not_vaults() {
    let dir = TestDir::new("open-not-vault");

    // ないフォルダ
    expect_code(
        Vault::open(&dir.join("missing"), false, &env()),
        ErrorCode::NotAVault,
        "ない",
    );

    // 空のフォルダ
    let empty = dir.join("empty");
    fs::create_dir(&empty).unwrap();
    expect_code(
        Vault::open(&empty, false, &env()),
        ErrorCode::NotAVault,
        "空",
    );

    // .vault がファイル
    let file = dir.join("vault-file");
    fs::create_dir(&file).unwrap();
    fs::write(file.join(".vault"), b"x").unwrap();
    expect_code(
        Vault::open(&file, false, &env()),
        ErrorCode::NotAVault,
        ".vault がファイル",
    );

    // vault.json がない・壊れている・知らない形式・大きすぎる
    let cases: [(&str, Option<Vec<u8>>); 5] = [
        ("vault.json がない", None),
        ("壊れた JSON", Some(b"{not json".to_vec())),
        (
            "format 2",
            Some(
                br#"{"format":2,"vault_id":"1-1","created_at":"2026-01-01T00:00:00.000Z"}"#
                    .to_vec(),
            ),
        ),
        ("項目が足りない", Some(br#"{"format":1}"#.to_vec())),
        ("大きすぎる", Some(vec![b' '; 64 * 1024 + 1])),
    ];
    for (i, (what, content)) in cases.into_iter().enumerate() {
        let (path, _vault) = new_vault(&dir, &format!("bad-json-{i}"));
        let json = path.join(".vault").join("vault.json");
        fs::remove_file(&json).unwrap();
        if let Some(bytes) = content {
            fs::write(&json, bytes).unwrap();
        }
        expect_code(
            Vault::open(&path, false, &env()),
            ErrorCode::NotAVault,
            what,
        );
    }

    // vault.json がフォルダ
    let (path, _vault) = new_vault(&dir, "json-dir");
    let json = path.join(".vault").join("vault.json");
    fs::remove_file(&json).unwrap();
    fs::create_dir(&json).unwrap();
    expect_code(
        Vault::open(&path, false, &env()),
        ErrorCode::NotAVault,
        "vault.json がフォルダ",
    );
}

#[test]
fn open_rejects_a_folder_inside_a_vault() {
    let dir = TestDir::new("open-nested");
    let (path, _vault) = new_vault(&dir, "v");
    // 保管庫の中に、手で .vault/vault.json を置いて、入れ子の保管庫を作る
    let inner = path.join("sub");
    let src = path.join(".vault").join("vault.json");
    fs::create_dir_all(inner.join(".vault")).unwrap();
    fs::copy(&src, inner.join(".vault").join("vault.json")).unwrap();
    expect_code(
        Vault::open(&inner, false, &env()),
        ErrorCode::InvalidRoot,
        "入れ子",
    );
}

#[test]
fn open_allows_missing_subfolders_of_the_management_folder() {
    let dir = TestDir::new("open-missing-sub");
    let (path, _vault) = new_vault(&dir, "v");
    // journal には init の行があるので、中身ごと消す（テストのデータ）
    for sub in ["trash", "journal", "tmp"] {
        fs::remove_dir_all(path.join(".vault").join(sub)).unwrap();
    }
    let vault = Vault::open(&path, false, &env()).unwrap();
    // 使うときに作る
    common::call(
        &vault,
        "vault_write",
        serde_json::json!({"path": "a.txt", "content": "x"}),
    )
    .unwrap();
    for sub in ["journal", "tmp"] {
        assert!(path.join(".vault").join(sub).is_dir(), "{sub}");
    }
}

#[test]
fn test_dirs_are_removed_when_dropped() {
    let dir = TestDir::new("cleanup");
    let (path, _vault) = new_vault(&dir, "v");
    let kept = dir.path().to_path_buf();
    assert!(path.join(".vault").exists());
    drop(dir);
    assert!(!kept.exists());
}

#[cfg(windows)]
#[test]
fn open_rejects_management_folders_that_are_junctions() {
    use common::junction;

    for (i, sub) in [".vault", ".vault/trash", ".vault/journal", ".vault/tmp"]
        .into_iter()
        .enumerate()
    {
        let dir = TestDir::new("open-junction");
        let (path, _vault) = new_vault(&dir, "v");
        let outside = dir.join(format!("outside-{i}"));
        // mklink は / の区切りを受け付けないので、要素ごとに join する
        let target = sub.split('/').fold(path.clone(), |p, part| p.join(part));
        // 本物のフォルダを外へ移し、そこを指すジャンクションに置き換える
        fs::rename(&target, &outside).unwrap();
        if !junction(&target, &outside) {
            return;
        }
        expect_code(Vault::open(&path, false, &env()), ErrorCode::NotAVault, sub);
    }
}

#[cfg(windows)]
#[test]
fn open_through_a_junction_uses_the_real_folder() {
    use common::junction;

    let dir = TestDir::new("open-via-junction");
    let (path, vault) = new_vault(&dir, "v");
    let link = dir.join("link");
    if !junction(&link, &path) {
        return;
    }
    let via = Vault::open(&link, false, &env()).unwrap();
    assert_eq!(via.vault_id(), vault.vault_id());
    assert_eq!(via.name(), vault.name());
}

/// init が .vault を作ったあとで失敗したら、この呼び出しで作った .vault を片づけ、
/// もとからあった保管庫のフォルダは残す。
#[cfg(windows)]
#[test]
fn init_removes_what_it_created_when_it_fails_after_making_dot_vault() {
    use common::DenySubdirsInChildren;

    let dir = TestDir::new("init-undo-vault");
    let target = dir.join("v");
    fs::create_dir(&target).unwrap();
    // v の中には .vault を作れるが、.vault の中には trash などを作れないようにする
    let deny = DenySubdirsInChildren::new(&target);
    let result = Vault::init(&target, &env(), &actor());
    drop(deny);
    expect_code(
        result,
        ErrorCode::IoError,
        ".vault の中にフォルダを作れない",
    );
    assert!(target.is_dir(), "もとからあったフォルダは残す");
    assert_eq!(
        fs::read_dir(&target).unwrap().count(),
        0,
        ".vault を片づける"
    );
    // 片づけたあとは、ふつうに作れる
    Vault::init(&target, &env(), &actor()).unwrap();
}

/// init が途中で失敗したら、この呼び出しで作った保管庫のフォルダ（途中のフォルダも）を片づける。
#[cfg(windows)]
#[test]
fn init_removes_the_folders_it_created_when_it_fails() {
    use common::DenySubdirsInChildren;

    let dir = TestDir::new("init-undo-dirs");
    let parent = dir.join("p");
    fs::create_dir(&parent).unwrap();
    // p の中には new を作れるが、new の中には v を作れないようにする
    let deny = DenySubdirsInChildren::new(&parent);
    let result = Vault::init(&parent.join("new").join("v"), &env(), &actor());
    drop(deny);
    expect_code(result, ErrorCode::IoError, "new の中に v を作れない");
    assert!(!parent.join("new").exists(), "作った new を片づける");
    assert!(parent.is_dir(), "もとからあった p は残す");
}

/// init の片づけは、ほかの処理が途中のフォルダに置いたファイルを消さない。
/// 片づけの処理（Windows 以外の `InitScope::undo` と同じもの）を、CARGO_TARGET_TMPDIR の下で直接動かす。
/// Windows 以外だけ（Windows は、ハンドルで片づける InitScope を tests/init_undo.rs の I5 で確かめる）。
#[cfg(not(windows))]
#[test]
fn undo_keeps_files_that_others_put_in_created_folders() {
    let dir = TestDir::new("undo-foreign");
    let created_dir = dir.join("new");
    let inner = created_dir.join("v");
    fs::create_dir_all(&inner).unwrap();
    let own_file = inner.join("own.txt");
    fs::write(&own_file, b"x").unwrap();
    // ほかの処理が、init の作った途中のフォルダにファイルを置いた
    let foreign = created_dir.join("foreign.txt");
    fs::write(&foreign, b"keep").unwrap();

    vault_core::test_support::undo_created(vec![
        (created_dir.clone(), true),
        (inner.clone(), true),
        (own_file.clone(), false),
    ]);
    assert!(!own_file.exists(), "自分で作ったファイルは消す");
    assert!(!inner.exists(), "空になったフォルダは消す");
    assert_eq!(
        fs::read(&foreign).unwrap(),
        b"keep",
        "ほかの処理のファイルは消さない"
    );
    assert!(created_dir.is_dir(), "中身のあるフォルダは消さない");
}

/// UNC（`\server\…`・`//server/…`）とデバイスの形（`\?\`・`\.\`・`\?\GLOBALROOT`）の入力は、
/// ファイルシステムに触れる前（canonicalize の前）に、文字列の段階で INVALID_ROOT（init と open の両方）。
#[cfg(windows)]
#[test]
fn unc_and_device_inputs_are_rejected_before_touching_the_file_system() {
    use std::time::{Duration, Instant};

    let dir = TestDir::new("init-unc");
    let canon = fs::canonicalize(dir.path()).unwrap(); // \?\D:\… の形
    let inputs = [
        PathBuf::from(r"\vault-test-no-such-server\share\v"),
        PathBuf::from("//vault-test-no-such-server/share/v"),
        PathBuf::from(r"\?\UNC\vault-test-no-such-server\share\v"),
        PathBuf::from(r"\.\C:\v"),
        PathBuf::from(r"\?\GLOBALROOT\Device\HarddiskVolume1\v"),
        canon.join("v"), // ふつうの場所でも、\?\ の付いた形では受け付けない
    ];
    for input in &inputs {
        let start = Instant::now();
        let what = input.display().to_string();
        expect_code(
            Vault::init(input, &env(), &actor()),
            ErrorCode::InvalidRoot,
            &what,
        );
        expect_code(
            Vault::open(input, false, &env()),
            ErrorCode::InvalidRoot,
            &what,
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "ネットワークに触れていない：{what}"
        );
    }
    assert!(!canon.join("v").exists(), "作らない");
    // ふつうの形（ドライブ文字から）なら作れる
    let plain = dir.join("v");
    Vault::init(&plain, &env(), &actor()).unwrap();
}

/// すでに保管庫のフォルダに init したら、「すでに保管庫」とわかる文で断る。
#[test]
fn init_on_an_existing_vault_says_so() {
    let dir = TestDir::new("init-again");
    let (path, _vault) = new_vault(&dir, "v");
    let err = Vault::init(&path, &env(), &actor()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRoot);
    assert!(err.message().contains("すでに保管庫"), "{err}");
    // 呼ぶ側は、文ではなく印で見分ける。印は表示（「CODE: 説明」）に出ない
    assert_eq!(err.detail(), Some(ErrorDetail::AlreadyAVault));
    assert!(!err.to_string().contains("AlreadyAVault"), "{err}");
}

/// ほかの「保管庫にできない」（空でないフォルダ・保管庫の中）には、すでに保管庫の印を付けない。
#[test]
fn other_invalid_roots_are_not_marked_as_already_a_vault() {
    let dir = TestDir::new("init-not-already");
    let full = dir.path().join("full");
    std::fs::create_dir_all(&full).unwrap();
    std::fs::write(full.join("a.txt"), b"a").unwrap();
    let err = Vault::init(&full, &env(), &actor()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRoot);
    assert_eq!(err.detail(), None, "{err}");

    let (path, _vault) = new_vault(&dir, "outer");
    let err = Vault::init(&path.join("inner"), &env(), &actor()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRoot);
    assert!(err.message().contains("すでに保管庫"), "{err}");
    assert_eq!(
        err.detail(),
        None,
        "保管庫の中は、すでに保管庫とは別：{err}"
    );
}

/// cmd の内部コマンドに渡すパスは、`/` 区切りの相対パスからでも、`/` のない絶対パスになる
/// （CARGO_TARGET_DIR を `target/xxx` にしても、mklink・dir がスイッチと読まないように）。
#[cfg(windows)]
#[test]
fn cmd_paths_are_absolute_without_slashes() {
    let arg = common::cmd_path(std::path::Path::new("target/dev/x"));
    let text = arg.to_string_lossy();
    assert!(!text.contains('/'), "{text}");
    assert!(std::path::Path::new(&arg).is_absolute(), "{text}");
    assert!(text.ends_with(r"target\dev\x"), "{text}");
}
