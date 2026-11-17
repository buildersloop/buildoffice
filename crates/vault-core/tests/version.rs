//! 版の出どころ（V1）：vault_write の version・直後の vault_read の version・索引の値が一致する。

mod common;

use std::fs;
use std::path::Path;

use common::{TestDir, call, new_vault};
use serde_json::json;

fn index_version(vault: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(vault.join(".vault").join("index.json")).ok()?;
    let index: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = index.get(key)?;
    Some(format!(
        "{}-{}",
        entry["size"].as_u64()?,
        entry["mtime_ns"].as_i64()?
    ))
}

/// V1：同じファイルを 20 回書く（大きさを変えるもの・変えないもの・続けて書くもの）→ 毎回、vault_write の version と、
/// 直後の vault_read の version と、索引の値が一致する。
#[test]
fn v1_write_read_and_index_versions_agree() {
    let dir = TestDir::new("v1");
    let (path, vault) = new_vault(&dir, "v");
    for i in 0..20 {
        let content = match i % 3 {
            0 => format!("size changes {i}"),
            1 => "same size!".to_owned(),
            _ => "same size?".to_owned(),
        };
        let w = call(
            &vault,
            "vault_write",
            json!({"path": "f.txt", "content": content}),
        )
        .unwrap();
        let r = call(&vault, "vault_read", json!({"path": "f.txt"})).unwrap();
        assert_eq!(w["version"], r["version"], "{i}：書いた版と読んだ版");
        assert_eq!(
            Some(w["version"].as_str().unwrap().to_owned()),
            index_version(&path, "f.txt"),
            "{i}：書いた版と索引"
        );
    }
}
