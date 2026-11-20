//! 点検での指摘の直しを確かめる：パスの深さと長さ、ACCESS_DENIED と古い中身の居場所、
//! 履歴を先に開くこと、読み取り専用の印、大文字小文字だけ違う索引のキー、1文字分の読み、actor の id、16 MiB の判定。

mod common;

use std::fs;

use common::{TestDir, call, expect_code, journal, new_vault, set_read_only};
use serde_json::{Value, json};
use vault_core::{
    Actor, ErrorCode, MAX_FULL_PATH_UTF16, MAX_PATH_DEPTH, MAX_WRITE_BASE64_CHARS, Op,
};

use common::root_len;

/// 1. 要素は64個まで。根＋区切り＋相対パスは UTF-16 で260未満。どちらも超えたら INVALID_PATH。
#[test]
fn paths_that_are_too_deep_or_too_long_are_rejected() {
    let dir = TestDir::new("safety-limits");
    let (path, vault) = new_vault(&dir, "v");

    let deep_ok = vec!["a"; MAX_PATH_DEPTH].join("/");
    call(
        &vault,
        "vault_write",
        json!({"path": deep_ok, "content": "x"}),
    )
    .unwrap();
    let deep_ng = vec!["a"; MAX_PATH_DEPTH + 1].join("/");
    let err = call(
        &vault,
        "vault_write",
        json!({"path": deep_ng, "content": "x"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidPath);
    assert!(err.message().contains("深すぎる"), "{err}");
    expect_code(
        call(&vault, "vault_read", json!({"path": deep_ng})),
        ErrorCode::InvalidPath,
        "65段を読む",
    );

    // 長さの境目：根＋1＋相対パス が 259 なら通り、260 なら拒否
    let max_rel = MAX_FULL_PATH_UTF16 - 1 - root_len(&path) - 1;
    assert!(max_rel < 255, "テストの場所が長すぎる");
    let ok_name = "x".repeat(max_rel);
    call(
        &vault,
        "vault_write",
        json!({"path": ok_name, "content": "ok"}),
    )
    .unwrap();
    assert_eq!(
        call(&vault, "vault_read", json!({"path": ok_name})).unwrap()["content"],
        "ok"
    );
    let ng_name = "x".repeat(max_rel + 1);
    let err = call(
        &vault,
        "vault_write",
        json!({"path": ng_name, "content": "x"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidPath);
    assert!(err.message().contains("長すぎる"), "{err}");
    assert!(err.message().contains("1 文字多い"), "{err}");
    // フォルダの下でも同じ（"d/" の2文字ぶん短くなる）
    let ng_nested = format!("d/{}", "y".repeat(max_rel - 1));
    expect_code(
        call(&vault, "vault_mkdir", json!({"path": ng_nested})),
        ErrorCode::InvalidPath,
        "フォルダの下",
    );
    expect_code(
        call(&vault, "vault_list", json!({"path": ng_name})),
        ErrorCode::InvalidPath,
        "一覧",
    );
}

/// 3・13. 上書きで、新しい中身も古い中身ももとの場所に置けないとき（親フォルダにファイルを作れない）、
/// ACCESS_DENIED で、古い中身の居場所（previous_trash_id）がエラーの文と履歴（write_failed）からわかる。
#[cfg(windows)]
#[test]
fn a_failed_overwrite_reports_where_the_old_content_went() {
    use common::DenyAce;

    let dir = TestDir::new("safety-access-denied");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "d/f.txt", "content": "old"}),
    )
    .unwrap();
    let before = journal(&path).len();

    let err = {
        // d の中に、ファイルを作れない（(WD)）。f.txt をゴミ箱へ移すことはできる
        let _deny = DenyAce::new(&path.join("d"), "(WD)");
        let err = call(
            &vault,
            "vault_write",
            json!({"path": "d/f.txt", "content": "new"}),
        )
        .unwrap_err();
        // 新しいファイルも作れない（ゴミ箱には何も入らない）
        expect_code(
            call(
                &vault,
                "vault_write",
                json!({"path": "d/g.txt", "content": "g"}),
            ),
            ErrorCode::AccessDenied,
            "新しいファイル",
        );
        err
    };
    assert_eq!(err.code(), ErrorCode::AccessDenied, "{err}");
    let message = err.message();
    let trash_id = message
        .split("previous_trash_id: ")
        .nth(1)
        .and_then(|rest| rest.split('。').next())
        .unwrap_or_else(|| panic!("エラーの文に previous_trash_id がない：{message}"))
        .to_owned();
    let data = path
        .join(".vault")
        .join("trash")
        .join(&trash_id)
        .join("data");
    assert_eq!(
        fs::read_to_string(data).unwrap(),
        "old",
        "古い中身はゴミ箱にある"
    );
    assert!(!path.join("d").join("f.txt").exists());
    assert!(!path.join("d").join("g.txt").exists());

    let entries = journal(&path);
    assert_eq!(
        entries.len(),
        before + 1,
        "書けなかった新しいファイルは履歴に残さない"
    );
    let last = entries.last().unwrap();
    assert_eq!(last.op, Op::WriteFailed);
    assert_eq!(last.path.as_deref(), Some("d/f.txt"));
    assert_eq!(last.trash_id.as_deref(), Some(trash_id.as_str()));
    // 索引からも除く。一時ファイルは残さない
    let index: Value =
        serde_json::from_slice(&fs::read(path.join(".vault").join("index.json")).unwrap()).unwrap();
    assert!(index.get("d/f.txt").is_none());
    assert_eq!(
        fs::read_dir(path.join(".vault").join("tmp"))
            .unwrap()
            .count(),
        0
    );
}

/// 4. 変更の前に履歴のファイルを開く：履歴に書けなければ、何も変えずにエラーにする。
#[test]
fn nothing_changes_when_the_journal_cannot_be_opened() {
    let dir = TestDir::new("safety-journal-first");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "old"}),
    )
    .unwrap();
    let journal_file = fs::read_dir(path.join(".vault").join("journal"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    set_read_only(&journal_file, true);
    let result = call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "new"}),
    );
    let mkdir = call(&vault, "vault_mkdir", json!({"path": "d"}));
    let moved = call(
        &vault,
        "vault_move",
        json!({"from": "a.txt", "to": "b.txt"}),
    );
    set_read_only(&journal_file, false);
    for r in [result, mkdir, moved] {
        assert!(r.is_err(), "{r:?}");
    }
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "old");
    assert!(!path.join("d").exists() && !path.join("b.txt").exists());
    assert_eq!(
        fs::read_dir(path.join(".vault").join("trash"))
            .unwrap()
            .count(),
        0,
        "ゴミ箱にも何も入らない"
    );
}

