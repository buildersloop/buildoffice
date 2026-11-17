//! `Vault::resolve`。

mod common;

use std::fs;

use common::{TestDir, expect_code, new_vault};
use vault_core::ErrorCode;

/// パスの表のうち、拒否されるもの（と、期待するエラーの種類）。
const ADR_REJECTED: [(&str, ErrorCode); 22] = [
    ("..", ErrorCode::InvalidPath),
    ("a/../b", ErrorCode::InvalidPath),
    ("/x", ErrorCode::InvalidPath),
    ("C:/x", ErrorCode::InvalidPath),
    ("C:x", ErrorCode::InvalidPath),
    (r"\\s\x", ErrorCode::InvalidPath),
    (r"a\b", ErrorCode::InvalidPath),
    ("a:b", ErrorCode::InvalidPath),
    ("CON", ErrorCode::InvalidPath),
    ("con.txt", ErrorCode::InvalidPath),
    ("CONIN$", ErrorCode::InvalidPath),
    ("a.", ErrorCode::InvalidPath),
    ("a ", ErrorCode::InvalidPath),
    ("a//b", ErrorCode::InvalidPath),
    ("a\u{0}b", ErrorCode::InvalidPath),
    ("a\u{202E}b", ErrorCode::InvalidPath),
    ("a\u{FEFF}b", ErrorCode::InvalidPath),
    (".VAULT/x", ErrorCode::ReservedPath),
    ("x/.vault/y", ErrorCode::ReservedPath),
    ("VAULT~1/x", ErrorCode::InvalidPath),
    (
        "vault~1/journal/00000000000000000001.jsonl",
        ErrorCode::InvalidPath,
    ),
    ("abc~2.txt", ErrorCode::InvalidPath),
];

/// パスの表のうち、通るもの。
const ADR_ACCEPTED: [&str; 3] = ["日本語のメモ.md", "my notes/a b.txt", "a/b/c.md"];

#[test]
fn adr_path_table_through_resolve() {
    let dir = TestDir::new("resolve-table");
    let (path, vault) = new_vault(&dir, "v");
    for (input, code) in ADR_REJECTED {
        expect_code(vault.resolve(input), code, &format!("{input:?}"));
    }
    let root = vault.resolve("").unwrap();
    for input in ADR_ACCEPTED {
        let abs = vault
            .resolve(input)
            .unwrap_or_else(|e| panic!("{input:?}: {e}"));
        assert!(abs.starts_with(&root), "{input:?}");
        assert_eq!(
            abs.strip_prefix(&root).unwrap().components().count(),
            input.split('/').count()
        );
    }
    // 根は保管庫のフォルダそのもの
    assert_eq!(root, fs::canonicalize(&path).unwrap());
}

#[test]
fn existing_and_missing_paths_resolve_inside_the_vault() {
    let dir = TestDir::new("resolve-existing");
    let (_path, vault) = new_vault(&dir, "v");
    let root = vault.resolve("").unwrap();
    fs::create_dir_all(root.join("a").join("b")).unwrap();
    fs::write(root.join("a").join("b").join("c.md"), b"x").unwrap();
    assert_eq!(
        vault.resolve("a/b/c.md").unwrap(),
        root.join("a").join("b").join("c.md")
    );
    // まだないパス（途中から先がない）
    assert_eq!(
        vault.resolve("a/new/deep/x.md").unwrap(),
        root.join("a").join("new").join("deep").join("x.md")
    );
    // 途中がファイル
    assert!(vault.resolve("a/b/c.md/x").is_ok());
}

#[cfg(windows)]
#[test]
fn junctions_are_rejected() {
    use common::junction;

    let dir = TestDir::new("resolve-junction");
    // cmd に渡すので、先頭に \\?\ の付かない形のパスを使う
    let (root, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"outside").unwrap();

    fs::create_dir(root.join("d")).unwrap();
    if !junction(&root.join("j"), &outside) || !junction(&root.join("d").join("j"), &outside) {
        return;
    }
    for input in [
        "j",
        "j/secret.txt",
        "j/new.txt",
        "d/j",
        "d/j/secret.txt",
        "d/j/x/y",
    ] {
        expect_code(vault.resolve(input), ErrorCode::OutsideVault, input);
    }
    assert!(vault.resolve("d").is_ok());
    assert!(vault.resolve("d/other").is_ok());
    // 外のファイルには触れていない
    assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), b"outside");
}

