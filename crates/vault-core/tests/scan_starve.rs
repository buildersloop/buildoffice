//! 走査の飢えと、走査の間隔。
//!
//! - 歩くあいだにコアが書いても、その回の結果を捨てずに、コアが書いたパスだけをロックの中で調べ直して合わせる。
//!   外での変更はその回で記録し、コアの書き込みを外での変更と二重に記録しない。削除と取り違えない。
//! - 合わせ直せない（調べ直すパスを開けない）ときは、続けて捨てるのは2回まで。3回目は、そのパスに「次の走査で
//!   調べ直す」印を付けて終える。
//! - 走査の最短の間隔は、最短 2 秒と、前の走査（捨てたものも）にかかった時間の 10 倍（10 分まで）の長いほう。
//!
//! 時間は偽の時計（`advance_scan_clock`・`fake_scan_duration`）で進める（本当には待たない）。

#![cfg(windows)]

mod common;

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::{TestDir, call, env, journal, new_vault};
use serde_json::{Value, json};
use vault_core::test_support::{
    Step, advance_scan_clock, after_next_walk, fake_scan_duration, on_next, record_dir_holds,
    scan_rechecks, take_dir_holds,
};
use vault_core::{MAX_LOCKED_RECHECKS, MAX_LOCKED_SCAN_TIME, Op, Vault};

/// 偽の時計を十分に進めてから、`since` より後の変更を読む（走査する）。返り値は (変更の (op, path), 返り値の全体)。
fn scan(vault: &Vault, since: u64) -> (Vec<(Op, String)>, Value) {
    advance_scan_clock(Duration::from_secs(3600));
    scan_now(vault, since)
}

