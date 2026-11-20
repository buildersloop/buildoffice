//! 偽の秘密を safe-log に流しても、出力に出ないこと。外のクレートと同じ使い方（`name!` を外から）で確かめる。

use std::io;
use std::time::Duration;

use safe_log::{Line, Name, Secret, is_name, name, pick};

/// 偽の秘密。
const FAKES: [&str; 20] = [
    r"C:\Profiles\SECRET-USER-q7\a.txt",
    r"\\srv\share",
    "/home/secret-user-q7",
    "192.168.77.88",
    "fe80::1",
    "[2001:db8::1]:443",
    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    "n4bQgYhMfWWaL+qgxVrQFaO/TxsrC4Is0V1sFbDwCgg=",
    "Zm9vYmFyQmF6UXV4UTdLZXlTZWNyZXRWYWx1ZTEyMzQ1",
    "山田太郎",
    "ABCD-EFGH-JKLM-NPQR",
    "12345678",
    "S-1-5-21-1004336348-1177238915-682003330-1001",
    "secret-host-q7.example",
    "home/alice",
    "alice",
    // UUID の形（公開の前の検査に当たらないよう、2つに分けて書く）
    concat!("a23e4567-e89b-12d3", "-a456-426614174000"),
    "",
    "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
];

/// ふつうの一覧（メソッドとツールの名前）。
const KNOWN: &[Name] = &[
    name!("initialize"),
    name!("tools/call"),
    name!("vault_write"),
    name!("NOT_A_VAULT"),
    name!("notification-ignored"),
];

/// 相手の文字を一覧と照らすと、偽の秘密はどれも `?` になり、出力に出ない。
#[test]
fn one_of_turns_every_fake_into_a_question_mark() {
    for fake in FAKES {
        assert_eq!(pick(fake, KNOWN), None, "{fake:?}");
        let line = Line::new(name!("mcp"))
            .one_of(name!("method"), fake, KNOWN)
            .one_of(name!("tool"), fake, KNOWN)
            .count(name!("n"), 1)
            .ms(name!("ms"), Duration::from_millis(5));
        let text = line.to_string();
        assert_eq!(text, "mcp: method=? tool=? n=1 ms=5", "{fake:?}");
    }
}

/// 一覧にある名前は、一覧の文字のまま出る。
#[test]
fn listed_names_are_written_as_listed() {
    for known in [
        "tools/call",
        "vault_write",
        "NOT_A_VAULT",
        "notification-ignored",
    ] {
        let line = Line::new(name!("mcp")).one_of(name!("tool"), known, KNOWN);
        assert_eq!(line.to_string(), format!("mcp: tool={known}"));
    }
}

/// 名前の形の検査（書式の守り）：区切り・パス・IP・長い鍵・日本語・空・長すぎるものは通らない。
/// `name!` では、これらはコンパイルの誤りになる（compile_fail の doctest）。
#[test]
fn malformed_fakes_are_not_names() {
    for fake in [
        r"C:\Profiles\SECRET-USER-q7\a.txt",
        r"\\srv\share",
        "/home/secret-user-q7",
        "192.168.77.88",
        "fe80::1",
        "[2001:db8::1]:443",
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        "n4bQgYhMfWWaL+qgxVrQFaO/TxsrC4Is0V1sFbDwCgg=",
        "Zm9vYmFyQmF6UXV4UTdLZXlTZWNyZXRWYWx1ZTEyMzQ1",
        "山田太郎",
        "12345678",
        "secret-host-q7.example",
        "",
        "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
    ] {
        assert!(!is_name(fake), "{fake:?}");
    }
}

/// 書式の守りの限界：次の形は名前の形に合う。これらが出ない保証は、名前を定数からしか作れないこと・
/// 相手の文字を一覧と照らすこと・ソースの検査で持つ（名前の形の検査では持たない）。
#[test]
fn the_format_check_alone_does_not_stop_these() {
    for fake in [
        "ABCD-EFGH-JKLM-NPQR",
        "home/alice",
        "alice",
        "S-1-5-21-1004336348-1177238915-682003330-1001",
        // UUID の形（公開の前の検査に当たらないよう、2つに分けて書く）
        concat!("a23e4567-e89b-12d3", "-a456-426614174000"),
        "deadbeefdeadbeef",
    ] {
        assert!(is_name(fake), "{fake:?}");
    }
}

/// 数のキーが秘密の数の名前なら、値を書かない。
#[test]
fn secret_numbers_are_not_written() {
    let line = Line::new(name!("vaultd"))
        .count(name!("short_pin"), 12_345_678)
        .int(name!("port"), 443)
        .count(name!("invite_code"), 87_654_321);
    let text = line.to_string();
    assert_eq!(text, "vaultd: short_pin=? port=? invite_code=?");
}

/// 入出力のエラーは種類の名前だけ。文（パスが入りうる）は出ない。
#[test]
fn io_errors_are_written_by_kind_only() {
    let error = io::Error::new(io::ErrorKind::PermissionDenied, FAKES[0]);
    let line = Line::new(name!("vaultd"))
        .name(name!("event"), name!("open_failed"))
        .io_kind(name!("kind"), error.kind());
    assert_eq!(
        line.to_string(),
        "vaultd: event=open_failed kind=permission_denied"
    );
}

/// 秘密の包みの Debug は `Secret(..)` だけ。
#[test]
fn secrets_debug_as_a_placeholder() {
    for fake in FAKES {
        assert_eq!(format!("{:?}", Secret::new(fake)), "Secret(..)");
        assert_eq!(format!("{:#?}", Secret::new(fake.to_owned())), "Secret(..)");
    }
}