/// 項目4：変更のあとで、索引だけが直せないときは、ツールは成功にする。古い索引は消して、(h) の走査に作り直させる。
/// Windows では、読み取り専用の印が付いた索引は、rename で置き換えられない。
#[cfg(windows)]
#[test]
fn a_failed_index_update_does_not_fail_the_tool() {
    let dir = TestDir::new("safety-index");
    let (path, vault) = new_vault(&dir, "v");
    let index_file = path.join(".vault").join("index.json");
    set_read_only(&index_file, true);
    let w = call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    );
    let still_there = index_file.exists();
    if still_there {
        set_read_only(&index_file, false);
    }
    let w = w.unwrap();
    assert_eq!(w["created"], true);
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "a");
    assert_eq!(journal(&path).last().unwrap().op, Op::Write, "履歴には残る");
    assert!(!still_there, "直せなかった索引は消して、走査に作り直させる");
    assert!(!index_file.exists());
}

/// 5. 読み取り専用の印が付いたファイルは、上書きも移動もしない（ACCESS_DENIED）。
#[test]
fn files_marked_read_only_are_not_overwritten_or_moved() {
    let dir = TestDir::new("safety-read-only-attr");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "keep.txt", "content": "keep"}),
    )
    .unwrap();
    let file = path.join("keep.txt");
    set_read_only(&file, true);
    let write = call(
        &vault,
        "vault_write",
        json!({"path": "keep.txt", "content": "x"}),
    );
    let moved = call(
        &vault,
        "vault_move",
        json!({"from": "keep.txt", "to": "k2.txt"}),
    );
    let before = journal(&path).len();
    set_read_only(&file, false);
    expect_code(write, ErrorCode::AccessDenied, "上書き");
    expect_code(moved, ErrorCode::AccessDenied, "移動");
    assert_eq!(fs::read_to_string(&file).unwrap(), "keep");
    assert!(!path.join("k2.txt").exists());
    assert_eq!(
        fs::read_dir(path.join(".vault").join("trash"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(journal(&path).len(), before);
    // 印を外せば書ける
    call(
        &vault,
        "vault_write",
        json!({"path": "keep.txt", "content": "x"}),
    )
    .unwrap();
}

/// 6. 上書きで今あるファイルが見つかったら、大文字小文字だけ違う索引のキーも除く（Windows）。
#[cfg(windows)]
#[test]
fn overwriting_with_a_different_case_keeps_one_index_key() {
    let dir = TestDir::new("safety-case");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "Note.TXT", "content": "1"}),
    )
    .unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "note.txt", "content": "2"}),
    )
    .unwrap();
    let index: Value =
        serde_json::from_slice(&fs::read(path.join(".vault").join("index.json")).unwrap()).unwrap();
    let keys: Vec<&String> = index.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["note.txt"]);
}

