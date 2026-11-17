//! 走査と vault_changes の結合テスト。

mod common;

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use common::{TestDir, call, env, expect_code, journal, new_vault};
use serde_json::{Value, json};
use vault_core::{ErrorCode, MIN_RESCAN_INTERVAL, Op, Vault};

/// 1つの変更（op・path・to）。
type Change = (String, String, Option<String>);

fn change(op: &str, path: &str, to: Option<&str>) -> Change {
    (op.to_owned(), path.to_owned(), to.map(str::to_owned))
}

/// vault_changes を呼び、(変更, 返り値の全体) を返す。外での変更は actor が external/fs であることも確かめる。
fn changes_with(vault: &Vault, args: Value) -> (Vec<Change>, Value) {
    let result =
        call(vault, "vault_changes", args).unwrap_or_else(|e| panic!("vault_changes: {e}"));
    let list = result["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let op = c["op"].as_str().unwrap().to_owned();
            if op.starts_with("external_") || op == "index_rebuilt" {
                assert_eq!(c["actor"]["kind"], "external", "{c}");
                assert_eq!(c["actor"]["id"], "fs", "{c}");
            }
            (
                op,
                c["path"].as_str().unwrap_or_default().to_owned(),
                c["to"].as_str().map(str::to_owned),
            )
        })
        .collect();
    (list, result)
}

/// 走査してから、`since` より後の変更を読む。返り値は (変更, next_seq)。
fn rescan(vault: &Vault, since: u64) -> (Vec<Change>, u64) {
    let (list, result) = changes_with(vault, json!({"since_seq": since}));
    (list, result["next_seq"].as_u64().unwrap())
}

/// 走査のテスト用の保管庫（走査の最短の間隔を 0 にして、続けて走査できるようにする）。
fn scan_vault(dir: &TestDir) -> (PathBuf, Vault) {
    let (path, mut vault) = new_vault(dir, "v");
    vault.set_min_rescan_interval(Duration::ZERO);
    vault.set_rescan_interval_factor(0); // 前の走査の時間で伸ばさない（続けて走査する）
    (path, vault)
}

fn index(vault_path: &Path) -> Value {
    serde_json::from_slice(&fs::read(vault_path.join(".vault").join("index.json")).unwrap())
        .unwrap()
}