/// 偽の時計を進めずに、走査を求める。
fn scan_now(vault: &Vault, since: u64) -> (Vec<(Op, String)>, Value) {
    let result = call(
        vault,
        "vault_changes",
        json!({"since_seq": since, "limit": 10000}),
    )
    .unwrap();
    let changes = result["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                serde_json::from_value::<Op>(c["op"].clone()).unwrap(),
                c["path"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    (changes, result)
}

fn next_seq(result: &Value) -> u64 {
    result["next_seq"].as_u64().unwrap()
}

/// 外での変更として記録した (op, path)。
fn external(path: &Path) -> Vec<(Op, String)> {
    journal(path)
        .into_iter()
        .filter(|e| {
            matches!(
                e.op,
                Op::ExternalCreate | Op::ExternalModify | Op::ExternalDelete | Op::ExternalRename
            )
        })
        .map(|e| (e.op, e.path.unwrap_or_default()))
        .collect()
}

fn index_keys(path: &Path) -> Vec<String> {
    let index: Value =
        serde_json::from_slice(&fs::read(path.join(".vault").join("index.json")).unwrap()).unwrap();
    index.as_object().unwrap().keys().cloned().collect()
}

/// 別の Vault（同じプロセスの、ほかのツールの呼び出し）で書く。
fn core_write(path: &Path, rel: &str, content: &str) {
    let other = Vault::open(path, false, &env()).unwrap();
    call(
        &other,
        "vault_write",
        json!({"path": rel, "content": content}),
    )
    .unwrap();
}

/// `dir` を、ほかのアプリが DELETE の権利で開いている状態にする（そのあいだ、こちらはそのフォルダを開けない）。
fn hold_with_delete(dir: &Path) -> fs::File {
    const DELETE: u32 = 0x0001_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(7)
        .custom_flags(BACKUP_SEMANTICS)
        .open(dir)
        .unwrap()
}

/// 下のフォルダ `d`（走査が持つ）と、その中のファイルがある保管庫。最初の走査まで済ませる。
fn vault_with_folder(dir: &TestDir) -> (PathBuf, Vault, u64) {
    let (path, vault) = new_vault(dir, "v");
    fs::create_dir(path.join("d")).unwrap();
    fs::write(path.join("d").join("x.txt"), "x").unwrap();
    let (_, first) = scan(&vault, 0);
    assert_eq!(first["scanned"], true);
    (path, vault, next_seq(&first))
}

/// 書き込みが絶えず入る（歩いているあいだに毎回コアが書く）保管庫でも、走査は毎回終わる。外での変更はその回で
/// 記録し、コアの書き込みは外での変更として記録しない。
#[test]
fn scans_finish_even_when_the_core_writes_during_every_walk() {
    let dir = TestDir::new("b0-starve");
    let (path, vault, mut seq) = vault_with_folder(&dir);
    for i in 0..3 {
        fs::write(path.join(format!("ext{i}.txt")), "e").unwrap();
        let p = path.clone();
        on_next(Step::WalkHolding, move || {
            core_write(&p, &format!("core{i}.txt"), "c");
            core_write(&p, &format!("d/core{i}.txt"), "c");
        });
        let (found, result) = scan(&vault, seq);
        assert_eq!(
            result["scanned"], true,
            "{i} 回目：書き込みのあった走査を捨てた"
        );
        assert!(
            found.contains(&(Op::ExternalCreate, format!("ext{i}.txt"))),
            "{i} 回目：外での変更を記録していない：{found:?}"
        );
        seq = next_seq(&result);
    }
    let external = external(&path);
    assert!(
        external.iter().all(|(_, p)| !p.contains("core")),
        "コアの書き込みを外での変更と記録した：{external:?}"
    );
    let keys = index_keys(&path);
    for i in 0..3 {
        for key in [
            format!("core{i}.txt"),
            format!("d/core{i}.txt"),
            format!("ext{i}.txt"),
        ] {
            assert!(keys.contains(&key), "索引に {key} がない：{keys:?}");
        }
    }
}

/// ロックの外で合わせたあと、ロックを取る直前にコアが書いても、そのパスはロックの中で合わせる（外での変更と
/// 記録しない）。
#[test]
fn a_core_write_just_before_the_lock_is_reconciled_inside_the_lock() {
    let dir = TestDir::new("b0-before-lock");
    let (path, vault, seq) = vault_with_folder(&dir);
    fs::write(path.join("ext.txt"), "e").unwrap();
    let p = path.clone();
    on_next(Step::WalkHolding, move || core_write(&p, "early.txt", "c"));
    let p = path.clone();
    on_next(Step::BeforeScanLock, move || {
        core_write(&p, "d/late.txt", "c")
    });
    let before = external(&path).len();
    let (_, result) = scan(&vault, seq);
    assert_eq!(result["scanned"], true);
    assert_eq!(
        external(&path)[before..],
        [(Op::ExternalCreate, "ext.txt".to_owned())]
    );
    let keys = index_keys(&path);
    assert!(keys.contains(&"early.txt".to_owned()) && keys.contains(&"d/late.txt".to_owned()));
}

/// 歩くあいだにコアが消す（ゴミ箱へ移す）・移す（まだ歩いていないフォルダごと）・上書きしても、外での変更と
/// 取り違えない（走査が持っているフォルダ m は移せないので、まだ持っていない d を移す）。
#[test]
fn core_moves_deletes_and_overwrites_during_the_walk_are_not_reported() {
    let dir = TestDir::new("b0-moves");
    let (path, vault, _) = vault_with_folder(&dir);
    let other = Vault::open(&path, false, &env()).unwrap();
    call(
        &other,
        "vault_write",
        json!({"path": "m/a.txt", "content": "a"}),
    )
    .unwrap();
    call(
        &other,
        "vault_write",
        json!({"path": "c.txt", "content": "c"}),
    )
    .unwrap();
    call(
        &other,
        "vault_write",
        json!({"path": "w.txt", "content": "w"}),
    )
    .unwrap();
    drop(other);
    let (_, base) = scan(&vault, 0);
    let seq = next_seq(&base);
    let before = external(&path).len();
    fs::write(path.join("ext.txt"), "e").unwrap();
    let p = path.clone();
    on_next(Step::WalkHolding, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        call(&other, "vault_move", json!({"from": "d", "to": "d2"})).unwrap();
        call(&other, "vault_delete", json!({"path": "c.txt"})).unwrap();
        call(
            &other,
            "vault_write",
            json!({"path": "w.txt", "content": "longer"}),
        )
        .unwrap();
    });
    let (found, result) = scan(&vault, seq);
    assert_eq!(result["scanned"], true);
    let external = external(&path);
    assert_eq!(
        external[before..],
        [(Op::ExternalCreate, "ext.txt".to_owned())],
        "外での変更は ext.txt だけ：{found:?}"
    );
    let keys = index_keys(&path);
    assert!(keys.contains(&"d2/x.txt".to_owned()) && !keys.contains(&"d/x.txt".to_owned()));
    assert!(keys.contains(&"m/a.txt".to_owned()));
    assert!(!keys.contains(&"c.txt".to_owned()));
}

/// 合わせ直せない（調べ直すパスのフォルダを開けない）ときは、続けて捨てるのは2回まで。3回目は、そのパスに
/// 「次の走査で調べ直す」印を付けて終える（外での変更はその回で記録し、そのパスは外での変更として記録しない）。
/// 次の走査で印は消え、コアの書き込みを外での変更とは記録しない。
#[test]
fn the_third_scan_finishes_and_marks_paths_it_could_not_reconcile() {
    let dir = TestDir::new("b0-third");
    let (path, vault, mut seq) = vault_with_folder(&dir);
    let baseline = external(&path).len();
    for attempt in 1..=3 {
        fs::write(path.join(format!("ext{attempt}.txt")), "e").unwrap();
        let p = path.clone();
        on_next(Step::WalkHolding, move || {
            core_write(&p, "d/x.txt", &format!("core-{attempt}"));
        });
        let holder: Rc<RefCell<Option<fs::File>>> = Rc::new(RefCell::new(None));
        let out = holder.clone();
        let d = path.join("d");
        after_next_walk(move || {
            *out.borrow_mut() = Some(hold_with_delete(&d));
        });
        let (found, result) = scan(&vault, seq);
        drop(holder.borrow_mut().take());
        if attempt < 3 {
            assert_eq!(
                result["scanned"], false,
                "{attempt} 回目は捨てる：{found:?}"
            );
        } else {
            assert_eq!(result["scanned"], true, "3 回目は終える");
            seq = next_seq(&result);
        }
    }
    let external = external(&path)[baseline..].to_vec();
    for attempt in 1..=3 {
        assert!(
            external.contains(&(Op::ExternalCreate, format!("ext{attempt}.txt"))),
            "外での変更を記録していない：{external:?}"
        );
    }
    assert!(
        external.iter().all(|(_, p)| p != "d/x.txt"),
        "コアの書き込みを外での変更と記録した：{external:?}"
    );
    assert_eq!(scan_rechecks(&vault), ["d/x.txt"], "印がない");

    // 次の走査（書き込みなし）：印は消え、d/x.txt を外での変更とは記録しない
    let (found, result) = scan(&vault, seq);
    assert_eq!(result["scanned"], true);
    assert!(found.is_empty(), "{found:?}");
    assert!(scan_rechecks(&vault).is_empty());
}

/// 走査の最短の間隔は、最短 2 秒と、前の走査にかかった時間の 10 倍（10 分まで）の長いほう。
#[test]
fn the_rescan_interval_grows_with_the_last_scan_duration() {
    let dir = TestDir::new("b0-interval");
    let (path, vault) = new_vault(&dir, "v");
    advance_scan_clock(Duration::from_secs(3600));
    fake_scan_duration(Duration::from_secs(10));
    let (_, first) = scan_now(&vault, 0);
    assert_eq!(first["scanned"], true);
    let seq = next_seq(&first);
    fs::write(path.join("a.txt"), "a").unwrap();

    advance_scan_clock(Duration::from_secs(60));
    let (found, soon) = scan_now(&vault, seq);
    assert_eq!(
        soon["scanned"], false,
        "前の走査の 10 倍（100 秒）より前は走査しない"
    );
    assert!(found.is_empty());

    advance_scan_clock(Duration::from_secs(41));
    fake_scan_duration(Duration::from_secs(2 * 3600));
    let (found, later) = scan_now(&vault, seq);
    assert_eq!(later["scanned"], true, "100 秒を過ぎたら走査する");
    assert_eq!(found, [(Op::ExternalCreate, "a.txt".to_owned())]);

    // 前の走査がとても長くても、間隔は 10 分まで
    advance_scan_clock(Duration::from_secs(9 * 60));
    assert_eq!(scan_now(&vault, seq).1["scanned"], false);
    advance_scan_clock(Duration::from_secs(61));
    assert_eq!(
        scan_now(&vault, seq).1["scanned"],
        true,
        "10 分を過ぎたら走査する"
    );
}

/// 捨てた走査も、間隔に数える（すぐ次の rescan で、また根を持って歩かない）。
#[test]
fn discarded_scans_count_toward_the_interval() {
    let dir = TestDir::new("b0-discard-interval");
    let (path, vault, seq) = vault_with_folder(&dir);
    let p = path.clone();
    on_next(Step::WalkHolding, move || core_write(&p, "d/x.txt", "core"));
    let holder: Rc<RefCell<Option<fs::File>>> = Rc::new(RefCell::new(None));
    let out = holder.clone();
    let d = path.join("d");
    after_next_walk(move || {
        *out.borrow_mut() = Some(hold_with_delete(&d));
    });
    advance_scan_clock(Duration::from_secs(3600));
    fake_scan_duration(Duration::from_secs(10));
    let (_, discarded) = scan_now(&vault, seq);
    drop(holder.borrow_mut().take());
    assert_eq!(discarded["scanned"], false, "合わせ直せないので捨てる");

    let walked = Rc::new(Cell::new(false));
    let flag = walked.clone();
    on_next(Step::AfterScanIndexLoaded, move || flag.set(true));
    advance_scan_clock(Duration::from_secs(5));
    let (_, soon) = scan_now(&vault, seq);
    assert_eq!(soon["scanned"], false);
    assert!(!walked.get(), "捨てた走査のすぐあとに、また歩いた");

    advance_scan_clock(Duration::from_secs(100));
    let (_, later) = scan_now(&vault, seq);
    assert_eq!(later["scanned"], true);
    assert!(walked.get());
}

/// 読み取り専用で開いた保管庫（--read-only の MCP）から vault_changes を繰り返し呼んでも、走査（根を持って歩く）
/// のは、前の走査の時間の 10 倍に1回まで。
#[test]
fn repeated_rescans_from_a_read_only_vault_do_not_keep_holding_the_root() {
    let dir = TestDir::new("b0-read-only");
    let (path, _vault) = new_vault(&dir, "v");
    fs::create_dir(path.join("d")).unwrap();
    let ro = Vault::open(&path, true, &env()).unwrap();
    advance_scan_clock(Duration::from_secs(3600));
    fake_scan_duration(Duration::from_secs(10));
    assert_eq!(scan_now(&ro, 0).1["scanned"], true);

    record_dir_holds();
    for _ in 0..20 {
        advance_scan_clock(Duration::from_secs(4));
        scan_now(&ro, 0);
    }
    let roots = take_dir_holds().iter().filter(|h| h.depth == 0).count();
    assert_eq!(roots, 0, "80 秒のあいだに、根を持って歩いた（{roots} 回）");

    record_dir_holds();
    advance_scan_clock(Duration::from_secs(30));
    assert_eq!(scan_now(&ro, 0).1["scanned"], true);
    let roots = take_dir_holds().iter().filter(|h| h.depth == 0).count();
    assert_eq!(roots, 1);
}

// ---------------------------------------------------------------------------
// 点検での直し（反例と、ロックの中の仕事の上限）
// ---------------------------------------------------------------------------

/// 大文字小文字だけが違う名前で、歩くあいだにコアが上書きしても、外での変更と記録しない。
/// 索引に、大文字小文字だけが違うキーが2つ残らない。
#[test]
fn case_variant_overwrite_during_walk() {
    let dir = TestDir::new("b0-case-overwrite");
    let (path, vault, _) = vault_with_folder(&dir);
    fs::write(path.join("README.md"), "orig").unwrap();
    let (_, base) = scan(&vault, 0);
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::WalkHolding, move || {
        core_write(&p, "readme.md", "new content")
    });
    let (_, first) = scan(&vault, next_seq(&base));
    let (_, second) = scan(&vault, next_seq(&first));
    assert_eq!(first["scanned"], true);
    assert_eq!(second["scanned"], true);
    assert_eq!(external(&path)[before..], [], "外での変更と記録した");
    assert_no_case_twins(&path);
    assert!(index_keys(&path).contains(&"readme.md".to_owned()));
}