#[cfg(any(windows, unix))]
#[test]
fn symbolic_links_are_rejected() {
    use common::skip;

    let dir = TestDir::new("resolve-symlink");
    let (root, vault) = new_vault(&dir, "v");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"outside").unwrap();

    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&outside, root.join("ld")).and_then(|()| {
        std::os::windows::fs::symlink_file(outside.join("secret.txt"), root.join("lf"))
    });
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, root.join("ld"))
        .and_then(|()| std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("lf")));

    if let Err(e) = made {
        // 権限がない（Windows の ERROR_PRIVILEGE_NOT_HELD = 1314）ときだけ飛ばす。ほかの失敗はテストの失敗
        if cfg!(windows) && e.raw_os_error() == Some(1314) {
            skip(
                "symbolic_links_are_rejected",
                "シンボリックリンクを作る権限がない（OS のエラー 1314。開発者モードか管理者の権限が要る）",
            );
            return;
        }
        panic!("シンボリックリンクを作れない：{e}");
    }
    for input in ["ld", "ld/secret.txt", "ld/new.txt", "lf"] {
        expect_code(vault.resolve(input), ErrorCode::OutsideVault, input);
    }
}

/// 短い名前が作られない PC でも、`dir /x` の出力の読み方は確かめておく。
#[test]
fn dir_x_output_is_parsed() {
    use common::parse_short_name;

    let text = "\
2026/11/16  02:01    <DIR>                       .
2026/11/16  02:01    <DIR>          VAULT~1      .vault
2026/11/16  02:01    <DIR>                       x.vault
2026/11/16  02:01                 1 LONGFI~1.TXT longfilename-for-shortname-test.txt
2026/11/16  02:01    <DIR>          LONGFO~1     Long Folder Name For Test
2026/11/16  02:01    <DIR>                       short
";
    assert_eq!(
        parse_short_name(text, ".vault"),
        Some(Some("VAULT~1".to_owned()))
    );
    assert_eq!(
        parse_short_name(text, "longfilename-for-shortname-test.txt"),
        Some(Some("LONGFI~1.TXT".to_owned()))
    );
    assert_eq!(
        parse_short_name(text, "Long Folder Name For Test"),
        Some(Some("LONGFO~1".to_owned()))
    );
    assert_eq!(parse_short_name(text, "short"), Some(None));
    assert_eq!(parse_short_name(text, "missing"), None);
}

#[cfg(windows)]
#[test]
fn short_names_are_rejected() {
    use common::{ShortName, short_name, skip};

    let dir = TestDir::new("resolve-short");
    // cmd に渡すので、先頭に \\?\ の付かない形のパスを使う
    let (root, vault) = new_vault(&dir, "v");
    let long_file = "longfilename-for-shortname-test.txt";
    let long_dir = "Long Folder Name For Test";
    fs::write(root.join(long_file), b"x").unwrap();
    fs::create_dir(root.join(long_dir)).unwrap();
    // 長い名前では通る
    assert!(vault.resolve(long_file).is_ok());
    assert!(vault.resolve(&format!("{long_dir}/x")).is_ok());

    // dir /x が成功し、一覧に出たうえで、短い名前の欄が空のときだけ飛ばす
    let file_short = match short_name(&root, long_file) {
        ShortName::Name(name) => name,
        ShortName::None => {
            skip(
                "short_names_are_rejected",
                "dir /x は成功したが、短い名前の欄が空（このボリュームでは 8.3 形式の名前が作られない）",
            );
            return;
        }
    };
    // 短い名前で開くと、同じファイルを指してしまう（だから拒否する）
    assert_eq!(fs::read(root.join(&file_short)).unwrap(), b"x");
    expect_code(
        vault.resolve(&file_short),
        ErrorCode::InvalidPath,
        &file_short,
    );

    let ShortName::Name(dir_short) = short_name(&root, long_dir) else {
        panic!("ファイルには短い名前があるのに、フォルダにない");
    };
    let input = format!("{dir_short}/x");
    expect_code(vault.resolve(&input), ErrorCode::InvalidPath, &input);

    // 管理用のフォルダ .vault の短い名前（例：VAULT~1）でも入れない
    let ShortName::Name(vault_short) = short_name(&root, ".vault") else {
        panic!("ほかの名前には短い名前があるのに、.vault にない");
    };
    for input in [vault_short.clone(), format!("{vault_short}/vault.json")] {
        expect_code(vault.resolve(&input), ErrorCode::InvalidPath, &input);
    }
}