/// 7. utf8 で max_bytes が1文字より小さいときは、1文字分（最大4バイト）だけ読んで返す。
#[test]
fn one_character_is_returned_when_max_bytes_is_smaller_than_it() {
    let dir = TestDir::new("safety-one-char");
    let (_path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "jp.txt", "content": "あい𝄞"}),
    )
    .unwrap();
    let read = |offset: u64, max: u64| {
        call(
            &vault,
            "vault_read",
            json!({"path": "jp.txt", "offset": offset, "max_bytes": max}),
        )
        .unwrap()
    };
    let r = read(0, 1);
    assert_eq!(
        (r["content"].as_str(), r["truncated"].as_bool()),
        (Some("あ"), Some(true))
    );
    let r = read(3, 2);
    assert_eq!(
        (r["content"].as_str(), r["truncated"].as_bool()),
        (Some("い"), Some(true))
    );
    let r = read(6, 3);
    assert_eq!(
        (r["content"].as_str(), r["truncated"].as_bool()),
        (Some("𝄞"), Some(false))
    );
    // 0 バイトのときは何も読まない（大きさと版だけを知るため）
    let r = read(0, 0);
    assert_eq!(
        (r["content"].as_str(), r["truncated"].as_bool()),
        (Some(""), Some(true))
    );
    // 文字の途中から読むと NOT_UTF8 のまま
    expect_code(
        call(
            &vault,
            "vault_read",
            json!({"path": "jp.txt", "offset": 1, "max_bytes": 1}),
        ),
        ErrorCode::NotUtf8,
        "文字の途中",
    );
}

/// 11. actor の id は、コアでも整える（印字できる ASCII だけ、64字まで、空なら unknown）。
#[test]
fn actor_ids_are_sanitized_before_they_reach_the_journal() {
    let dir = TestDir::new("safety-actor");
    let (path, vault) = new_vault(&dir, "v");
    let args = json!({"path": "a.txt", "content": "a"});
    vault_core::api::call(
        &vault,
        "vault_write",
        &args,
        &Actor::ai("mcp:\u{202E}evil\nname"),
    )
    .unwrap();
    vault_core::api::call(
        &vault,
        "vault_mkdir",
        &json!({"path": "d"}),
        &Actor::ai("\u{0}\u{7}"),
    )
    .unwrap();
    let entries = journal(&path);
    let ids: Vec<&str> = entries.iter().rev().take(2).map(|e| e.actor.id()).collect();
    assert_eq!(ids, ["unknown", "mcp:evilname"]);
}

