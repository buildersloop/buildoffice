//! 走査の名前の変更の組を深い位置にも広げる。
//!
//! 新しいフォルダを作ってファイルを移す・フォルダから出して元を消す・フォルダを新しいフォルダの下へ移す、が
//! 削除と作成でなく名前の変更として出る。深い組は、名前が同じで、種類・大きさ・更新時刻が同じもの（フォルダは
//! 更新時刻が同じもの）。先の親のフォルダは、組の前に作成として書く（前から順に当てれば今の状態になる）。

#![cfg(windows)]

mod common;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use common::{TestDir, call, env, journal, new_vault};
use serde_json::{Value, json};
use vault_core::Vault;
use vault_core::test_support::{Step, on_next};

/// 1つの変更（op・path・to）。
type Change = (String, String, Option<String>);

fn change(op: &str, path: &str, to: Option<&str>) -> Change {
    (op.to_owned(), path.to_owned(), to.map(str::to_owned))
}

/// 走査のテスト用の保管庫（続けて走査できるように、間隔を 0 にする）。
fn scan_vault(dir: &TestDir, name: &str) -> (PathBuf, Vault) {
    let (path, mut vault) = new_vault(dir, name);
    vault.set_min_rescan_interval(Duration::ZERO);
    vault.set_rescan_interval_factor(0);
    (path, vault)
}

/// 走査し、履歴の最後の seq から後の外での変更を返す（limit で切らないように、履歴のファイルから読む）。
fn rescan_all(vault: &Vault, path: &Path) -> Vec<Change> {
    let since = journal(path).last().map_or(0, |e| e.seq);
    let r = call(
        vault,
        "vault_changes",
        json!({"since_seq": since, "limit": 1}),
    )
    .unwrap();
    assert_eq!(r["scanned"], true, "{r}");
    journal(path)
        .into_iter()
        .filter(|e| e.seq > since)
        .map(|e| {
            let op = serde_json::to_value(e.op)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            (op, e.path.unwrap_or_default(), e.to)
        })
        .collect()
}