/// 歩くあいだに、作って・名前を変えて・大文字小文字だけ違う名前へ移しても、履歴に残った古い大文字小文字の名前を
/// 索引に入れない。
#[test]
fn stale_case_name_in_journal_during_walk() {
    let dir = TestDir::new("b0-case-stale");
    let (path, vault, seq) = vault_with_folder(&dir);
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::WalkHolding, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        call(
            &other,
            "vault_write",
            json!({"path": "Note.TXT", "content": "1"}),
        )
        .unwrap();
        call(
            &other,
            "vault_move",
            json!({"from": "Note.TXT", "to": "tmp.txt"}),
        )
        .unwrap();
        call(
            &other,
            "vault_move",
            json!({"from": "tmp.txt", "to": "note.txt"}),
        )
        .unwrap();
    });
    let (_, first) = scan(&vault, seq);
    let (_, second) = scan(&vault, next_seq(&first));
    assert_eq!(second["scanned"], true);
    assert_eq!(external(&path)[before..], [], "外での変更と記録した");
    assert_no_case_twins(&path);
    assert!(index_keys(&path).contains(&"note.txt".to_owned()));
}

/// コアが "a/y.txt" を書いた（履歴には "a/y.txt"）あとで、外のアプリが親の大文字小文字だけを変える（a→A）。
/// 歩いた結果（"A" の下）に、履歴の古い名前 "a/y.txt" を入れて二重にしない。
/// 実際の名前（"A/y.txt"）で合わせ、外での変更は親の名前の変更だけ。次の走査では何も出ない。
#[test]
fn a_parent_case_change_after_a_core_write_is_not_doubled() {
    let dir = TestDir::new("b0-case-parent");
    let (path, vault, _) = vault_with_folder(&dir);
    fs::create_dir(path.join("a")).unwrap();
    fs::write(path.join("a").join("x.txt"), "x").unwrap();
    let (_, base) = scan(&vault, 0);
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::AfterScanIndexLoaded, move || {
        core_write(&p, "a/y.txt", "y");
        fs::rename(p.join("a"), p.join("A")).unwrap();
    });
    let (_, first) = scan(&vault, next_seq(&base));
    let (_, second) = scan(&vault, next_seq(&first));
    let (_, third) = scan(&vault, next_seq(&second));
    assert_eq!(third["scanned"], true);
    assert_no_case_twins(&path);
    let external = external(&path)[before..].to_vec();
    assert!(
        external.iter().all(|(_, p)| !p.contains("y.txt")),
        "コアの書き込みを外での変更と記録した：{external:?}"
    );
    let keys = index_keys(&path);
    assert!(
        keys.contains(&"A/y.txt".to_owned()) && keys.contains(&"A/x.txt".to_owned()),
        "{keys:?}"
    );
}

/// 別の走査（アプリと MCP など）が、歩くあいだに索引を作り直しても（index_rebuilt。パスのない行）、実在する
/// ファイルを削除と記録しない（反例）。パスのない行は、合わせ直せない（gap）として扱う。
#[test]
fn other_scanner_rebuilds_index_during_walk() {
    let dir = TestDir::new("b0-rebuild-race");
    let (path, vault, seq) = vault_with_folder(&dir);
    fs::remove_file(path.join(".vault").join("index.json")).unwrap();
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::WalkHolding, move || {
        fs::write(p.join("z.txt"), "z").unwrap();
        let mut other = Vault::open(&p, false, &env()).unwrap();
        other.set_min_rescan_interval(Duration::ZERO);
        other.set_rescan_interval_factor(0);
        call(&other, "vault_changes", json!({})).unwrap();
    });
    let (_, first) = scan(&vault, seq);
    let (_, second) = scan(&vault, next_seq(&first));
    assert_eq!(second["scanned"], true);
    assert_eq!(
        external(&path)[before..],
        [],
        "実在するファイルを削除と記録した"
    );
    assert!(index_keys(&path).contains(&"z.txt".to_owned()));
}