fn index_keys(vault_path: &Path) -> Vec<String> {
    index(vault_path)
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

/// 外での作成・更新（大きさを必ず変える）・削除・名前の変更（ファイルとフォルダ）。
#[test]
fn external_creates_modifies_deletes_and_renames_are_reported() {
    let dir = TestDir::new("scan-external");
    let (path, vault) = scan_vault(&dir);
    let (first, seq) = rescan(&vault, 0);
    assert_eq!(first, [change("init", "", None)], "init のほかには何もない");

    fs::write(path.join("a.txt"), b"a").unwrap();
    fs::write(path.join("r.txt"), b"rr").unwrap();
    fs::create_dir(path.join("d")).unwrap();
    fs::write(path.join("d").join("x.txt"), b"xxx").unwrap();
    fs::create_dir(path.join("m")).unwrap();
    fs::write(path.join("m").join("n.txt"), b"nnnn").unwrap();
    let (created, seq) = rescan(&vault, seq);
    assert_eq!(
        created,
        [
            change("external_create", "a.txt", None),
            change("external_create", "d", None),
            change("external_create", "d/x.txt", None),
            change("external_create", "m", None),
            change("external_create", "m/n.txt", None),
            change("external_create", "r.txt", None),
        ]
    );
    let entries = journal(&path);
    let a = entries
        .iter()
        .find(|e| e.op == Op::ExternalCreate && e.path.as_deref() == Some("a.txt"))
        .unwrap();
    assert_eq!(a.size, Some(1));
    let d = entries
        .iter()
        .find(|e| e.op == Op::ExternalCreate && e.path.as_deref() == Some("d"))
        .unwrap();
    assert_eq!(d.size, None, "フォルダの大きさは書かない");

    // 更新（大きさを変える）・削除（フォルダごと）・名前の変更（ファイルとフォルダ）
    fs::write(path.join("a.txt"), b"aaaaaa").unwrap();
    fs::remove_dir_all(path.join("d")).unwrap();
    fs::rename(path.join("r.txt"), path.join("s.txt")).unwrap();
    fs::rename(path.join("m"), path.join("m2")).unwrap();
    let (changed, seq) = rescan(&vault, seq);
    assert_eq!(
        changed,
        [
            change("external_rename", "m", Some("m2")), // 中の n.txt は書かない
            change("external_rename", "r.txt", Some("s.txt")),
            change("external_delete", "d", None), // 中の x.txt は書かない
            change("external_modify", "a.txt", None),
        ]
    );
    let modify = journal(&path)
        .into_iter()
        .find(|e| e.op == Op::ExternalModify)
        .unwrap();
    assert_eq!(modify.size, Some(6));

    // もう一度調べても、何も出ない。索引は今の状態
    let (again, next) = rescan(&vault, seq);
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(next, seq);
    assert_eq!(index_keys(&path), ["a.txt", "m2", "m2/n.txt", "s.txt"]);
    assert_eq!(index(&path)["a.txt"]["size"], 6);
}

/// コアの操作は先に索引に入っているので、走査で二重に出ない。
#[test]
fn core_operations_are_not_reported_twice() {
    let dir = TestDir::new("scan-core");
    let (_path, vault) = scan_vault(&dir);
    let (_, start) = rescan(&vault, 0);

    let ok = |tool: &str, args: Value| {
        call(&vault, tool, args).unwrap_or_else(|e| panic!("{tool}: {e}"))
    };
    ok("vault_write", json!({"path": "a.txt", "content": "1"}));
    ok("vault_write", json!({"path": "a.txt", "content": "22"}));
    ok("vault_mkdir", json!({"path": "d/e"}));
    ok(
        "vault_write",
        json!({"path": "d/e/f.txt", "content": "fff"}),
    );
    ok("vault_move", json!({"from": "a.txt", "to": "d/a.txt"}));
    ok("vault_move", json!({"from": "d", "to": "d2"}));
    let deleted = ok("vault_delete", json!({"path": "d2/e", "recursive": true}));
    ok(
        "vault_restore",
        json!({"trash_id": deleted["trash_id"].clone()}),
    );
    let deleted = ok("vault_delete", json!({"path": "d2/a.txt"}));
    ok(
        "vault_restore",
        json!({"trash_id": deleted["trash_id"].clone(), "to": "new/place/z.txt"}),
    );
    ok("vault_write", json!({"path": "d2/e/f.txt", "content": "g"}));
    let deleted = ok("vault_delete", json!({"path": "d2/e/f.txt"}));
    ok(
        "vault_restore",
        json!({"trash_id": deleted["trash_id"].clone(), "replace": true}),
    );

    let (after, seq) = rescan(&vault, start);
    assert!(!after.is_empty());
    assert!(
        after.iter().all(|(op, _, _)| !op.starts_with("external_")),
        "コアの操作が、外での変更としても出た：{after:?}"
    );
    let (again, _) = rescan(&vault, seq);
    assert!(again.is_empty(), "{again:?}");
}

/// 途中のフォルダの大文字小文字が実際と違っても、記録（返り値・履歴・索引のキー・entry）は
/// 実際の名前にそろう。走査で、ありもしない削除と作成が出ない。
#[cfg(windows)]
#[test]
fn recorded_paths_use_the_actual_case() {
    let dir = TestDir::new("scan-case");
    let (path, vault) = scan_vault(&dir);
    let ok = |tool: &str, args: Value| {
        call(&vault, tool, args).unwrap_or_else(|e| panic!("{tool}: {e}"))
    };
    ok("vault_mkdir", json!({"path": "p"}));
    let (_, start) = rescan(&vault, 0);

    let w = ok("vault_write", json!({"path": "P/b.txt", "content": "b"}));
    assert_eq!(w["path"], "p/b.txt");
    let m = ok("vault_mkdir", json!({"path": "P/n"}));
    assert_eq!(m["path"], "p/n");
    assert_eq!(m["created"], true);
    let m = ok("vault_mkdir", json!({"path": "P/N"}));
    assert_eq!(m["path"], "p/n", "すでにあるフォルダも、実際の名前で返す");
    assert_eq!(m["created"], false);
    let mv = ok("vault_move", json!({"from": "P/B.TXT", "to": "P/c.txt"}));
    assert_eq!(mv["from"], "p/b.txt");
    assert_eq!(mv["to"], "p/c.txt");
    // 上書き：古い中身の entry は実際の名前。新しいファイルは、渡した名前で作られる（それが実際の名前）
    let w = ok("vault_write", json!({"path": "P/C.TXT", "content": "cc"}));
    assert_eq!(w["path"], "p/C.TXT");
    let previous = w["previous_trash_id"].as_str().unwrap();
    assert_eq!(
        common::trash_entry(&path, previous).original_path,
        "p/c.txt"
    );
    let deleted = ok("vault_delete", json!({"path": "P/c.txt"}));
    assert_eq!(deleted["path"], "p/C.TXT");
    let restored = ok(
        "vault_restore",
        json!({"trash_id": deleted["trash_id"].clone(), "to": "P/n/r.txt"}),
    );
    assert_eq!(restored["path"], "p/n/r.txt");
    // 親を作りながら書く：作った親も、その下も、記録は実際の名前
    let w = ok(
        "vault_write",
        json!({"path": "P/new/q.txt", "content": "q"}),
    );
    assert_eq!(w["path"], "p/new/q.txt");

    let ours: Vec<String> = journal(&path)
        .into_iter()
        .filter(|e| e.seq > start)
        .flat_map(|e| [e.path, e.to])
        .flatten()
        .collect();
    assert!(
        ours.iter().all(|p| !p.starts_with("P/")),
        "履歴に、実際と違う大文字小文字が残った：{ours:?}"
    );
    assert!(index_keys(&path).iter().all(|k| !k.starts_with("P/")));

    let (after, _) = rescan(&vault, start);
    assert!(
        after.iter().all(|(op, _, _)| !op.starts_with("external_")),
        "ありもしない外での変更が出た：{after:?}"
    );
}

/// どの深さでも `.vault`（大文字小文字を問わない）には入らない。
#[test]
fn vault_folders_at_any_depth_are_not_scanned() {
    let dir = TestDir::new("scan-dotvault");
    let (path, vault) = scan_vault(&dir);
    let (_, seq) = rescan(&vault, 0);
    fs::create_dir_all(path.join("sub").join(".vault")).unwrap();
    fs::write(path.join("sub").join(".vault").join("x.txt"), b"x").unwrap();
    fs::create_dir_all(path.join("sub").join("deeper").join(".VAULT")).unwrap();
    fs::write(
        path.join("sub").join("deeper").join(".VAULT").join("y.txt"),
        b"y",
    )
    .unwrap();
    // 根の .vault の中（履歴・索引・ゴミ箱）も出ない
    fs::write(path.join(".vault").join("tmp").join("stray.tmp"), b"z").unwrap();

    let (found, _) = rescan(&vault, seq);
    assert_eq!(
        found,
        [
            change("external_create", "sub", None),
            change("external_create", "sub/deeper", None),
        ]
    );
    assert!(
        index_keys(&path)
            .iter()
            .all(|k| !k.to_lowercase().contains(".vault"))
    );
}

/// seq が since_seq より大きいものだけ。limit・truncated・next_seq。履歴のファイルが分かれていても読める。
#[test]
fn since_seq_limit_truncated_and_next_seq() {
    let dir = TestDir::new("scan-seq");
    let (path, vault) = scan_vault(&dir);
    for name in ["a", "b", "c", "d", "e"] {
        fs::write(path.join(name), name).unwrap();
    }
    // seq 1 は init、2〜6 は外での作成
    let (page, all) = changes_with(&vault, json!({"limit": 2}));
    assert_eq!(
        page,
        [
            change("init", "", None),
            change("external_create", "a", None)
        ]
    );
    assert_eq!(all["truncated"], true);
    assert_eq!(all["next_seq"], 2);
    let (page, all) = changes_with(&vault, json!({"since_seq": 2, "limit": 2, "rescan": false}));
    assert_eq!(
        page,
        [
            change("external_create", "b", None),
            change("external_create", "c", None)
        ]
    );
    assert_eq!(all["truncated"], true);
    assert_eq!(all["next_seq"], 4);
    let (page, all) = changes_with(&vault, json!({"since_seq": 4, "rescan": false}));
    assert_eq!(page.len(), 2);
    assert_eq!(all["truncated"], false);
    assert_eq!(all["next_seq"], 6);
    // ちょうど limit 件で終わるときは truncated:false
    let (page, all) = changes_with(&vault, json!({"since_seq": 4, "limit": 2, "rescan": false}));
    assert_eq!(page.len(), 2);
    assert_eq!(all["truncated"], false);
    // 先の seq：何も返さず、next_seq は since_seq のまま
    let (page, all) = changes_with(&vault, json!({"since_seq": 1000, "rescan": false}));
    assert!(page.is_empty());
    assert_eq!(all["next_seq"], 1000);
    assert_eq!(all["truncated"], false);
    for seq in [0, 3, 5] {
        let (_, all) = changes_with(&vault, json!({"since_seq": seq, "rescan": false}));
        for c in all["changes"].as_array().unwrap() {
            assert!(c["seq"].as_u64().unwrap() > seq);
        }
    }
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 10_001}),
        json!({"since_seq": -1}),
    ] {
        let what = bad.to_string();
        expect_code(
            call(&vault, "vault_changes", bad),
            ErrorCode::InvalidArgument,
            &what,
        );
    }

    // 履歴のファイルが分かれているとき（次のファイルの名前は、その最初の seq）。読めない行は飛ばす
    let line = |seq: u64, name: &str| {
        format!(
            "{{\"seq\":{seq},\"ts\":\"2026-11-16T00:00:00.000Z\",\"actor\":{{\"kind\":\"external\",\"id\":\"fs\",\"session\":\"t\"}},\"op\":\"external_create\",\"path\":\"{name}\"}}\n"
        )
    };
    let next_file = path
        .join(".vault")
        .join("journal")
        .join(format!("{:020}.jsonl", 10));
    fs::write(&next_file, line(10, "x") + "{broken\n" + &line(11, "y")).unwrap();
    let seqs = |since: u64| -> Vec<u64> {
        let (_, all) = changes_with(&vault, json!({"since_seq": since, "rescan": false}));
        all["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["seq"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(seqs(0), [1, 2, 3, 4, 5, 6, 10, 11]);
    assert_eq!(seqs(5), [6, 10, 11]);
    assert_eq!(seqs(9), [10, 11]);
    assert_eq!(seqs(10), [11]);
    // 次に書く行は、その続きの seq
    fs::write(path.join("f"), b"f").unwrap();
    let (found, next) = rescan(&vault, 11);
    assert_eq!(found, [change("external_create", "f", None)]);
    assert_eq!(next, 12);
}

/// 索引が壊れている・ないとき：作り直して index_rebuilt だけを記録する（差分は出さない）。
#[test]
fn a_broken_or_missing_index_is_rebuilt() {
    let dir = TestDir::new("scan-rebuild");
    let (path, vault) = scan_vault(&dir);
    let (_, seq) = rescan(&vault, 0);
    fs::write(path.join("a.txt"), b"a").unwrap();
    let index_file = path.join(".vault").join("index.json");
    fs::write(&index_file, b"{not json").unwrap();

    let (found, seq) = rescan(&vault, seq);
    assert_eq!(found, [change("index_rebuilt", "", None)]);
    assert_eq!(index_keys(&path), ["a.txt"]);

    // 作り直したあとは、ふつうに比べる
    fs::write(path.join("c.txt"), b"c").unwrap();
    let (found, seq) = rescan(&vault, seq);
    assert_eq!(found, [change("external_create", "c.txt", None)]);

    // 索引がない
    fs::remove_file(&index_file).unwrap();
    fs::write(path.join("b.txt"), b"b").unwrap();
    let (found, _) = rescan(&vault, seq);
    assert_eq!(found, [change("index_rebuilt", "", None)]);
    assert_eq!(index_keys(&path), ["a.txt", "b.txt", "c.txt"]);
}

/// 上限（64段・260字）を超える木は対象にしない（数だけを返す）。
#[test]
fn trees_beyond_the_limits_are_not_scanned() {
    let dir = TestDir::new("scan-over");
    let (path, vault) = scan_vault(&dir);
    let (_, seq) = rescan(&vault, 0);
    let root = fs::canonicalize(&path).unwrap();
    let mut deep = root.join("deep");
    for _ in 0..70 {
        deep.push("a");
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("bottom.txt"), b"bottom").unwrap();
    let long_name = "n".repeat(200);
    fs::create_dir_all(root.join("long")).unwrap();
    fs::write(root.join("long").join(&long_name), b"long").unwrap();

    let (found, result) = changes_with(&vault, json!({"since_seq": seq}));
    assert_eq!(
        result["skipped_over_limit"], 2,
        "深すぎる1つと、長すぎる1つ（その中は数えない）"
    );
    assert!(found.iter().any(|(_, p, _)| p == "deep"));
    assert!(found.iter().any(|(_, p, _)| p == "long"));
    for (_, p, _) in &found {
        assert!(p.split('/').count() <= 64, "{p}");
        assert!(!p.contains(&long_name));
    }
    assert!(
        index_keys(&path)
            .iter()
            .all(|k| k.split('/').count() <= 64 && !k.contains(&long_name))
    );
    // 次の走査でも、出ない（消えたとも、作られたとも言わない）
    let (_, result) = changes_with(&vault, json!({"since_seq": result["next_seq"].clone()}));
    assert!(result["changes"].as_array().unwrap().is_empty());
    assert_eq!(result["skipped_over_limit"], 2);
    // rescan:false では調べないので 0
    let (_, result) = changes_with(&vault, json!({"rescan": false}));
    assert_eq!(result["skipped_over_limit"], 0);
}

/// 外を指すジャンクションは、リンクそのものを項目として扱い、先は走査しない。
#[cfg(windows)]
#[test]
fn junctions_to_the_outside_are_not_followed() {
    let dir = TestDir::new("scan-junction");
    let (path, vault) = scan_vault(&dir);
    let (_, seq) = rescan(&vault, 0);
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    fs::create_dir(path.join("f")).unwrap();
    if !common::junction(&path.join("f").join("j"), &outside) {
        return;
    }
    let (found, _) = rescan(&vault, seq);
    assert_eq!(
        found,
        [
            change("external_create", "f", None),
            change("external_create", "f/j", None),
        ]
    );
    let idx = index(&path);
    assert_eq!(idx["f/j"]["kind"], "link");
    assert!(index_keys(&path).iter().all(|k| !k.contains("secret")));
    assert_eq!(
        fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "secret"
    );
}

/// 大文字小文字だけの名前の変更は、名前の変更として出る（ファイルもフォルダも）。
#[test]
fn case_only_renames_are_renames() {
    let dir = TestDir::new("scan-case-rename");
    let (path, vault) = scan_vault(&dir);
    fs::write(path.join("a.txt"), b"a").unwrap();
    fs::create_dir(path.join("dir")).unwrap();
    fs::write(path.join("dir").join("x.txt"), b"xx").unwrap();
    let (_, seq) = rescan(&vault, 0);

    fs::rename(path.join("a.txt"), path.join("A.txt")).unwrap();
    fs::rename(path.join("dir"), path.join("DIR")).unwrap();
    let (found, _) = rescan(&vault, seq);
    assert_eq!(
        found,
        [
            change("external_rename", "a.txt", Some("A.txt")),
            change("external_rename", "dir", Some("DIR")),
        ]
    );
    assert_eq!(index_keys(&path), ["A.txt", "DIR", "DIR/x.txt"]);
}

/// 読み取り専用で開いた保管庫でも、rescan は .vault の索引と履歴に書く。rescan:false なら調べない。
#[test]
fn rescan_in_a_read_only_vault_writes_the_journal_and_index() {
    let dir = TestDir::new("scan-readonly");
    let (path, _vault) = new_vault(&dir, "v");
    let mut ro = Vault::open(&path, true, &env()).unwrap();
    ro.set_min_rescan_interval(Duration::ZERO);
    ro.set_rescan_interval_factor(0); // 前の走査の時間で伸ばさない（続けて走査する）
    let before = journal(&path).len();

    fs::write(path.join("a.txt"), b"a").unwrap();
    let (found, seq) = rescan(&ro, 0);
    assert!(found.contains(&change("external_create", "a.txt", None)));
    assert_eq!(journal(&path).len(), before + 1);
    assert_eq!(index_keys(&path), ["a.txt"]);

    fs::write(path.join("b.txt"), b"b").unwrap();
    let (found, next) = changes_with(&ro, json!({"since_seq": seq, "rescan": false}));
    assert!(found.is_empty(), "rescan:false では調べない");
    assert_eq!(next["next_seq"], seq);
    assert_eq!(journal(&path).len(), before + 1);
    // 書くツールは、読み取り専用のまま
    expect_code(
        call(&ro, "vault_write", json!({"path": "c.txt", "content": "c"})),
        ErrorCode::ReadOnly,
        "write",
    );
}

/// 読めないフォルダ（権限）は、中を削除と取り違えない。vault_changes も失敗しない。
#[cfg(windows)]
#[test]
fn unreadable_folders_are_not_reported_as_deleted() {
    let dir = TestDir::new("scan-unreadable");
    let (path, vault) = scan_vault(&dir);
    fs::create_dir(path.join("locked")).unwrap();
    fs::write(path.join("locked").join("x.txt"), b"x").unwrap();
    let (_, seq) = rescan(&vault, 0);
    {
        // 中の一覧を読めなくする（RD ＝ フォルダの一覧）
        let _deny = common::DenyAce::new(&path.join("locked"), "(RD)");
        if fs::read_dir(path.join("locked")).is_ok() {
            common::skip("unreadable", "拒否を付けても一覧を読めた（管理者など）");
            return;
        }
        let (found, _) = rescan(&vault, seq);
        assert!(found.is_empty(), "{found:?}");
        assert!(index_keys(&path).contains(&"locked/x.txt".to_owned()));
    }
    let (found, _) = rescan(&vault, seq);
    assert!(found.is_empty(), "{found:?}");
}

/// 前の走査（捨てたもの・エラーで終わったものも）が終わってから、最短の間隔（2 秒。前の走査の時間の 10 倍のほうが
/// 長ければ、そちら）が過ぎていない rescan は、走査せずに履歴だけを返す。
#[test]
fn rescans_within_two_seconds_only_read_the_journal() {
    assert_eq!(MIN_RESCAN_INTERVAL, Duration::from_secs(2));
    let dir = TestDir::new("scan-interval");
    let (path, vault) = new_vault(&dir, "v"); // 既定の間隔のまま
    let (_, first) = changes_with(&vault, json!({}));
    assert_eq!(first["scanned"], true);
    let seq = first["next_seq"].as_u64().unwrap();

    fs::write(path.join("a.txt"), b"a").unwrap();
    let (found, soon) = changes_with(&vault, json!({"since_seq": seq}));
    assert_eq!(soon["scanned"], false, "2 秒以内は走査しない");
    assert!(found.is_empty());
    assert_eq!(soon["skipped_over_limit"], 0);

    thread::sleep(MIN_RESCAN_INTERVAL + Duration::from_millis(100));
    let (found, later) = changes_with(&vault, json!({"since_seq": seq}));
    assert_eq!(later["scanned"], true);
    assert_eq!(found, [change("external_create", "a.txt", None)]);
}

/// 歩いたあと・書く前に、コアの操作が入っても、その回の結果を捨てずに、コアが書いたパスだけを合わせる
/// （以前は「捨てる」だった）。外での変更はその回で記録し、コアの書き込みは二重に出ない。
#[test]
fn a_core_operation_during_the_walk_is_reconciled() {
    let dir = TestDir::new("scan-discard");
    let (path, vault) = new_vault(&dir, "v"); // 既定の間隔のまま
    fs::write(path.join("ext.txt"), b"e").unwrap();
    let other_path = path.clone();
    vault_core::test_support::after_next_walk(move || {
        let other = Vault::open(&other_path, false, &env()).unwrap();
        call(
            &other,
            "vault_write",
            json!({"path": "core.txt", "content": "c"}),
        )
        .unwrap();
    });
    let (found, first) = changes_with(&vault, json!({}));
    assert_eq!(first["scanned"], true, "歩くあいだに書かれても捨てない");
    assert_eq!(
        found,
        [
            change("init", "", None),
            change("write", "core.txt", None),
            change("external_create", "ext.txt", None)
        ]
    );
    assert_eq!(index_keys(&path), ["core.txt", "ext.txt"]);
    // もう1回走査しても、外での変更は出ない（合わせ方を誤ると、次の回に削除・作成として出る）。
    // この Vault は既定の間隔のままなので、走査は、開き直した Vault で行う
    let next = first["next_seq"].as_u64().unwrap();
    let fresh = Vault::open(&path, false, &env()).unwrap();
    let (found, again) = changes_with(&fresh, json!({"since_seq": next}));
    assert_eq!(again["scanned"], true);
    assert!(found.is_empty(), "{found:?}");
}

/// 歩くあいだはロックを持たない：とても多くのフォルダ（歩くのに1秒以上かかる数）の保管庫で、走査のあいだに
/// 別のスレッドから vault_write しても LOCKED にならない。走査がロックを持つのは、書く前の合わせから書き終えるまで
/// だけで、その時間（ロックを取った直後＝BeforeReconcile から、呼び出しが返るまで）を直接測って、3 秒の上限で
/// 確かめる（debug の速さに頼らない。ロックの中の上限は 1 秒）。書き込みが入った回も捨てずに終え、書いたファイルを
/// 外での変更と二重に出さない。
#[test]
fn walking_many_folders_does_not_block_writers() {
    let dir = TestDir::new("scan-many");
    let (path, vault) = scan_vault(&dir);
    for a in 0..150 {
        let parent = path.join(format!("d{a:03}"));
        fs::create_dir(&parent).unwrap();
        for b in 0..100 {
            fs::create_dir(parent.join(format!("e{b:03}"))).unwrap();
        }
    }
    let mut writer = Vault::open(&path, false, &env()).unwrap();
    // 歩くあいだロックを持てば、この待ちを超えて LOCKED になる（ロックを持つのは、書く前の合わせから書き終えるまで）
    writer.set_lock_timeout(Duration::from_secs(5));
    // 走査が最後にロックを取った時刻（ロックを外して取り直したときは、最後の分。差し込み口は多めに積む）
    let locked_at: std::rc::Rc<std::cell::Cell<Option<Instant>>> = std::rc::Rc::default();
    for _ in 0..4 {
        let at = locked_at.clone();
        vault_core::test_support::on_next(
            vault_core::test_support::Step::BeforeReconcile,
            move || {
                at.set(Some(Instant::now()));
            },
        );
    }
    let done = AtomicBool::new(false);
    let (scan_time, held, result, waits) = thread::scope(|s| {
        let writes = s.spawn(|| {
            let mut waits = Vec::new();
            let mut i = 0;
            while !done.load(Ordering::Relaxed) {
                let start = Instant::now();
                call(
                    &writer,
                    "vault_write",
                    json!({"path": format!("w-{i}.txt"), "content": "w"}),
                )
                .unwrap_or_else(|e| panic!("走査のあいだの vault_write：{e}"));
                waits.push(start.elapsed());
                i += 1;
                thread::sleep(Duration::from_millis(20));
            }
            waits
        });
        thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        let (_, result) = changes_with(&vault, json!({"limit": 10}));
        let scan_time = start.elapsed();
        let held = locked_at.get().expect("ロックを取っていない").elapsed();
        done.store(true, Ordering::Relaxed);
        (scan_time, held, result, writes.join().unwrap())
    });
    eprintln!(
        "15150 フォルダ：走査 {scan_time:?}、ロックを持った時間 {held:?}、そのあいだの書き込み {} 回（最長 {:?}）",
        waits.len(),
        waits.iter().max().unwrap()
    );
    if scan_time < Duration::from_secs(1) {
        common::skip(
            "walking_many_folders_does_not_block_writers",
            &format!(
                "この PC では歩くのが速い（{scan_time:?}）。書き込みが待たないことだけ確かめた"
            ),
        );
    }
    // ロックを持つのは、書く前の合わせから書き終えるまでだけ（走査の時間のほとんどは持たない）
    assert!(
        held < Duration::from_secs(3),
        "ロックを長く持った：{held:?}（走査 {scan_time:?}）"
    );
    assert!(waits.len() >= 2);
    // 走査のあいだに書き込みがあっても捨てずに終える（以前は「捨てる」だった）：
    // フォルダは外での作成、書いたファイルは出ない
    assert_eq!(
        result["scanned"], true,
        "走査のあいだに書き込みがあっても終える"
    );
    let entries = journal(&path);
    let creates = entries
        .iter()
        .filter(|e| e.op == Op::ExternalCreate)
        .count();
    assert_eq!(creates, 150 + 150 * 100);
    assert!(
        entries
            .iter()
            .filter(|e| e.path.as_deref().is_some_and(|p| p.starts_with("w-")))
            .all(|e| e.op == Op::Write),
        "書いたファイルが、外での変更としても出た"
    );
    // もう1回走査しても、外での変更は出ない
    let before = journal(&path).len();
    let (_, again) = changes_with(&vault, json!({"limit": 1}));
    assert_eq!(again["scanned"], true);
    assert_eq!(journal(&path).len(), before, "次の走査で外での変更が出た");
}

/// 読めないフォルダの祖先（またはそのフォルダ自身）の名前を外で変えても、ありもしない削除・作成を出さない。
#[cfg(windows)]
#[test]
fn renaming_the_parent_of_an_unreadable_folder() {
    let dir = TestDir::new("scan-unreadable-rename");
    let (path, vault) = scan_vault(&dir);
    fs::create_dir_all(path.join("a").join("locked")).unwrap();
    fs::write(path.join("a").join("locked").join("x.txt"), b"x").unwrap();
    fs::write(path.join("a").join("y.txt"), b"yy").unwrap();
    let (_, seq) = rescan(&vault, 0);

    fs::rename(path.join("a"), path.join("b")).unwrap();
    let seq = {
        let _deny = common::DenyAce::new(&path.join("b").join("locked"), "(RD)");
        if fs::read_dir(path.join("b").join("locked")).is_ok() {
            common::skip(
                "unreadable-rename",
                "拒否を付けても一覧を読めた（管理者など）",
            );
            return;
        }
        let (found, seq) = rescan(&vault, seq);
        assert_eq!(found, [change("external_rename", "a", Some("b"))]);
        assert!(index_keys(&path).contains(&"b/locked/x.txt".to_owned()));
        seq
    };
    // 拒否を外しても、作成は出ない
    let (found, _) = rescan(&vault, seq);
    assert!(found.is_empty(), "{found:?}");
}

/// 大きさを変えずに、更新時刻だけを変えた更新も出る。
#[test]
fn a_change_of_only_the_modified_time_is_reported() {
    let dir = TestDir::new("scan-mtime");
    let (path, vault) = scan_vault(&dir);
    fs::write(path.join("m.txt"), b"same").unwrap();
    let (_, seq) = rescan(&vault, 0);
    let file = OpenOptions::new()
        .write(true)
        .open(path.join("m.txt"))
        .unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    drop(file);
    let (found, _) = rescan(&vault, seq);
    assert_eq!(found, [change("external_modify", "m.txt", None)]);
    let modify = journal(&path)
        .into_iter()
        .find(|e| e.op == Op::ExternalModify)
        .unwrap();
    assert_eq!(modify.size, Some(4), "大きさは同じ");
}

/// まだ索引にない（走査の前に外で作られた）ものを vault_move しても、行き先を索引に入れるので、
/// あとの走査で作成と出ない。
#[test]
fn moving_items_not_yet_in_the_index() {
    let dir = TestDir::new("scan-move-unindexed");
    let (path, vault) = scan_vault(&dir);
    fs::write(path.join("ext.txt"), b"e").unwrap();
    fs::create_dir_all(path.join("extdir").join("sub")).unwrap();
    fs::write(path.join("extdir").join("sub").join("f.txt"), b"f").unwrap();
    call(
        &vault,
        "vault_move",
        json!({"from": "ext.txt", "to": "moved.txt"}),
    )
    .unwrap();
    call(&vault, "vault_move", json!({"from": "extdir", "to": "d2"})).unwrap();
    assert_eq!(
        index_keys(&path),
        ["d2", "d2/sub", "d2/sub/f.txt", "moved.txt"]
    );
    let (found, _) = rescan(&vault, 0);
    assert_eq!(
        found,
        [
            change("init", "", None),
            change("move", "ext.txt", Some("moved.txt")),
            change("move", "extdir", Some("d2")),
        ]
    );
}

/// 索引のファイルがふつうのファイルでない（フォルダ）なら NOT_A_VAULT（open でも走査でも）。
/// 索引を作り直す回は、直前の行が index_rebuilt でも index_rebuilt を書く（確認の結果、変えた。
/// 同じときに歩いているほかの走査が、索引の作り直しを履歴の行で知るため）。
#[test]
fn index_files_that_are_not_files_and_repeated_rebuilds() {
    let dir = TestDir::new("scan-index-file");
    let (path, vault) = scan_vault(&dir);
    drop(vault);
    let index_file = path.join(".vault").join("index.json");
    fs::remove_file(&index_file).unwrap();
    fs::create_dir(&index_file).unwrap();
    for read_only in [false, true] {
        expect_code(
            Vault::open(&path, read_only, &env()),
            ErrorCode::NotAVault,
            "index.json がフォルダ",
        );
    }
    fs::remove_dir(&index_file).unwrap();
    let mut vault = Vault::open(&path, false, &env()).unwrap();
    vault.set_min_rescan_interval(Duration::ZERO);
    vault.set_rescan_interval_factor(0); // 前の走査の時間で伸ばさない（続けて走査する）
    let (found, seq) = rescan(&vault, 0);
    assert_eq!(
        found,
        [change("init", "", None), change("index_rebuilt", "", None)],
        "なかった索引は作り直す"
    );
    // 開いたあとでフォルダにされたら、走査も NOT_A_VAULT（作り直しを繰り返さない）
    fs::remove_file(&index_file).unwrap();
    fs::create_dir(&index_file).unwrap();
    expect_code(
        call(&vault, "vault_changes", json!({})),
        ErrorCode::NotAVault,
        "走査のときに index.json がフォルダ",
    );
    fs::remove_dir(&index_file).unwrap();
    // 直前の行が index_rebuilt でも、索引を作り直す回は書く
    let (found, seq) = rescan(&vault, seq);
    assert_eq!(found, [change("index_rebuilt", "", None)]);
    assert!(index_file.is_file());
    fs::write(path.join("a.txt"), b"a").unwrap();
    let (found, seq) = rescan(&vault, seq);
    assert_eq!(found, [change("external_create", "a.txt", None)]);
    fs::write(&index_file, b"broken").unwrap();
    let (found, _) = rescan(&vault, seq);
    assert_eq!(found, [change("index_rebuilt", "", None)]);
}

/// 外で書き換えられた履歴の path・to（絶対パス・`..`・UNC）は返さない。索引のキーに絶対パス・`../` があれば作り直す。
#[test]
fn journal_lines_and_index_keys_outside_the_vault_are_rejected() {
    let dir = TestDir::new("scan-outside-paths");
    let (path, vault) = scan_vault(&dir);
    let journal_file = fs::read_dir(path.join(".vault").join("journal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .max()
        .unwrap();
    let line = |seq: u64, path: &str, to: Option<&str>| {
        let mut v = json!({
            "seq": seq, "ts": "2026-11-16T00:00:00.000Z",
            "actor": {"kind": "external", "id": "fs", "session": "t"},
            "op": "external_rename", "path": path,
        });
        if let Some(to) = to {
            v["to"] = json!(to);
        }
        v.to_string() + "\n"
    };
    let mut text = fs::read_to_string(&journal_file).unwrap();
    text += &line(2, r"C:\Windows\system.ini", None);
    text += &line(3, "../secret.txt", None);
    text += &line(4, r"\\server\share\x", None);
    text += &line(5, "ok.txt", Some("../../escape.txt"));
    text += &line(6, "/etc/passwd", None);
    text += &line(7, "good.txt", Some("better.txt"));
    fs::write(&journal_file, text).unwrap();
    let (found, all) = changes_with(&vault, json!({"rescan": false}));
    assert_eq!(
        found,
        [
            change("init", "", None),
            change("external_rename", "good.txt", Some("better.txt")),
        ]
    );
    assert_eq!(all["next_seq"], 7);

    for bad_key in ["C:/Windows/system.ini", "../outside.txt", "a/../../b"] {
        fs::write(path.join("a.txt"), b"a").unwrap();
        let index = json!({
            "a.txt": {"kind": "file", "size": 1, "mtime_ns": 1},
            bad_key: {"kind": "file", "size": 1, "mtime_ns": 1},
        });
        fs::write(path.join(".vault").join("index.json"), index.to_string()).unwrap();
        let (_, seq_before) = changes_with(&vault, json!({"rescan": false}));
        let since = seq_before["next_seq"].as_u64().unwrap();
        let (found, _) = rescan(&vault, since);
        assert!(
            found == [change("index_rebuilt", "", None)] || found.is_empty(),
            "{bad_key}: {found:?}"
        );
        assert_eq!(index_keys(&path), ["a.txt"], "{bad_key}");
    }
}

/// 履歴の seq があふれるなら NOT_A_VAULT。
#[test]
fn journal_seq_overflow_is_not_a_vault() {
    let dir = TestDir::new("scan-seq-overflow");
    let (path, vault) = scan_vault(&dir);
    let journal_file = fs::read_dir(path.join(".vault").join("journal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .max()
        .unwrap();
    let mut text = fs::read_to_string(&journal_file).unwrap();
    text += &json!({
        "seq": u64::MAX, "ts": "2026-11-16T00:00:00.000Z",
        "actor": {"kind": "external", "id": "fs", "session": "t"}, "op": "external_create", "path": "x",
    })
    .to_string();
    text.push('\n');
    fs::write(&journal_file, text).unwrap();
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "a.txt", "content": "a"}),
        ),
        ErrorCode::NotAVault,
        "seq があふれる",
    );
    assert!(
        !path.join("a.txt").exists(),
        "履歴に書けないので、変更もしない"
    );
    expect_code(
        call(&vault, "vault_changes", json!({})),
        ErrorCode::NotAVault,
        "走査でも",
    );
    // 名前が最大の seq の履歴ファイル
    let dir2 = TestDir::new("scan-seq-overflow-name");
    let (path2, vault2) = scan_vault(&dir2);
    fs::write(
        path2
            .join(".vault")
            .join("journal")
            .join(format!("{:020}.jsonl", u64::MAX)),
        b"",
    )
    .unwrap();
    expect_code(
        call(&vault2, "vault_mkdir", json!({"path": "d"})),
        ErrorCode::NotAVault,
        "名前が最大の seq",
    );
}

/// ツールで扱えない名前（UTF-8 にできない・予約名・~ と数字・255 バイトを超える要素・末尾の .）は、
/// vault_list にも走査にも出さず、数だけを返す。
#[cfg(windows)]
#[test]
fn invalid_names_are_counted_but_not_listed_or_scanned() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let dir = TestDir::new("scan-invalid-names");
    let (path, vault) = scan_vault(&dir);
    let root = fs::canonicalize(&path).unwrap(); // \\?\ の付いた形なら、予約名や末尾の . も作れる
    let long_jp = "あ".repeat(90); // 270 バイト（UTF-16 では 90）
    let unpaired = OsString::from_wide(&[0xD800, u16::from(b'x')]);
    let names: Vec<OsString> = vec![
        "CON.txt".into(),
        "file~1.txt".into(),
        "trail.".into(),
        long_jp.clone().into(),
        unpaired,
    ];
    for name in &names {
        fs::write(root.join(name), b"x").unwrap();
    }
    fs::create_dir(root.join("bad~2")).unwrap();
    fs::write(root.join("bad~2").join("inside.txt"), b"x").unwrap();
    fs::write(root.join("ok.txt"), b"ok").unwrap();

    let list = call(&vault, "vault_list", json!({"recursive": true})).unwrap();
    let listed: Vec<&str> = list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["ok.txt"]);
    assert_eq!(
        list["skipped_invalid_names"], 6,
        "5つのファイルと bad~2（中は数えない）"
    );
    assert_eq!(list["skipped_over_limit"], 0);

    let (found, result) = changes_with(&vault, json!({}));
    assert_eq!(
        found,
        [
            change("init", "", None),
            change("external_create", "ok.txt", None)
        ]
    );
    assert_eq!(result["skipped_invalid_names"], 6);
    assert_eq!(index_keys(&path), ["ok.txt"]);

    // 片づけ（\\?\ の形で消す）
    for name in &names {
        fs::remove_file(root.join(name)).unwrap();
    }
}