/// 外のアプリの書き込みと同じ値（大きさと更新時刻）のファイルを作る。
fn write_at(path: &Path, bytes: &[u8], modified: SystemTime) {
    fs::write(path, bytes).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

fn fixed_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// D1：新しいフォルダ n を作って a.txt を n/a.txt へ移す → [create n, rename a.txt → n/a.txt]。
#[test]
fn d1_moving_a_file_into_a_new_folder_is_a_rename() {
    let dir = TestDir::new("sr-d1");
    let (path, vault) = scan_vault(&dir, "v");
    fs::write(path.join("a.txt"), b"a").unwrap();
    rescan_all(&vault, &path);
    fs::create_dir(path.join("n")).unwrap();
    fs::rename(path.join("a.txt"), path.join("n").join("a.txt")).unwrap();
    assert_eq!(
        rescan_all(&vault, &path),
        [
            change("external_create", "n", None),
            change("external_rename", "a.txt", Some("n/a.txt")),
        ]
    );
    assert!(rescan_all(&vault, &path).is_empty());
}

/// D2：d/x.txt を x.txt へ出して d を消す → [rename d/x.txt → x.txt, delete d]。
#[test]
fn d2_moving_a_file_out_of_a_removed_folder_is_a_rename() {
    let dir = TestDir::new("sr-d2");
    let (path, vault) = scan_vault(&dir, "v");
    fs::create_dir(path.join("d")).unwrap();
    fs::write(path.join("d").join("x.txt"), b"x").unwrap();
    rescan_all(&vault, &path);
    fs::rename(path.join("d").join("x.txt"), path.join("x.txt")).unwrap();
    fs::remove_dir(path.join("d")).unwrap();
    assert_eq!(
        rescan_all(&vault, &path),
        [
            change("external_rename", "d/x.txt", Some("x.txt")),
            change("external_delete", "d", None),
        ]
    );
}

/// D3：フォルダ f（中に x.txt）を新しいフォルダ n の下へ移す → [create n, rename f → n/f]。
#[test]
fn d3_moving_a_folder_into_a_new_folder_is_a_rename() {
    let dir = TestDir::new("sr-d3");
    let (path, vault) = scan_vault(&dir, "v");
    fs::create_dir(path.join("f")).unwrap();
    fs::write(path.join("f").join("x.txt"), b"x").unwrap();
    rescan_all(&vault, &path);
    fs::create_dir(path.join("n")).unwrap();
    fs::rename(path.join("f"), path.join("n").join("f")).unwrap();
    assert_eq!(
        rescan_all(&vault, &path),
        [
            change("external_create", "n", None),
            change("external_rename", "f", Some("n/f")),
        ]
    );
}

/// D4 の1回：p/x.txt と q/x.txt（同じ名前・同じ値）を、新しい m・n の下へ移した保管庫を走査した結果。
fn d4_once(dir: &TestDir, name: &str) -> Vec<Change> {
    let (path, vault) = scan_vault(dir, name);
    for d in ["p", "q"] {
        fs::create_dir(path.join(d)).unwrap();
        write_at(&path.join(d).join("x.txt"), b"same", fixed_time());
    }
    rescan_all(&vault, &path);
    for (from, to) in [("p", "m"), ("q", "n")] {
        fs::create_dir(path.join(to)).unwrap();
        fs::rename(path.join(from).join("x.txt"), path.join(to).join("x.txt")).unwrap();
    }
    rescan_all(&vault, &path)
}

/// D4：同じ名前・同じ値のファイル2つを新しい2つのフォルダへ移す → どちらも名前の変更。2回流して同じ結果。
#[test]
fn d4_two_identical_files_moved_into_new_folders_are_both_renames() {
    let dir = TestDir::new("sr-d4");
    let first = d4_once(&dir, "v1");
    let renames = first.iter().filter(|c| c.0 == "external_rename").count();
    assert_eq!(renames, 2, "{first:?}");
    assert!(first.iter().all(|c| c.0 != "external_delete"), "{first:?}");
    assert_eq!(d4_once(&dir, "v2"), first, "2回流して同じ結果");
}

/// D5：深い位置で名前も変えた（a.txt → n/b.txt）→ 名前の変更にしない（削除と作成）。
#[test]
fn d5_a_deep_move_with_a_new_name_is_a_delete_and_a_create() {
    let dir = TestDir::new("sr-d5");
    let (path, vault) = scan_vault(&dir, "v");
    fs::write(path.join("a.txt"), b"a").unwrap();
    rescan_all(&vault, &path);
    fs::create_dir(path.join("n")).unwrap();
    fs::rename(path.join("a.txt"), path.join("n").join("b.txt")).unwrap();
    assert_eq!(
        rescan_all(&vault, &path),
        [
            change("external_delete", "a.txt", None),
            change("external_create", "n", None),
            change("external_create", "n/b.txt", None),
        ]
    );
}

/// D10：a/x.txt と b/x.txt が同じ値。a/x.txt を消し、b を新しい n の下へ移す
/// → フォルダの組を先に決める：[create n, rename b → n/b, delete a/x.txt]。
#[test]
fn d10_folder_pairs_are_decided_before_file_pairs() {
    let dir = TestDir::new("sr-d10");
    let (path, vault) = scan_vault(&dir, "v");
    for d in ["a", "b"] {
        fs::create_dir(path.join(d)).unwrap();
        write_at(&path.join(d).join("x.txt"), b"same", fixed_time());
    }
    rescan_all(&vault, &path);
    fs::remove_file(path.join("a").join("x.txt")).unwrap();
    fs::create_dir(path.join("n")).unwrap();
    fs::rename(path.join("b"), path.join("n").join("b")).unwrap();
    assert_eq!(
        rescan_all(&vault, &path),
        [
            change("external_create", "n", None),
            change("external_rename", "b", Some("n/b")),
            change("external_delete", "a/x.txt", None),
        ]
    );
}

/// D7 の1回：r/d00〜d99/x00〜x99.txt（大きさ 0・同じ時刻）を s/n00〜n99/ へ移し r/d* を消す。ロックを取る直前に
/// コアが1件書く（ロックの中で比べ直す）。名前の変更が 10000 件・削除が 100 件出ることを確かめ、ロックを持った時間
/// （ロックを取ったあと、合わせる前から、走査を終えてロックを外すまで）を返す。
fn d7_lock_time(name: &str) -> Duration {
    let dir = TestDir::new(name);
    let (path, vault) = scan_vault(&dir, "v");
    fs::create_dir(path.join("r")).unwrap();
    for d in 0..100 {
        let folder = path.join("r").join(format!("d{d:02}"));
        fs::create_dir(&folder).unwrap();
        for f in 0..100 {
            write_at(&folder.join(format!("x{f:02}.txt")), b"", fixed_time());
        }
    }
    rescan_all(&vault, &path);
    fs::create_dir(path.join("s")).unwrap();
    for d in 0..100 {
        let to = path.join("s").join(format!("n{d:02}"));
        fs::create_dir(&to).unwrap();
        let from = path.join("r").join(format!("d{d:02}"));
        for f in 0..100 {
            let name = format!("x{f:02}.txt");
            fs::rename(from.join(&name), to.join(&name)).unwrap();
        }
        fs::remove_dir(&from).unwrap();
    }
    let writer = Vault::open(&path, false, &env()).unwrap();
    on_next(Step::BeforeScanLock, move || {
        call(
            &writer,
            "vault_write",
            json!({"path": "core.txt", "content": "c"}),
        )
        .unwrap();
    });
    let locked_at: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let released_at: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let (lock_slot, release_slot) = (Rc::clone(&locked_at), Rc::clone(&released_at));
    on_next(Step::BeforeReconcile, move || {
        lock_slot.set(Some(Instant::now()))
    });
    on_next(Step::AfterScan, move || {
        release_slot.set(Some(Instant::now()))
    });

    let found = rescan_all(&vault, &path);
    let held = released_at.get().expect("走査を終えた") - locked_at.get().expect("ロックを取った");
    let renames = found.iter().filter(|c| c.0 == "external_rename").count();
    let deletes = found.iter().filter(|c| c.0 == "external_delete").count();
    println!(
        "D7：名前の変更 {renames} 件・削除 {deletes} 件・全体 {} 件、ロックを持った時間 {held:?}",
        found.len()
    );
    assert_eq!(renames, 10_000);
    assert_eq!(deletes, 100, "r/d00〜d99 を消した");
    assert!(rescan_all(&vault, &path).is_empty());
    held
}

/// D7（性能）：1 万件の深い名前の変更を、ロックの中で比べ直しても終わる。ふだん流すときは、PC が混んでいても落ちない
/// ゆるい上限（20 秒）だけを確かめる（3 秒の上限は、下の #[ignore] の測定）。
#[test]
fn d7_ten_thousand_deep_renames_keep_the_lock_bounded() {
    let held = d7_lock_time("sr-d7");
    assert!(
        held < Duration::from_secs(20),
        "ロックを持った時間 {held:?}"
    );
}

/// D7 の測定（記録）：ロックを持った時間が 3 秒未満（debug）。PC が空いているときに流す。
/// `cargo test -p vault-core --test scan_renames -- --ignored --nocapture d7_measure`
#[test]
#[ignore = "1 万件の深い名前の変更で、ロックを持った時間を測る（--ignored で動かす）"]
fn d7_measure_the_lock_time() {
    let held = d7_lock_time("sr-d7-measure");
    assert!(held < Duration::from_secs(3), "ロックを持った時間 {held:?}");
}

// ---------------------------------------------------------------------------
// RI：本物のファイルシステムで、乱数の操作のあいだに走査する（同期と CRDT の乱数のテストと同じ考え方）
// ---------------------------------------------------------------------------

/// 種を決めて並べる、小さな乱数（xorshift。テストのためだけ）。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

/// 索引（`.vault/index.json`）のキー → 種類（"file"・"dir"・"link"）。
fn index_kinds(path: &Path) -> BTreeMap<String, String> {
    let index: Value =
        serde_json::from_slice(&fs::read(path.join(".vault").join("index.json")).unwrap()).unwrap();
    index
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v["kind"].as_str().unwrap().to_owned()))
        .collect()
}