/// 履歴の行が欠けている（seq は正しいが path が規則に通らない行）：2回捨て、3回目は記録せず索引も変えずに終え、
/// 未完了として scanned:false と scan_incomplete:true を返し、保管庫の全体に印を付ける。4回目は、続けて捨てた回数が
/// 0 に戻っていないので、すぐ未完了（incomplete_scans は 2）。次の走査で外での変更が出て、回数は 0 に戻る
#[test]
fn a_gap_in_the_journal_is_discarded_twice_then_left_incomplete() {
    let dir = TestDir::new("b0-gap");
    let (path, vault, seq) = vault_with_folder(&dir);
    let base = external(&path).len();
    fs::write(path.join("ext.txt"), "e").unwrap();
    // 4回目：未完了のあとも続けて捨てた回数は 0 に戻らないので、すぐ未完了（2回捨てて1回未完了、を繰り返させない）。未完了は、返り値の欄で、間隔が来ていない・捨てたときと見分けられる
    for attempt in 1_u32..=4 {
        let p = path.clone();
        on_next(Step::WalkHolding, move || {
            let jdir = p.join(".vault").join("journal");
            let mut files: Vec<_> = fs::read_dir(&jdir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            files.sort();
            let seq = journal(&p).last().unwrap().seq + 1;
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(files.last().unwrap())
                .unwrap();
            writeln!(
                f,
                "{{\"seq\":{seq},\"ts\":\"2026-11-16T00:00:00Z\",\"actor\":{{\"kind\":\"user\",\"id\":\"x\"}},\"op\":\"write\",\"path\":\"../evil\"}}"
            )
            .unwrap();
            drop(f);
            core_write(&p, &format!("core{attempt}.txt"), "c");
        });
        let (_, result) = scan(&vault, seq);
        assert_eq!(
            result["scanned"], false,
            "{attempt} 回目：未完了なのに scanned:true"
        );
        let incomplete = attempt >= 3;
        assert_eq!(result["scan_incomplete"], incomplete, "{attempt} 回目");
        assert_eq!(
            result["incomplete_scans"],
            attempt.saturating_sub(2),
            "{attempt} 回目：続けて未完了になった回数"
        );
        if incomplete {
            assert_eq!(scan_rechecks(&vault), [""], "保管庫の全体に印がない");
            assert!(
                !index_keys(&path).contains(&"ext.txt".to_owned()),
                "索引を変えた"
            );
        }
    }
    assert_eq!(external(&path)[base..], [], "欠けた回に記録した");
    let (_, next) = scan(&vault, 0);
    assert_eq!(next["scanned"], true);
    assert_eq!(next["scan_incomplete"], false);
    assert_eq!(next["incomplete_scans"], 0, "走査を終えたら 0 に戻る");
    assert_eq!(
        external(&path)[base..],
        [(Op::ExternalCreate, "ext.txt".to_owned())],
        "次の走査で外での変更が出ない"
    );
    assert!(scan_rechecks(&vault).is_empty());
}

/// ロックの中では、下を歩き直さない：ロックを取る直前にコアが大きなフォルダを移しても、
/// ロックの中ではそのフォルダの下を開かず（今の索引の値のまま、印を付ける）、ロックを持つ時間は短い。
/// 外での変更とは記録せず、次の走査で印は消える。
#[test]
fn a_big_folder_moved_just_before_the_lock_is_not_walked_inside_the_lock() {
    let dir = TestDir::new("b0-big-move");
    let (path, vault, _) = vault_with_folder(&dir);
    let big = path.join("big");
    for d in 0..10 {
        let sub = big.join(format!("s{d}"));
        fs::create_dir_all(&sub).unwrap();
        for f in 0..300 {
            fs::write(sub.join(format!("f{f}.txt")), "x").unwrap();
        }
    }
    let (_, base) = scan(&vault, 0);
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::BeforeScanLock, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        call(&other, "vault_move", json!({"from": "big", "to": "big2"})).unwrap();
    });
    let locked_at: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let at = locked_at.clone();
    on_next(Step::BeforeReconcile, move || {
        at.set(Some(Instant::now()));
        record_dir_holds();
    });
    let (_, first) = scan(&vault, next_seq(&base));
    let held = locked_at.get().expect("ロックを取っていない").elapsed();
    let walked_inside = take_dir_holds();
    assert_eq!(first["scanned"], true);
    assert!(
        walked_inside.is_empty(),
        "ロックの中で下を歩き直した（{} フォルダ）",
        walked_inside.len()
    );
    assert!(
        held < Duration::from_secs(3),
        "ロックを長く持った：{held:?}"
    );
    assert_eq!(scan_rechecks(&vault), ["big2"], "下を調べ直す印がない");
    assert_eq!(external(&path)[before..], [], "外での変更と記録した");
    let (_, second) = scan(&vault, next_seq(&first));
    assert_eq!(second["scanned"], true);
    assert_eq!(external(&path)[before..], [], "次の走査で外での変更が出た");
    assert!(scan_rechecks(&vault).is_empty());
    assert!(index_keys(&path).contains(&"big2/s9/f299.txt".to_owned()));
}

/// ロックの中で調べ直すパスが多すぎる（上限を超える）ときは、ロックを外して、ロックの外で合わせ直してから、
/// もう一度ロックを取る。外での変更とは記録しない。
#[test]
fn too_many_writes_just_before_the_lock_are_reconciled_outside_the_lock() {
    let dir = TestDir::new("b0-many-before-lock");
    let (path, vault, seq) = vault_with_folder(&dir);
    fs::write(path.join("ext.txt"), "e").unwrap();
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::BeforeScanLock, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        for i in 0..MAX_LOCKED_RECHECKS + 10 {
            call(
                &other,
                "vault_write",
                json!({"path": format!("w{i}.txt"), "content": "w"}),
            )
            .unwrap();
        }
    });
    let again = Rc::new(Cell::new(false));
    let flag = again.clone();
    on_next(Step::BeforeScanLock, move || flag.set(true));
    let (_, result) = scan(&vault, seq);
    assert_eq!(result["scanned"], true);
    assert!(again.get(), "ロックを外して合わせ直していない");
    assert_eq!(
        external(&path)[before..],
        [(Op::ExternalCreate, "ext.txt".to_owned())]
    );
    assert!(index_keys(&path).contains(&format!("w{}.txt", MAX_LOCKED_RECHECKS + 9)));
}

/// 索引に、大文字小文字だけが違うキーが2つない（Windows）。
fn assert_no_case_twins(path: &Path) {
    let keys = index_keys(path);
    let mut lower: Vec<String> = keys.iter().map(|k| k.to_lowercase()).collect();
    lower.sort();
    let before = lower.len();
    lower.dedup();
    assert_eq!(
        lower.len(),
        before,
        "大文字小文字だけが違うキーがある：{keys:?}"
    );
}

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

    fn pick(&mut self, items: &[&str]) -> String {
        items[usize::try_from(self.next() % items.len() as u64).unwrap()].to_owned()
    }
}

/// 乱数で選ぶ操作。コアの操作（大文字小文字だけが違う名前・作成・上書き・移動・削除・フォルダ作成・別の Vault の
/// 走査）と、外のアプリの操作（フォルダを DELETE の権利で持って開けなくする・索引を消す・x の下にファイルを本当に
/// 作る）。並びと選び方は、同じ種で同じ操作になる。
#[derive(Debug)]
enum RandomOp {
    Write(String, usize),
    Move(String, String),
    Delete(String),
    Mkdir(String),
    Scan,
    Hold(String),
    DropIndex,
    ExtWrite(u32),
}

/// 外のアプリが持っているフォルダ（その回の走査が終わるまで）。
type Holders = Rc<RefCell<Vec<fs::File>>>;

/// 混ぜる外のアプリの操作と、ロックを取った直後に走査の時計を進めるか（ロックの中の時間の上限に当てる）。
#[derive(Clone, Copy)]
struct Mode {
    holds: bool,
    drops: bool,
    ext: bool,
    clock: bool,
}

/// 流した操作の記録（失敗したときに文に出す）。
#[derive(Default)]
struct Log {
    ops: Vec<String>,
    ext_files: Vec<String>,
}

const NAMES: [&str; 16] = [
    "a.txt",
    "A.txt",
    "b.md",
    "B.MD",
    "d/x.txt",
    "D/X.txt",
    "g/h/i.txt",
    "G/H/i.TXT",
    "f1/n.txt",
    "F1/N.TXT",
    "e",
    "E",
    "d",
    "D",
    "g",
    "G/H",
];

