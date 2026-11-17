//! 空き容量の確かめ：T16・T16b。

#![cfg(windows)]

mod common;

use std::fs;
use std::path::Path;

use common::{TestDir, call, expect_code, journal, new_vault};
use serde_json::json;
use vault_core::ErrorCode;
use vault_core::test_support::{FakeFreeSpace, fake_free_space};

const GIB: u64 = 1 << 30;

fn tmp_count(vault: &Path) -> usize {
    fs::read_dir(vault.join(".vault").join("tmp"))
        .map(|d| d.count())
        .unwrap_or(0)
}

/// T16：書いたあとに 1 GiB 残らないなら DISK_FULL。.vault/tmp は空、履歴は増えず、書く先は変わらず、
/// create_dirs の親のフォルダもできない。ちょうど 1 GiB 残るなら成功する。
#[test]
fn t16_writes_that_leave_less_than_a_gib_are_refused() {
    let dir = TestDir::new("t16");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "old"}),
    )
    .unwrap();
    let before = journal(&path).len();
    let content = "0123456789";
    let len = content.len() as u64;

    fake_free_space(FakeFreeSpace::Bytes(len + GIB - 1));
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": content}),
        ),
        ErrorCode::DiskFull,
        "上書きで 1 GiB 残らない",
    );
    fake_free_space(FakeFreeSpace::Bytes(len + GIB - 1));
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "a/b/g.txt", "content": content}),
        ),
        ErrorCode::DiskFull,
        "新しいフォルダの下で 1 GiB 残らない",
    );
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), "old");
    assert!(!path.join("a").exists(), "親のフォルダができた");
    assert_eq!(tmp_count(&path), 0);
    assert_eq!(journal(&path).len(), before, "履歴が増えた");

    fake_free_space(FakeFreeSpace::Bytes(len + GIB));
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": content}),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(path.join("f.txt")).unwrap(), content);
}

/// T16b：空き容量を問い合わせられなければ IO_ERROR で、何も変えない。
#[test]
fn t16b_a_failed_free_space_query_is_an_io_error() {
    let dir = TestDir::new("t16b");
    let (path, vault) = new_vault(&dir, "v");
    let before = journal(&path).len();
    fake_free_space(FakeFreeSpace::Fail);
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "n/f.txt", "content": "x"}),
        ),
        ErrorCode::IoError,
        "空き容量を問い合わせられない",
    );
    assert!(!path.join("n").exists());
    assert_eq!(tmp_count(&path), 0);
    assert_eq!(journal(&path).len(), before);
}