/// 12. base64 の入力が 16 MiB 分の長さを超えたら、広げる前に TOO_LARGE。ちょうどは書ける。
#[test]
fn base64_input_length_is_checked_before_decoding() {
    let dir = TestDir::new("safety-base64-size");
    let (path, vault) = new_vault(&dir, "v");
    let too_long = "A".repeat(MAX_WRITE_BASE64_CHARS + 4);
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "big.bin", "content": too_long, "encoding": "base64"}),
        ),
        ErrorCode::TooLarge,
        "長すぎる base64",
    );
    let exact = vault_core::base64::encode(&vec![7u8; 16 * 1024 * 1024]);
    assert_eq!(exact.len(), MAX_WRITE_BASE64_CHARS);
    call(
        &vault,
        "vault_write",
        json!({"path": "big.bin", "content": exact, "encoding": "base64"}),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(path.join("big.bin")).unwrap().len(),
        16 * 1024 * 1024
    );
}

/// 再確認 中A：フォルダを移すとき、中のいちばん深いもの・長いものが、移す先で上限に収まるかを、
/// 何も変える前に確かめる（深さ64・長さ260未満。resolve の上限と同じ計算）。
#[test]
fn moving_a_folder_checks_the_depth_and_length_of_its_contents() {
    let dir = TestDir::new("mvx");
    let (path, vault) = new_vault(&dir, "v");
    let chain = |n: usize| vec!["d"; n].join("/");

    // x の中は、x からの深さ 31（d が30段と、その下の f.txt）
    call(
        &vault,
        "vault_write",
        json!({"path": format!("x/{}/f.txt", chain(30)), "content": "deep"}),
    )
    .unwrap();
    // もう1つの木：y の下に d が32段
    call(
        &vault,
        "vault_mkdir",
        json!({"path": format!("y/{}", chain(32))}),
    )
    .unwrap();
    let before = journal(&path).len();

    // y/d×32/x へ移すと、いちばん深いものは 34 + 31 = 65 段 → 拒否して、何も変えない
    let err = call(
        &vault,
        "vault_move",
        json!({"from": "x", "to": format!("y/{}/x", chain(32))}),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidPath, "{err}");
    assert!(err.message().contains("深すぎる"), "{err}");
    assert!(path.join("x").is_dir());
    assert_eq!(journal(&path).len(), before, "履歴も変わらない");

    // y/d×31/x へは、ぴったり64段なので移せる
    let to = format!("y/{}/x", chain(31));
    call(&vault, "vault_move", json!({"from": "x", "to": to})).unwrap();
    let deepest = format!("{to}/{}/f.txt", chain(30));
    assert_eq!(deepest.split('/').count(), MAX_PATH_DEPTH);
    assert_eq!(
        call(&vault, "vault_read", json!({"path": deepest})).unwrap()["content"],
        "deep"
    );

    // 長さ：z/<名前> へ移したとき、中のいちばん長いものが 根＋…＝259 なら移せて、260 なら拒否
    call(&vault, "vault_mkdir", json!({"path": "z"})).unwrap();
    let fit = MAX_FULL_PATH_UTF16 - 1 - root_len(&path) - 1 - "z/a1".len() - 1;
    assert!(fit < 255, "テストの場所が長すぎる");
    call(
        &vault,
        "vault_write",
        json!({"path": format!("a1/{}", "n".repeat(fit)), "content": "1"}),
    )
    .unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": format!("a2/{}", "n".repeat(fit + 1)), "content": "2"}),
    )
    .unwrap();
    let err = call(&vault, "vault_move", json!({"from": "a2", "to": "z/a2"})).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidPath, "{err}");
    assert!(err.message().contains("長すぎる"), "{err}");
    assert!(err.message().contains("1 文字多い"), "{err}");
    assert!(path.join("a2").is_dir() && !path.join("z").join("a2").exists());
    call(&vault, "vault_move", json!({"from": "a1", "to": "z/a1"})).unwrap();
    assert!(path.join("z").join("a1").is_dir());
}