impl RandomOp {
    fn pick(rng: &mut Rng, mode: Mode, ext_count: &mut u32) -> Self {
        let name = rng.pick(&NAMES);
        loop {
            match rng.next() % 9 {
                0 | 1 => {
                    return Self::Write(name, 1 + usize::try_from(rng.next() % 5).unwrap());
                }
                2 => return Self::Move(name, rng.pick(&NAMES)),
                3 => return Self::Delete(name),
                4 => return Self::Mkdir(rng.pick(&["e", "E", "g", "G/k"])),
                5 => return Self::Scan,
                6 if mode.holds => {
                    return Self::Hold(rng.pick(&["d", "f1", "F2", "g", "G/H", "e"]));
                }
                7 if mode.drops => return Self::DropIndex,
                8 if mode.ext => {
                    *ext_count += 1;
                    return Self::ExtWrite(*ext_count);
                }
                _ => {}
            }
        }
    }

    /// 行う（失敗してもよい。成功したコアの操作は、どれも履歴に残る）。
    fn run(&self, path: &Path, holders: &Holders, log: &Rc<RefCell<Log>>) {
        log.borrow_mut().ops.push(format!("{self:?}"));
        let other = Vault::open(path, false, &env()).unwrap();
        let _ = match self {
            Self::Write(name, len) => call(
                &other,
                "vault_write",
                json!({"path": name, "content": "x".repeat(*len)}),
            ),
            Self::Move(from, to) => call(&other, "vault_move", json!({"from": from, "to": to})),
            Self::Delete(name) => call(
                &other,
                "vault_delete",
                json!({"path": name, "recursive": true}),
            ),
            Self::Mkdir(name) => call(&other, "vault_mkdir", json!({"path": name})),
            Self::Scan => {
                let mut scanner = Vault::open(path, false, &env()).unwrap();
                scanner.set_min_rescan_interval(Duration::ZERO);
                scanner.set_rescan_interval_factor(0);
                call(&scanner, "vault_changes", json!({}))
            }
            Self::Hold(name) => {
                const DELETE: u32 = 0x0001_0000;
                const BACKUP_SEMANTICS: u32 = 0x0200_0000;
                if let Ok(file) = fs::OpenOptions::new()
                    .access_mode(DELETE)
                    .share_mode(7)
                    .custom_flags(BACKUP_SEMANTICS)
                    .open(path.join(name))
                {
                    holders.borrow_mut().push(file);
                }
                return;
            }
            Self::DropIndex => {
                let _ = fs::remove_file(path.join(".vault").join("index.json"));
                return;
            }
            Self::ExtWrite(n) => {
                let name = format!("x/ext{n}.txt");
                fs::write(path.join(&name), "e").unwrap();
                log.borrow_mut().ext_files.push(name);
                return;
            }
        };
    }
}

/// 歩くあいだ・歩き終えた直後・ロックを取る直前に、乱数で選んだ操作を差し込んで4回走査し、差し込みなしで2回
/// 走査する。確かめること：コアの操作を外での変更と記録しない（x の下で外のアプリが作ったもの以外に、外での変更が
/// ない）、索引に大文字小文字だけが違うキーが2つない、外のアプリが作ったファイルは索引に入る。索引を消さない形では、
/// 外のアプリが作ったファイルは、ちょうど1回ずつ外での作成として記録する（取りこぼしも二重もない）。索引を消す形では、
/// 作り直した索引に入ったものは記録されない（index_rebuilt は差分を出さない）ので、二重がないことだけを確かめる。
fn random_run(seed: u64, mode: Mode) -> Result<(), String> {
    let dir = TestDir::new(&format!("b0-random-{seed}"));
    let (path, vault, _) = vault_with_folder(&dir);
    for folder in ["f1", "f2", "f3", "x"] {
        fs::create_dir(path.join(folder)).unwrap();
        fs::write(path.join(folder).join("n.txt"), "n").unwrap();
    }
    let (_, base) = scan(&vault, 0);
    let mut seq = next_seq(&base);
    let before = external(&path).len();
    let rng = Rc::new(RefCell::new(Rng(seed)));
    let ext_count = Rc::new(Cell::new(0_u32));
    let holders: Holders = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::new(RefCell::new(Log::default()));
    // 差し込みを行うか（最後の走査の前に止める。使われずに残った差し込み口が、そこで呼ばれても何もしない）
    let active = Rc::new(Cell::new(true));
    let pick = {
        let ext_count = ext_count.clone();
        move |rng: &Rc<RefCell<Rng>>| {
            let mut n = ext_count.get();
            let op = RandomOp::pick(&mut rng.borrow_mut(), mode, &mut n);
            ext_count.set(n);
            op
        }
    };
    for _ in 0..4 {
        for _ in 0..3 {
            let (p, r, h, a, l, pk) = (
                path.clone(),
                rng.clone(),
                holders.clone(),
                active.clone(),
                log.clone(),
                pick.clone(),
            );
            on_next(Step::WalkHolding, move || {
                if !a.get() {
                    return;
                }
                // 先に決めてから行う（別の Vault の走査の中で、残りの差し込み口が呼ばれるため）
                let ops = [pk(&r), pk(&r)];
                for op in ops {
                    op.run(&p, &h, &l);
                }
            });
        }
        // ロックを取る直前（ロックの中で合わせる分）
        let (p, r, h, a, l, pk) = (
            path.clone(),
            rng.clone(),
            holders.clone(),
            active.clone(),
            log.clone(),
            pick.clone(),
        );
        on_next(Step::BeforeScanLock, move || {
            if !a.get() {
                return;
            }
            let op = pk(&r);
            op.run(&p, &h, &l);
        });
        let (p, r, h, a, l, pk) = (
            path.clone(),
            rng.clone(),
            holders.clone(),
            active.clone(),
            log.clone(),
            pick.clone(),
        );
        after_next_walk(move || {
            if !a.get() {
                return;
            }
            let op = pk(&r);
            op.run(&p, &h, &l);
        });
        if mode.clock {
            // ロックを取った直後に、半分の割合で走査の時計を上限より進める（ロックの中の時間の上限に当てる）
            for _ in 0..2 {
                let (r, a) = (rng.clone(), active.clone());
                on_next(Step::BeforeReconcile, move || {
                    if a.get() && r.borrow_mut().next() % 2 == 0 {
                        advance_scan_clock(MAX_LOCKED_SCAN_TIME * 2);
                    }
                });
            }
        }
        let (_, result) = scan(&vault, seq);
        holders.borrow_mut().clear();
        seq = next_seq(&result);
    }
    active.set(false);
    let ops = || log.borrow().ops.join(", ");
    for _ in 0..2 {
        let (_, result) = scan(&vault, seq);
        if result["scanned"] != true {
            return Err(format!(
                "差し込みなしの走査を終えなかった：{result}（操作：{}）",
                ops()
            ));
        }
        seq = next_seq(&result);
    }
    let keys = index_keys(&path);
    let mut lower: Vec<String> = keys.iter().map(|k| k.to_lowercase()).collect();
    lower.sort();
    let count = lower.len();
    lower.dedup();
    if lower.len() != count {
        return Err(format!("大文字小文字だけが違うキー：{keys:?}"));
    }
    let ext_files = log.borrow().ext_files.clone();
    if let Some(missing) = ext_files.iter().find(|f| !keys.contains(f)) {
        return Err(format!(
            "外のアプリが作ったファイルが索引にない：{missing}（操作：{}）",
            ops()
        ));
    }
    type Changes = Vec<(Op, String)>;
    let recorded = external(&path)[before..].to_vec();
    let (mut made, others): (Changes, Changes) =
        recorded.into_iter().partition(|(_, p)| p.starts_with("x/"));
    if !others.is_empty() {
        return Err(format!(
            "コアの操作を外での変更と記録した：{others:?}（操作：{}）",
            ops()
        ));
    }
    made.sort_by(|a, b| a.1.cmp(&b.1));
    if mode.drops {
        let mut names: Vec<&String> = made.iter().map(|(_, p)| p).collect();
        let n = names.len();
        names.dedup();
        if names.len() != n || made.iter().any(|(op, _)| *op != Op::ExternalCreate) {
            return Err(format!(
                "外のアプリが作ったファイルを二重に・作成以外で記録した：{made:?}"
            ));
        }
    } else {
        let mut expected: Vec<(Op, String)> = ext_files
            .iter()
            .map(|f| (Op::ExternalCreate, f.clone()))
            .collect();
        expected.sort_by(|a, b| a.1.cmp(&b.1));
        if made != expected {
            return Err(format!(
                "外のアプリが作ったファイルを、ちょうど1回ずつ記録していない：{made:?}（作った：{expected:?}。操作：{}）",
                ops()
            ));
        }
    }
    Ok(())
}