/// `key` とその下のキー。
fn subtree(keys: &BTreeMap<String, String>, key: &str) -> Vec<String> {
    let prefix = format!("{key}/");
    keys.keys()
        .filter(|k| *k == key || k.starts_with(&prefix))
        .cloned()
        .collect()
}

fn parent_of(key: &str) -> Option<&str> {
    key.rsplit_once('/').map(|(p, _)| p)
}

/// Windows の名前の比べ方（大文字小文字を問わない）で、`key` と同じキーがあるか（`except` を除く）。
fn has_same_name(keys: &BTreeMap<String, String>, key: &str, except: Option<&str>) -> bool {
    let lower = key.to_lowercase();
    keys.keys()
        .any(|k| k.to_lowercase() == lower && Some(k.as_str()) != except)
}

/// 前の索引（`before`）に、履歴の external_* を前から順に当てる（索引の rename_tree を使わずに、当てられることも
/// 確かめる：rename は元があり・先がなく（大文字小文字を問わず、元を除く）・先の親がフォルダ、create は親が
/// フォルダで同じキーがない、delete はある）。`after` は種類を引くための今の索引。
fn apply(
    before: &BTreeMap<String, String>,
    changes: &[Change],
    after: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut keys = before.clone();
    let parent_is_dir = |keys: &BTreeMap<String, String>, key: &str| {
        parent_of(key).is_none_or(|p| keys.get(p).is_some_and(|k| k == "dir"))
    };
    for (op, path, to) in changes {
        match op.as_str() {
            "external_rename" => {
                let to = to.as_deref().ok_or("rename に to がない")?;
                if !keys.contains_key(path) {
                    return Err(format!("rename の元がない：{path}"));
                }
                if has_same_name(&keys, to, Some(path)) {
                    return Err(format!("rename の先がふさがっている：{path} → {to}"));
                }
                if !parent_is_dir(&keys, to) {
                    return Err(format!("rename の先の親がフォルダでない：{to}"));
                }
                for key in subtree(&keys, path) {
                    let kind = keys.remove(&key).unwrap();
                    keys.insert(format!("{to}{}", &key[path.len()..]), kind);
                }
            }
            "external_create" => {
                if has_same_name(&keys, path, None) {
                    return Err(format!("create の先がふさがっている：{path}"));
                }
                if !parent_is_dir(&keys, path) {
                    return Err(format!("create の親がフォルダでない：{path}"));
                }
                let kind = after
                    .get(path)
                    .ok_or(format!("create したものが今の索引にない：{path}"))?;
                keys.insert(path.clone(), kind.clone());
            }
            "external_delete" => {
                if !keys.contains_key(path) {
                    return Err(format!("delete するものがない：{path}"));
                }
                for key in subtree(&keys, path) {
                    keys.remove(&key);
                }
            }
            "external_modify" => {
                if !keys.contains_key(path) {
                    return Err(format!("modify するものがない：{path}"));
                }
            }
            "index_rebuilt" => return Err("索引を作り直した".to_owned()),
            _ => {}
        }
    }
    Ok(keys)
}

