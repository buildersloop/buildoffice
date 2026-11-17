//! 乱数の操作（不変条件 (b)）。
//!
//! 種を固定した自作の xorshift で、種5つ × 500回、write・上書き・mkdir・move・delete・restore を混ぜる。
//! そのあとで、消したもの・上書きしたものが、全部ゴミ箱から同じ中身で戻せること、履歴の seq が連番であること、
//! 保管庫の中にリンクが1つもないこと（不変条件 (b)：どのツールもリンクを作らない）、
//! 走査（vault_changes の rescan）でコアの操作が外での変更として二重に出ないことを確かめる。

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::thread;

use common::{TestDir, call, journal, new_vault};
use serde_json::{Value, json};
use vault_core::Vault;

const SEEDS: [u64; 5] = [1, 42, 2026, 0xDEAD_BEEF, 0x1234_5678_9ABC];
const STEPS: usize = 500;

/// 自作の xorshift64（外部の依存を使わない）。
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn text(&mut self) -> String {
        let len = self.below(40);
        (0..len)
            .map(|_| char::from(b'a' + self.below(26) as u8))
            .collect()
    }
}

/// パス（ファイルかフォルダ）の中身の写し：相対パス → 中身（フォルダは None）。
type Snapshot = BTreeMap<String, Option<Vec<u8>>>;

fn snapshot(abs: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    let meta = fs::symlink_metadata(abs).unwrap();
    if meta.is_file() {
        out.insert(String::new(), Some(fs::read(abs).unwrap()));
        return out;
    }
    out.insert(String::new(), None);
    let mut stack = vec![(abs.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                out.insert(child_rel.clone(), None);
                stack.push((entry.path(), child_rel));
            } else {
                out.insert(child_rel, Some(fs::read(entry.path()).unwrap()));
            }
        }
    }
    out
}

/// 消したもの・上書きしたものの記録（trash_id → もとの場所と中身の写し）。
struct Trashed {
    trash_id: String,
    original: String,
    snapshot: Snapshot,
}

fn entries(vault: &Vault) -> (Vec<String>, Vec<String>) {
    let list = call(
        vault,
        "vault_list",
        json!({"recursive": true, "limit": 10000}),
    )
    .unwrap();
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for e in list["entries"].as_array().unwrap() {
        let path = e["path"].as_str().unwrap().to_owned();
        match e["kind"].as_str().unwrap() {
            "file" => files.push(path),
            "dir" => dirs.push(path),
            other => panic!("ツールはリンクを作らないはず：{other} {path}"),
        }
    }
    (files, dirs)
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

/// 中へ入れる先のフォルダ（根か、浅いフォルダ）。
fn pick_parent(rng: &mut XorShift, dirs: &[String], not_inside: Option<&str>) -> String {
    let candidates: Vec<&String> = dirs
        .iter()
        .filter(|d| d.split('/').count() < 6)
        .filter(|d| match not_inside {
            Some(x) => *d != x && !d.starts_with(&format!("{x}/")),
            None => true,
        })
        .collect();
    if candidates.is_empty() || rng.below(3) == 0 {
        String::new()
    } else {
        candidates[rng.below(candidates.len())].clone()
    }
}

/// 戻す先として、もとの場所が使えるか（ふさがっておらず、途中がフォルダか、まだない）。
fn original_is_free(root: &Path, original: &str) -> bool {
    if original.is_empty() || root.join(original).exists() {
        return false;
    }
    let mut prefix = root.to_path_buf();
    let parts: Vec<&str> = original.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        prefix.push(part);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return false,
            Err(_) => return true,
        }
    }
    true
}