/// 保管庫のフォルダがリンク（ジャンクション）に差し替えられたら、走査は中へ進まない（NOT_A_VAULT）。
#[cfg(windows)]
#[test]
fn a_vault_folder_replaced_by_a_junction_is_not_walked() {
    let dir = TestDir::new("scan-root-junction");
    let (path, vault) = scan_vault(&dir);
    let moved = dir.join("v-moved");
    fs::rename(&path, &moved).unwrap();
    if !common::junction(&path, &moved) {
        return;
    }
    expect_code(
        call(&vault, "vault_changes", json!({})),
        ErrorCode::NotAVault,
        "根がジャンクション",
    );
}

/// 1万ファイルの保管庫で、走査1回の時間を測る。
/// `cargo test --release -p vault-core --test scan -- --ignored --nocapture`
#[test]
#[ignore = "1万ファイルを作って時間を測る（--ignored で動かす）"]
fn time_a_scan_of_ten_thousand_files() {
    let dir = TestDir::new("scan-timing");
    let (path, vault) = scan_vault(&dir);
    for d in 0..100 {
        let folder = path.join(format!("d{d:03}"));
        fs::create_dir(&folder).unwrap();
        for f in 0..100 {
            fs::write(folder.join(format!("f{f:03}.txt")), format!("{d}-{f}")).unwrap();
        }
    }
    let start = Instant::now();
    let (found, _) = rescan(&vault, 0);
    let first = start.elapsed();
    assert_eq!(found.len(), 200, "既定の limit で切る");
    let entries = journal(&path);
    assert_eq!(entries.len(), 1 + 100 + 10_000, "init と、作成 10100 件");
    let mut seq = entries.last().unwrap().seq;

    let mut steady: Vec<Duration> = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let (found, next) = rescan(&vault, seq);
        steady.push(start.elapsed());
        assert!(found.is_empty());
        seq = next;
    }
    steady.sort();
    let start = Instant::now();
    let _ = changes_with(&vault, json!({"since_seq": seq, "rescan": false}));
    let no_rescan = start.elapsed();
    println!(
        "走査（1万ファイル・100フォルダ）：最初（作成 10100 件を記録）{first:?}／変更なし：最小 {:?}・中央 {:?}・最大 {:?}／rescan:false {no_rescan:?}",
        steady[0],
        steady[steady.len() / 2],
        steady[steady.len() - 1],
    );
}