/// 保管庫の中のファイル・フォルダ（相対パス。.vault は除く）。
fn tree(root: &Path) -> (Vec<String>, Vec<String>) {
    fn walk(root: &Path, rel: &str, files: &mut Vec<String>, dirs: &mut Vec<String>) {
        let here = if rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(rel)
        };
        for entry in fs::read_dir(here).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if rel.is_empty() && name == ".vault" {
                continue;
            }
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                dirs.push(child.clone());
                walk(root, &child, files, dirs);
            } else {
                files.push(child);
            }
        }
    }
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    walk(root, "", &mut files, &mut dirs);
    (files, dirs)
}

const NAMES: [&str; 6] = ["a.txt", "b.txt", "A.txt", "c", "d", "C"];

/// 乱数の外の操作を1つ（新しいフォルダを作って移す・フォルダから出して元を消す・同じフォルダでの名前の変更・
/// 大文字小文字だけの変更・すでにあるフォルダへの移動・木ごと消す・作る・値を変える）。
fn random_fs_op(rng: &mut Rng, root: &Path, counter: &mut u32) {
    let (files, dirs) = tree(root);
    let mut places: Vec<String> = vec![String::new()];
    places.extend(dirs.iter().filter(|d| d.matches('/').count() < 3).cloned());
    let join = |dir: &str, name: &str| {
        if dir.is_empty() {
            name.to_owned()
        } else {
            format!("{dir}/{name}")
        }
    };
    let free = |rel: &str| {
        let p = root.join(rel);
        !p.exists() && p.parent().is_some_and(Path::is_dir)
    };
    *counter += 1;
    match rng.below(9) {
        // 作る（ファイル）
        0 => {
            let at = join(&places[rng.below(places.len())], rng.pick(&NAMES));
            if free(&at) {
                fs::write(root.join(&at), format!("new {counter}")).unwrap();
            }
        }
        // 作る（フォルダ）
        1 => {
            let at = join(&places[rng.below(places.len())], rng.pick(&NAMES));
            if free(&at) {
                fs::create_dir(root.join(&at)).unwrap();
            }
        }
        // 値を変える
        2 if !files.is_empty() => {
            let f = &files[rng.below(files.len())];
            fs::write(
                root.join(f),
                format!("changed {counter} {}", "x".repeat(rng.below(5))),
            )
            .unwrap();
        }
        // 同じフォルダでの名前の変更（大文字小文字だけの変更も）
        3 if !files.is_empty() || !dirs.is_empty() => {
            let all: Vec<&String> = files.iter().chain(&dirs).collect();
            let from = all[rng.below(all.len())].clone();
            let to = join(parent_of(&from).unwrap_or(""), rng.pick(&NAMES));
            if to != from && (free(&to) || to.to_lowercase() == from.to_lowercase()) {
                let _ = fs::rename(root.join(&from), root.join(&to));
            }
        }
        // すでにあるフォルダへ移す
        4 if !files.is_empty() => {
            let from = files[rng.below(files.len())].clone();
            let name = from.rsplit('/').next().unwrap().to_owned();
            let to = join(&places[rng.below(places.len())], &name);
            if free(&to) {
                fs::rename(root.join(&from), root.join(&to)).unwrap();
            }
        }
        // 新しいフォルダを作って移す（ファイルかフォルダ）
        5 if !files.is_empty() || !dirs.is_empty() => {
            let all: Vec<&String> = files.iter().chain(&dirs).collect();
            let from = all[rng.below(all.len())].clone();
            let name = from.rsplit('/').next().unwrap().to_owned();
            let new_dir = join(&places[rng.below(places.len())], &format!("n{counter}"));
            if free(&new_dir) && !new_dir.starts_with(&format!("{from}/")) {
                fs::create_dir(root.join(&new_dir)).unwrap();
                let to = format!("{new_dir}/{name}");
                let _ = fs::rename(root.join(&from), root.join(&to));
            }
        }
        // フォルダから出して元を消す
        6 if !dirs.is_empty() => {
            let d = dirs[rng.below(dirs.len())].clone();
            let up = parent_of(&d).unwrap_or("").to_owned();
            let (inner, _) = tree(&root.join(&d));
            for f in inner.iter().filter(|f| !f.contains('/')) {
                let to = join(&up, f);
                if free(&to) {
                    fs::rename(root.join(&d).join(f), root.join(&to)).unwrap();
                }
            }
            let _ = fs::remove_dir(root.join(&d));
        }
        // 木ごと消す
        7 if !dirs.is_empty() || !files.is_empty() => {
            if rng.below(2) == 0 && !dirs.is_empty() {
                let d = &dirs[rng.below(dirs.len())];
                let _ = fs::remove_dir_all(root.join(d));
            } else if !files.is_empty() {
                let f = &files[rng.below(files.len())];
                let _ = fs::remove_file(root.join(f));
            }
        }
        // ファイルをフォルダに替える（種類を変える）
        _ if !files.is_empty() => {
            let f = files[rng.below(files.len())].clone();
            if fs::remove_file(root.join(&f)).is_ok() {
                fs::create_dir(root.join(&f)).unwrap();
            }
        }
        _ => {}
    }
}