/// 流す種。`VAULT_RANDOM_SEED_FROM` と `VAULT_RANDOM_SEED_N` があれば、FROM から N 個（広く流すとき）。
fn random_seeds(default: &[u64]) -> Vec<u64> {
    let var = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    match (var("VAULT_RANDOM_SEED_FROM"), var("VAULT_RANDOM_SEED_N")) {
        (Some(from), Some(n)) => (from..from + n).collect(),
        _ => default.to_vec(),
    }
}

/// 種ごとに流し、落ちた種をまとめて文に出す。
fn random_runs(mode: Mode, default: &[u64]) {
    let failures: Vec<String> = random_seeds(default)
        .into_iter()
        .filter_map(|seed| {
            random_run(seed, mode)
                .err()
                .map(|e| format!("種 {seed}：{e}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// 開けないフォルダ・外のアプリが作るファイル・ロックの中の時間の上限を混ぜる（索引は消さない）。外のアプリが
/// 作ったファイルは、ちょうど1回ずつ外での作成として記録する。
#[test]
fn random_operations_with_held_folders_record_external_files_exactly_once() {
    random_runs(
        Mode {
            holds: true,
            drops: false,
            ext: true,
            clock: true,
        },
        &[1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233],
    );
}

/// 索引の削除・外のアプリが作るファイル・ロックの中の時間の上限を混ぜる（開けないフォルダは混ぜない。混ぜたときの
/// 既知の振る舞いは、ここでは確かめない）。別の走査が索引を作り直しても、外のアプリが作ったファイルを削除と記録
/// しない（種 62・102・124 が、直す前に落ちた）。
#[test]
fn random_operations_with_index_drops_do_not_misreport_external_files() {
    random_runs(
        Mode {
            holds: false,
            drops: true,
            ext: true,
            clock: true,
        },
        &[62, 102, 124, 1, 2, 3, 5, 8, 13, 21, 34, 55],
    );
}

/// 別の走査が、こちらが歩くあいだに索引を作り直す。履歴の最後の行がもう index_rebuilt のときも、索引を置き換える
/// 回は index_rebuilt を書く（書かないと、こちらは索引の変わりを履歴の行で合わせられず、外のアプリが作ったファイルを
/// 外での削除と記録し、次の走査で作成と記録していた）。
#[test]
fn another_scanner_rebuilding_the_index_after_an_index_rebuilt_line_is_not_silent() {
    let dir = TestDir::new("b0-silent-rebuild");
    let (path, vault, _) = vault_with_folder(&dir);
    fs::create_dir(path.join("x")).unwrap();
    fs::write(path.join("x").join("n.txt"), "n").unwrap();
    let (_, base) = scan(&vault, 0);
    // 履歴の最後の行を index_rebuilt にする（ほかの Vault の走査で作り直す）
    fs::remove_file(path.join(".vault").join("index.json")).unwrap();
    let other = |p: &Path| {
        let mut scanner = Vault::open(p, false, &env()).unwrap();
        scanner.set_min_rescan_interval(Duration::ZERO);
        scanner.set_rescan_interval_factor(0);
        call(&scanner, "vault_changes", json!({})).unwrap();
    };
    other(&path);
    assert_eq!(journal(&path).last().unwrap().op, Op::IndexRebuilt);
    let before = external(&path).len();
    let p = path.clone();
    after_next_walk(move || {
        fs::write(p.join("x").join("new.txt"), "e").unwrap();
        fs::remove_file(p.join(".vault").join("index.json")).unwrap();
        other(&p);
    });
    let mut seq = next_seq(&base);
    for _ in 0..3 {
        let (_, result) = scan(&vault, seq);
        seq = next_seq(&result);
    }
    assert_eq!(
        external(&path)[before..],
        [],
        "作り直した索引に入ったファイルを、外での削除・作成と記録した"
    );
    assert!(index_keys(&path).contains(&"x/new.txt".to_owned()));
}

// ---------------------------------------------------------------------------
// 点検での直し（ロックを持つ時間・索引がないときの印・親の大文字小文字）
// ---------------------------------------------------------------------------

thread_local! {
    /// 走査が最後にロックを取った時刻（locked_time）。
    static LOCKED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// `f` を呼ぶあいだに、走査がロックを持っていた時間（ロックを取った直後＝BeforeReconcile から、呼び出しが返るまで。
/// ロックを外して取り直したときは、最後に取ってから）。差し込み口は、ロックを取り直した回の分も多めに積む（残った
/// ものが後の走査で呼ばれても、時刻を書くだけ）。
fn locked_time(f: impl FnOnce() -> Value) -> (Value, Duration) {
    LOCKED_AT.with(|at| at.set(None));
    for _ in 0..4 {
        on_next(Step::BeforeReconcile, || {
            LOCKED_AT.with(|at| at.set(Some(Instant::now())));
        });
    }
    let result = f();
    let held = LOCKED_AT
        .with(Cell::get)
        .expect("ロックを取っていない")
        .elapsed();
    (result, held)
}

/// 歩くあいだにコアが d の下へ `count` 件書き、歩き終えたあとでほかのアプリが d を DELETE の権利で持つ（調べ直す
/// パスの親を開けない）。ロックの中では試し直さず、親を開けなければ下のパスはまとめて合わせ直せないとする
/// 返り値は (保管庫, 走査の返り値, ロックを持っていた時間)。
fn failed_parent_scenario(name: &str, count: usize) -> (PathBuf, Value, Duration) {
    let dir = TestDir::new(name);
    let (path, vault, seq) = vault_with_folder(&dir);
    let p = path.clone();
    on_next(Step::WalkHolding, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        for i in 0..count {
            call(
                &other,
                "vault_write",
                json!({"path": format!("d/w{i}.txt"), "content": "w"}),
            )
            .unwrap();
        }
    });
    let holder: Rc<RefCell<Option<fs::File>>> = Rc::new(RefCell::new(None));
    let out = holder.clone();
    let d = path.join("d");
    after_next_walk(move || *out.borrow_mut() = Some(hold_with_delete(&d)));
    let (result, held) = locked_time(|| scan(&vault, seq).1);
    drop(holder.borrow_mut().take());
    (path, result, held)
}

/// 調べ直せないパスが 60 件ある場合。
#[test]
fn failed_paths_are_not_retried_inside_the_lock() {
    let (_, result, held) = failed_parent_scenario("b0-lock-failed-60", 60);
    assert_eq!(result["scanned"], false, "開けないパスがあるので捨てる");
    assert!(
        held < Duration::from_secs(2),
        "ロックを長く持った：{held:?}"
    );
}

/// 調べ直せないパスが 256 件ある場合（ロックの中で調べ直す上限いっぱい）。
#[test]
fn failed_paths_up_to_the_limit_do_not_hold_the_lock_for_long() {
    let (_, result, held) = failed_parent_scenario("b0-lock-failed-256", MAX_LOCKED_RECHECKS);
    assert_eq!(result["scanned"], false);
    assert!(
        held < Duration::from_secs(2),
        "ロックを長く持った：{held:?}"
    );
}

/// 走査がロックを持つ
/// あいだに、ほかの利用者が書いても LOCKED にならない（既定の待ちの中で書け、長く待たない）。
#[test]
fn a_writer_is_not_locked_out_while_the_scan_reconciles() {
    type Writer = std::thread::JoinHandle<(Duration, Result<(), String>)>;
    let dir = TestDir::new("b0-writer-not-locked");
    let (path, vault, seq) = vault_with_folder(&dir);
    let p = path.clone();
    on_next(Step::WalkHolding, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        for i in 0..80 {
            call(
                &other,
                "vault_write",
                json!({"path": format!("d/w{i}.txt"), "content": "w"}),
            )
            .unwrap();
        }
    });
    let holder: Rc<RefCell<Option<fs::File>>> = Rc::new(RefCell::new(None));
    let out = holder.clone();
    let d = path.join("d");
    after_next_walk(move || *out.borrow_mut() = Some(hold_with_delete(&d)));
    let writer: Rc<RefCell<Option<Writer>>> = Rc::new(RefCell::new(None));
    let w = writer.clone();
    let p = path.clone();
    on_next(Step::BeforeReconcile, move || {
        let p = p.clone();
        *w.borrow_mut() = Some(std::thread::spawn(move || {
            let user = Vault::open(&p, false, &env()).unwrap();
            let start = Instant::now();
            let result = call(
                &user,
                "vault_write",
                json!({"path": "user.txt", "content": "u"}),
            )
            .map(|_| ())
            .map_err(|e| e.to_string());
            (start.elapsed(), result)
        }));
    });
    scan(&vault, seq);
    drop(holder.borrow_mut().take());
    let (waited, result) = writer.borrow_mut().take().unwrap().join().unwrap();
    assert!(
        result.is_ok(),
        "利用者の書き込みが失敗した：{result:?}（{waited:?} 待った）"
    );
    assert!(
        waited < Duration::from_secs(3),
        "利用者の書き込みが長く待った：{waited:?}"
    );
}

/// 大きさ0・同じ更新時刻のファイルを `n` 個、外で1つずつ名前を変え、ロックの
/// 直前にコアが1件書く。名前の変更の組を探す処理が、候補の数の2乗にならない（ロックを持つ時間が短い）。
/// 外での変更は、名前の変更として記録する。
#[test]
fn many_identical_renames_do_not_hold_the_lock_for_long() {
    for n in [5000_usize, 10000] {
        let dir = TestDir::new(&format!("b0-renames-{n}"));
        let (path, vault, _) = vault_with_folder(&dir);
        let r = path.join("r");
        fs::create_dir(&r).unwrap();
        let when = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for i in 0..n {
            let f = fs::File::create(r.join(format!("f{i:05}.txt"))).unwrap();
            f.set_modified(when).unwrap();
        }
        let (_, base) = scan(&vault, 0);
        let before = external(&path).len();
        for i in 0..n {
            fs::rename(
                r.join(format!("f{i:05}.txt")),
                r.join(format!("g{i:05}.txt")),
            )
            .unwrap();
        }
        let p = path.clone();
        on_next(Step::BeforeScanLock, move || {
            core_write(&p, "core.txt", "c")
        });
        let (result, held) = locked_time(|| scan(&vault, next_seq(&base)).1);
        eprintln!("同じ値の名前の変更 {n} 件：ロックを持った時間 {held:?}");
        assert_eq!(result["scanned"], true, "{n} 件");
        assert!(
            held < Duration::from_secs(3),
            "{n} 件：ロックを長く持った：{held:?}"
        );
        let renames = external(&path)[before..]
            .iter()
            .filter(|(op, _)| *op == Op::ExternalRename)
            .count();
        assert_eq!(renames, n, "{n} 件：名前の変更として記録していない");
    }
}

/// 索引がない状態で、ロックの直前にコアがフォルダを移す（ロックの中では、そのフォルダの下を歩き直さずに印を付ける
/// が、比べる索引がない）：次の走査で、コアが移したフォルダの中を外での作成と記録しない。
#[test]
fn a_folder_marked_inside_the_lock_while_the_index_is_missing() {
    let dir = TestDir::new("b0-mark-noindex");
    let (path, vault, _) = vault_with_folder(&dir);
    fs::create_dir_all(path.join("big").join("s")).unwrap();
    for i in 0..5 {
        fs::write(path.join("big").join("s").join(format!("f{i}.txt")), "x").unwrap();
    }
    let (_, base) = scan(&vault, 0);
    fs::remove_file(path.join(".vault").join("index.json")).unwrap();
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::BeforeScanLock, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        call(&other, "vault_move", json!({"from": "big", "to": "big2"})).unwrap();
    });
    let mut seq = next_seq(&base);
    for _ in 0..3 {
        let (_, result) = scan(&vault, seq);
        seq = next_seq(&result);
    }
    assert_eq!(
        external(&path)[before..],
        [],
        "コアが移したフォルダの中を、外での作成と記録した"
    );
    assert!(index_keys(&path).contains(&"big2/s/f4.txt".to_owned()));
}

/// 歩いたあとで、外のアプリが親の大文字小文字を変える（a→A）。コアは歩くあいだに a/y.txt を書いた。
/// 外での変更は親の名前の変更だけ（歩いた結果の親の大文字小文字が古いときは合わせない）。
#[test]
fn a_parent_case_changed_after_the_walk_visited_it() {
    let dir = TestDir::new("b0-case-parent-after");
    let (path, vault, _) = vault_with_folder(&dir);
    fs::create_dir(path.join("a")).unwrap();
    fs::write(path.join("a").join("x.txt"), "x").unwrap();
    let (_, base) = scan(&vault, 0);
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::WalkHolding, move || core_write(&p, "a/y.txt", "y"));
    let p = path.clone();
    after_next_walk(move || fs::rename(p.join("a"), p.join("A")).unwrap());
    let mut seq = next_seq(&base);
    for _ in 0..4 {
        let (_, result) = scan(&vault, seq);
        seq = next_seq(&result);
    }
    assert_eq!(
        external(&path)[before..],
        [(Op::ExternalRename, "a".to_owned())],
        "外での変更は親の名前の変更だけ"
    );
    assert_no_case_twins(&path);
}

/// ロックの中で調べ直すあいだに、ロックの中の時間の上限（`MAX_LOCKED_SCAN_TIME`）を超えたら、ロックを外して、
/// ロックの外で合わせ直す（1回捨てた回に数える。完了条件1）。時間は、走査の時計を差し込み口で進めて超えさせる。
/// 外での変更とは記録せず、その回のうちに終える。
#[test]
fn the_lock_is_released_when_rechecks_take_too_long() {
    let dir = TestDir::new("b0-lock-time-recheck");
    let (path, vault, seq) = vault_with_folder(&dir);
    fs::write(path.join("ext.txt"), "e").unwrap();
    let before = external(&path).len();
    let p = path.clone();
    on_next(Step::BeforeScanLock, move || {
        core_write(&p, "core.txt", "c")
    });
    // 最初にロックを取った直後に、走査の時計を上限より進める（調べ直しの最初で、上限を超えている）
    on_next(Step::BeforeReconcile, || {
        advance_scan_clock(MAX_LOCKED_SCAN_TIME * 2);
    });
    let locks = Rc::new(Cell::new(0));
    for _ in 0..3 {
        let l = locks.clone();
        on_next(Step::BeforeScanLock, move || l.set(l.get() + 1));
    }
    let (_, result) = scan(&vault, seq);
    assert_eq!(result["scanned"], true);
    assert_eq!(locks.get(), 1, "ロックを外して、取り直していない");
    assert_eq!(
        external(&path)[before..],
        [(Op::ExternalCreate, "ext.txt".to_owned())]
    );
    assert!(index_keys(&path).contains(&"core.txt".to_owned()));
}

/// 比べ直し（ロックの中で読み直した索引との比べ）までの合計が、ロックの中の時間の上限を超えたときも、書かずに
/// ロックを外して、ロックの外で用意し直す（完了条件1）。ロックの直前にほかのアプリが索引を消す（調べ直すパスは
/// ないが、索引が変わったので、ロックの中で比べ直す）。
#[test]
fn the_lock_is_released_when_comparing_takes_too_long() {
    let dir = TestDir::new("b0-lock-time-compare");
    let (path, vault, seq) = vault_with_folder(&dir);
    let p = path.clone();
    on_next(Step::BeforeScanLock, move || {
        fs::remove_file(p.join(".vault").join("index.json")).unwrap();
    });
    on_next(Step::BeforeReconcile, || {
        advance_scan_clock(MAX_LOCKED_SCAN_TIME * 2);
    });
    let locks = Rc::new(Cell::new(0));
    for _ in 0..3 {
        let l = locks.clone();
        on_next(Step::BeforeScanLock, move || l.set(l.get() + 1));
    }
    let (changes, result) = scan(&vault, seq);
    assert_eq!(result["scanned"], true);
    assert_eq!(locks.get(), 1, "ロックを外して、取り直していない");
    assert_eq!(
        changes,
        [(Op::IndexRebuilt, String::new())],
        "索引を作り直した1行だけ"
    );
    assert!(index_keys(&path).contains(&"d/x.txt".to_owned()));
}

/// 続けて捨てる上限に当たっている回は、ロックの中の時間の上限を超えても外へ戻らず、残りのパスに印を付けて終える
/// （今の索引の値のまま。外での変更とは記録しない）。
#[test]
fn at_the_discard_limit_paths_left_by_the_time_limit_are_marked() {
    let dir = TestDir::new("b0-lock-time-limit");
    let (path, vault, seq) = vault_with_folder(&dir);
    let before = external(&path).len();
    // 2回続けて捨てさせる（歩くあいだにコアが書いた d の下を、歩いたあとで開けなくする）
    let mut seq = seq;
    for attempt in 0..2 {
        let p = path.clone();
        on_next(Step::WalkHolding, move || {
            core_write(&p, &format!("d/w{attempt}.txt"), "w")
        });
        let holder: Rc<RefCell<Option<fs::File>>> = Rc::new(RefCell::new(None));
        let out = holder.clone();
        let d = path.join("d");
        after_next_walk(move || *out.borrow_mut() = Some(hold_with_delete(&d)));
        let (_, result) = scan(&vault, seq);
        drop(holder.borrow_mut().take());
        assert_eq!(result["scanned"], false, "{attempt} 回目は捨てる");
        seq = next_seq(&result);
    }
    // 3回目：ロックの直前にコアが書き、ロックを取った直後に時間の上限を超えさせる
    let p = path.clone();
    on_next(Step::BeforeScanLock, move || {
        core_write(&p, "late.txt", "c")
    });
    on_next(Step::BeforeReconcile, || {
        advance_scan_clock(MAX_LOCKED_SCAN_TIME * 2);
    });
    let (_, result) = scan(&vault, seq);
    assert_eq!(result["scanned"], true, "上限の回は終える");
    assert_eq!(
        scan_rechecks(&vault),
        ["late.txt"],
        "調べ直さなかったパスに印がない"
    );
    assert_eq!(external(&path)[before..], [], "外での変更と記録した");
    assert!(index_keys(&path).contains(&"late.txt".to_owned()));
    let (_, next) = scan(&vault, next_seq(&result));
    assert_eq!(next["scanned"], true);
    assert_eq!(external(&path)[before..], [], "次の走査で外での変更が出た");
    assert!(scan_rechecks(&vault).is_empty());
}

/// 開けない途中のフォルダが多くても（12 個。それぞれの下にコアが書いた）、ロックの中では1つずつ試し直さない
/// （試し直すと、1つにつき約 0.2 秒止まる）。ロックを持つ時間は短い（完了条件1）。
#[test]
fn many_failed_parents_are_not_retried_inside_the_lock() {
    let dir = TestDir::new("b0-lock-failed-parents");
    let (path, vault, seq) = vault_with_folder(&dir);
    let folders: Vec<String> = (0..12).map(|i| format!("p{i:02}")).collect();
    for folder in &folders {
        fs::create_dir(path.join(folder)).unwrap();
    }
    let (_, base) = scan(&vault, seq);
    let p = path.clone();
    let names = folders.clone();
    on_next(Step::WalkHolding, move || {
        let other = Vault::open(&p, false, &env()).unwrap();
        for folder in &names {
            call(
                &other,
                "vault_write",
                json!({"path": format!("{folder}/w.txt"), "content": "w"}),
            )
            .unwrap();
        }
    });
    let holder: Rc<RefCell<Vec<fs::File>>> = Rc::new(RefCell::new(Vec::new()));
    let out = holder.clone();
    let p = path.clone();
    after_next_walk(move || {
        for folder in &folders {
            out.borrow_mut().push(hold_with_delete(&p.join(folder)));
        }
    });
    let (result, held) = locked_time(|| scan(&vault, next_seq(&base)).1);
    holder.borrow_mut().clear();
    eprintln!("開けない親 12 個：ロックを持った時間 {held:?}");
    assert_eq!(result["scanned"], false, "開けないパスがあるので捨てる");
    assert!(
        held < Duration::from_millis(500),
        "ロックを長く持った（試し直した）：{held:?}"
    );
}