fn run_seed(seed: u64) {
    let dir = TestDir::new(&format!("random-{seed:x}"));
    let (path, vault) = new_vault(&dir, "v");
    let mut rng = XorShift(seed);
    let mut trashed: Vec<Trashed> = Vec::new();
    let mut replaced_count = 0usize;
    let ctx = |step: usize, op: &str| format!("seed {seed:#x} step {step} {op}");

    for step in 0..STEPS {
        let (files, dirs) = entries(&vault);
        let all: Vec<&String> = files.iter().chain(dirs.iter()).collect();
        let mut op = rng.below(6);
        if (op == 1 && files.is_empty())
            || (matches!(op, 3 | 4) && all.is_empty())
            || (op == 5 && trashed.is_empty())
        {
            op = 0;
        }
        match op {
            // 新しいファイルを書く
            0 => {
                let parent = pick_parent(&mut rng, &dirs, None);
                let content = rng.text();
                call(
                    &vault,
                    "vault_write",
                    json!({"path": join(&parent, &format!("f{step}.txt")), "content": content}),
                )
                .unwrap_or_else(|e| panic!("{}: {e}", ctx(step, "write")));
            }
            // 上書き（古い中身がゴミ箱へ）
            1 => {
                let target = files[rng.below(files.len())].clone();
                let before = snapshot(&path.join(&target));
                let content = rng.text();
                let w = call(
                    &vault,
                    "vault_write",
                    json!({"path": target, "content": content}),
                )
                .unwrap_or_else(|e| panic!("{}: {e}", ctx(step, "overwrite")));
                trashed.push(Trashed {
                    trash_id: w["previous_trash_id"].as_str().unwrap().to_owned(),
                    original: target,
                    snapshot: before,
                });
            }
            // フォルダを作る
            2 => {
                let parent = pick_parent(&mut rng, &dirs, None);
                call(
                    &vault,
                    "vault_mkdir",
                    json!({"path": join(&parent, &format!("d{step}"))}),
                )
                .unwrap_or_else(|e| panic!("{}: {e}", ctx(step, "mkdir")));
            }
            // 移す
            3 => {
                let from = all[rng.below(all.len())].clone();
                let parent = pick_parent(&mut rng, &dirs, Some(&from));
                let to = join(&parent, &format!("m{step}"));
                call(&vault, "vault_move", json!({"from": from, "to": to}))
                    .unwrap_or_else(|e| panic!("{}: {e}", ctx(step, "move")));
            }
            // 消す（フォルダは中身ごと）
            4 => {
                let target = all[rng.below(all.len())].clone();
                let before = snapshot(&path.join(&target));
                let d = call(
                    &vault,
                    "vault_delete",
                    json!({"path": target, "recursive": true}),
                )
                .unwrap_or_else(|e| panic!("{}: {e}", ctx(step, "delete")));
                trashed.push(Trashed {
                    trash_id: d["trash_id"].as_str().unwrap().to_owned(),
                    original: target,
                    snapshot: before,
                });
            }
            // ゴミ箱から戻す：もとの場所が使えれば、もとへ。もとの場所がふさがっていれば、半分は
            // CONFLICT を確かめてから replace:true で入れ替え（入れ替えた中身も記録する）、残りは to で別の場所へ
            _ => {
                let t = trashed.remove(rng.below(trashed.len()));
                let occupied =
                    !t.original.is_empty() && fs::symlink_metadata(path.join(&t.original)).is_ok();
                let (args, dest) = if original_is_free(&path, &t.original) {
                    (json!({"trash_id": t.trash_id}), t.original.clone())
                } else if occupied && rng.below(2) == 0 {
                    let conflict = call(&vault, "vault_restore", json!({"trash_id": t.trash_id}))
                        .expect_err(&ctx(step, "CONFLICT になるはず"));
                    assert_eq!(
                        conflict.code(),
                        vault_core::ErrorCode::Conflict,
                        "{conflict}"
                    );
                    let current = snapshot(&path.join(&t.original));
                    let args = json!({"trash_id": t.trash_id, "replace": true});
                    let r = call(&vault, "vault_restore", args)
                        .unwrap_or_else(|e| panic!("{}: {e}", ctx(step, "replace")));
                    trashed.push(Trashed {
                        trash_id: r["replaced_trash_id"].as_str().unwrap().to_owned(),
                        original: t.original.clone(),
                        snapshot: current,
                    });
                    replaced_count += 1;
                    assert_eq!(
                        snapshot(&path.join(&t.original)),
                        t.snapshot,
                        "{}",
                        ctx(step, "replace の中身")
                    );
                    continue;
                } else {
                    let to = format!("r{step}");
                    (json!({"trash_id": t.trash_id, "to": to}), to)
                };
                let r = call(&vault, "vault_restore", args)
                    .unwrap_or_else(|e| panic!("{}: {e}", ctx(step, "restore")));
                assert_eq!(r["path"], dest.as_str());
                assert_eq!(
                    snapshot(&path.join(&dest)),
                    t.snapshot,
                    "{}",
                    ctx(step, "restore の中身")
                );
            }
        }
    }

    // 消したもの・上書きしたものが、全部ゴミ箱から同じ中身で戻せる
    let remaining = trashed.len();
    for (k, t) in trashed.into_iter().enumerate() {
        let to = format!("final/{k}");
        call(
            &vault,
            "vault_restore",
            json!({"trash_id": t.trash_id, "to": to}),
        )
        .unwrap_or_else(|e| panic!("seed {seed:#x} 最後の restore {k}: {e}"));
        assert_eq!(snapshot(&path.join(&to)), t.snapshot, "seed {seed:#x} {k}");
    }
    let left = call(&vault, "vault_trash_list", json!({"limit": 10000})).unwrap();
    assert_eq!(left["items"], Value::Array(Vec::new()), "ゴミ箱は空になる");

    // 走査しても、コアの操作は外での変更として出ない（索引が、どの操作のあとも今の状態と合っている）
    let last = journal(&path).last().unwrap().seq;
    let scanned = call(&vault, "vault_changes", json!({"since_seq": last})).unwrap();
    assert_eq!(
        scanned["changes"],
        Value::Array(Vec::new()),
        "seed {seed:#x}: 走査で、コアの操作が二重に出た"
    );

    // 履歴の seq は 1 からの連番
    let seqs: Vec<u64> = journal(&path).iter().map(|e| e.seq).collect();
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(seqs, expected, "seed {seed:#x}");

    // 不変条件 (b)：保管庫の中（.vault を除く）に、リンクが1つもない
    assert_no_links(&path);
    eprintln!(
        "seed {seed:#x}: {STEPS} 回の操作、replace:true の restore {replaced_count} 回、最後に戻したもの {remaining} 個、履歴 {} 行",
        seqs.len()
    );
    assert!(
        replaced_count > 0,
        "seed {seed:#x}: replace の restore が一度も起きなかった"
    );
}

fn assert_no_links(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().eq_ignore_ascii_case(".vault") {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            assert!(
                !meta.file_type().is_symlink(),
                "リンクがある：{}",
                entry.path().display()
            );
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
}

#[test]
fn random_operations_keep_everything_restorable() {
    let handles: Vec<_> = SEEDS
        .iter()
        .map(|&seed| thread::spawn(move || run_seed(seed)))
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn xorshift_is_deterministic() {
    let mut a = XorShift(42);
    let mut b = XorShift(42);
    let xs: Vec<u64> = (0..5).map(|_| a.next()).collect();
    let ys: Vec<u64> = (0..5).map(|_| b.next()).collect();
    assert_eq!(xs, ys);
    assert!(xs.iter().all(|&x| x != 0));
}