/// フォルダがとても多い保管庫（9.6万）で、走査の時間と、そのあいだの書き込みの待ち時間を測る。
/// `cargo test --release -p vault-core --test scan -- --ignored --nocapture`
#[test]
#[ignore = "9.6万のフォルダを作って時間を測る（--ignored で動かす）"]
fn time_a_scan_of_many_folders() {
    let dir = TestDir::new("scan-timing-folders");
    let (path, vault) = scan_vault(&dir);
    for a in 0..300 {
        let parent = path.join(format!("d{a:03}"));
        fs::create_dir(&parent).unwrap();
        for b in 0..320 {
            fs::create_dir(parent.join(format!("e{b:03}"))).unwrap();
        }
    }
    let start = Instant::now();
    let (_, first) = changes_with(&vault, json!({"limit": 1}));
    let first_time = start.elapsed();
    assert_eq!(first["scanned"], true);
    // 変更のない走査で、フォルダを持っていた時間も測る（そのあいだ、ほかのアプリはそのフォルダの名前を変えられない）
    vault_core::test_support::record_dir_holds();
    let start = Instant::now();
    let (found, steady) = changes_with(&vault, json!({"since_seq": u64::MAX - 1}));
    let steady_time = start.elapsed();
    let holds = vault_core::test_support::take_dir_holds();
    assert_eq!(steady["scanned"], true);
    assert!(found.is_empty());
    let root_hold = holds
        .iter()
        .filter(|h| h.depth == 0)
        .map(|h| h.held)
        .max()
        .unwrap_or_default();
    let longest = holds
        .iter()
        .filter(|h| h.depth > 0)
        .max_by_key(|h| h.held)
        .copied();
    let over_a_second = holds
        .iter()
        .filter(|h| h.depth > 0 && h.held >= Duration::from_secs(1))
        .count();
    println!(
        "持っていた時間（変更なしの走査・フォルダ {}）：根 {root_hold:?}／根でないもので最長 {:?}（深さ {}）／1 秒以上 {over_a_second} 個",
        holds.len(),
        longest.map(|h| h.held).unwrap_or_default(),
        longest.map_or(0, |h| h.depth),
    );

    // 変更のない走査のあいだに、別のスレッドから書く（待ち時間を測る）
    let writer = Vault::open(&path, false, &env()).unwrap();
    let done = AtomicBool::new(false);
    let (scan_time, waits) = thread::scope(|s| {
        let writes = s.spawn(|| {
            let mut waits = Vec::new();
            let mut i = 0;
            while !done.load(Ordering::Relaxed) {
                let start = Instant::now();
                call(
                    &writer,
                    "vault_write",
                    json!({"path": format!("w-{i}.txt"), "content": "w"}),
                )
                .unwrap();
                waits.push(start.elapsed());
                i += 1;
                thread::sleep(Duration::from_millis(50));
            }
            waits
        });
        let start = Instant::now();
        let _ = changes_with(&vault, json!({"since_seq": u64::MAX - 1}));
        let scan_time = start.elapsed();
        done.store(true, Ordering::Relaxed);
        (scan_time, writes.join().unwrap())
    });
    println!(
        "走査（フォルダ 96300）：最初（作成を記録）{first_time:?}／変更なし {steady_time:?}／書き込みのあった走査 {scan_time:?}、そのあいだの書き込み {} 回・最長の待ち {:?}",
        waits.len(),
        waits.iter().max().unwrap()
    );
}