/// RI の1つの種：乱数の木を作って索引に入れ、30 の外の操作のあとに走査する、を 3 回。前の索引のキーに履歴の
/// external_* を当てると今の索引のキーになる。操作をやめたあとの2回目の走査は何も出さない。
fn ri_run(seed: u64) -> Result<(), String> {
    let dir = TestDir::new(&format!("sr-ri-{seed}"));
    let (path, vault) = scan_vault(&dir, "v");
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut counter = 0;
    for _ in 0..20 {
        random_fs_op(&mut rng, &path, &mut counter);
    }
    rescan_all(&vault, &path);
    for round in 0..3 {
        let before = index_kinds(&path);
        for _ in 0..30 {
            random_fs_op(&mut rng, &path, &mut counter);
        }
        let changes = rescan_all(&vault, &path);
        let after = index_kinds(&path);
        let applied = apply(&before, &changes, &after).map_err(|e| format!("回 {round}：{e}"))?;
        if applied != after {
            return Err(format!(
                "回 {round}：当てた結果が今の索引と違う\n当てた {applied:?}\n今 {after:?}\n変更 {changes:?}"
            ));
        }
        let creates: Vec<&String> = changes
            .iter()
            .filter(|c| c.0 == "external_create")
            .map(|c| &c.1)
            .collect();
        let unique: BTreeSet<&&String> = creates.iter().collect();
        if unique.len() != creates.len() {
            return Err(format!("回 {round}：同じパスの作成が2回ある {creates:?}"));
        }
    }
    rescan_all(&vault, &path);
    let quiet = rescan_all(&vault, &path);
    if !quiet.is_empty() {
        return Err(format!(
            "操作をやめたあとの2回目の走査で変更が出た：{quiet:?}"
        ));
    }
    Ok(())
}

/// RI：種 3 個（`VAULT_RANDOM_SEED_FROM`・`VAULT_RANDOM_SEED_N` で広げられる）。
#[test]
fn ri_random_external_operations_replay_onto_the_index() {
    let var = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let seeds: Vec<u64> = match (var("VAULT_RANDOM_SEED_FROM"), var("VAULT_RANDOM_SEED_N")) {
        (Some(from), Some(n)) => (from..from + n).collect(),
        _ => vec![1, 2, 3],
    };
    let failures: Vec<String> = seeds
        .into_iter()
        .filter_map(|seed| ri_run(seed).err().map(|e| format!("種 {seed}：{e}")))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