/// 走査で索引を読んだ直後（歩く前）に、別の Vault が書いて commit しても、そのファイルを外での変更として
/// 記録しない（印を、索引を読むより前に取るので、その書き込みは印の seq より後の行として合わせ直す）。
#[test]
fn a_core_write_between_reading_the_index_and_walking_is_not_reported() {
    use std::cell::Cell;
    use std::rc::Rc;
    use vault_core::test_support::{Step, on_next};

    let dir = TestDir::new("scan-index-race");
    let (path, vault) = scan_vault(&dir);
    call(
        &vault,
        "vault_write",
        json!({"path": "f.txt", "content": "x"}),
    )
    .unwrap();
    let wrote = Rc::new(Cell::new(false));
    let flag = wrote.clone();
    let p = path.clone();
    on_next(Step::AfterScanIndexLoaded, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        call(
            &other,
            "vault_write",
            json!({"path": "g.txt", "content": "g"}),
        )
        .unwrap();
        flag.set(true);
    });
    let (_, first) = changes_with(&vault, json!({}));
    assert!(wrote.get(), "差し込み口が呼ばれていない");
    assert_eq!(
        first["scanned"], true,
        "書き込みが入った回も、合わせて終える"
    );
    let (_, second) = changes_with(&vault, json!({}));
    assert_eq!(second["scanned"], true);
    let external: Vec<_> = journal(&path)
        .into_iter()
        .filter(|e| e.op == Op::ExternalCreate)
        .map(|e| e.path)
        .collect();
    assert!(
        external.is_empty(),
        "コアの書き込みを外での変更として記録した：{external:?}"
    );
}
