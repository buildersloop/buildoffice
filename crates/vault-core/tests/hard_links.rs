//! ハードリンク。
//!
//! 外とハードリンクでつながった中のファイル・管理用のファイルに、どの書く道で触れても、外のファイルのバイトと
//! 更新時刻は変わらない・外は消えない。書く道はいつも「一時ファイル → 置き換えない rename」か「名前を動かすだけ」で、
//! その場で中身を書くのは履歴の追記だけ（最後の履歴のファイルがつながっていれば、新しい履歴のファイルに切り替える）。

#![cfg(windows)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use common::{TestDir, call, env, expect_code, journal, new_vault};
use serde_json::{Value, json};
use vault_core::test_support::{
    FakeLinks, Place, Rename, Step, fake_journal_links, link_marks, on_next, set_sweep_slot,
};
use vault_core::{ErrorCode, Op, Vault};

/// 外のファイルの今の中身と更新時刻。
#[derive(Debug, PartialEq, Eq)]
struct Snap {
    bytes: Vec<u8>,
    modified: SystemTime,
}

fn snap(path: &Path) -> Snap {
    Snap {
        bytes: fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        modified: fs::metadata(path).unwrap().modified().unwrap(),
    }
}

/// 外のファイルのバイトと更新時刻が、`before` から変わっていない（消えてもいない）。
fn assert_unchanged(path: &Path, before: &Snap, what: &str) {
    assert!(path.is_file(), "{what}：外のファイルが消えた");
    assert_eq!(&snap(path), before, "{what}：外のファイルが変わった");
}

fn text(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} がない：{v}"))
        .to_owned()
}

/// 履歴のファイル（`<20桁>.jsonl` の形のものだけ。名前の順）。
fn journal_files(vault_path: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(vault_path.join(".vault").join("journal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            name.len() == 26
                && name.ends_with(".jsonl")
                && name[..20].bytes().all(|b| b.is_ascii_digit())
        })
        .collect();
    files.sort();
    files
}

fn journal_name(seq: u64) -> String {
    format!("{seq:020}.jsonl")
}

/// 履歴の seq が 1 から1つずつ続いている。
fn assert_contiguous(vault_path: &Path) -> u64 {
    let seqs: Vec<u64> = journal(vault_path).iter().map(|e| e.seq).collect();
    let n = seqs.len() as u64;
    assert_eq!(seqs, (1..=n).collect::<Vec<_>>(), "seq が続いていない");
    n
}

/// 外の名前から書いて、中の名前で読めれば、つながったまま。
fn assert_still_linked(outside: &Path, inner: &Path) {
    fs::write(outside, b"written through the outside name").unwrap();
    assert_eq!(
        fs::read(inner).unwrap(),
        b"written through the outside name",
        "中の名前が外とつながっていない"
    );
}

/// 外のファイル（`dir` の下、保管庫の外）を作り、中の `inner`（保管庫の相対の名前）とハードリンクにする。
fn linked(
    dir: &TestDir,
    vault_path: &Path,
    outside_name: &str,
    inner: &str,
    bytes: &[u8],
) -> PathBuf {
    let outside = dir.join(outside_name);
    fs::write(&outside, bytes).unwrap();
    fs::hard_link(&outside, vault_path.join(inner)).unwrap();
    outside
}

// ---------------------------------------------------------------------------
// 利用者のファイルの道（どれも名前を動かすだけ・一時ファイル → rename。そのままでよいことを固定する）
// ---------------------------------------------------------------------------

/// 外とつながった中のファイルを、版つき（expected_version）で上書きしても、外は変わらない。古い名前はゴミ箱へ
/// 移り、外とつながったまま。
#[test]
fn h1_writing_over_a_linked_file_keeps_the_outside_file() {
    let dir = TestDir::new("hl-h1");
    let (path, vault) = new_vault(&dir, "v");
    let outside = linked(&dir, &path, "outside.txt", "inner.txt", b"outside");
    let before = snap(&outside);

    let version = text(
        &call(&vault, "vault_read", json!({"path": "inner.txt"})).unwrap(),
        "version",
    );
    let w = call(
        &vault,
        "vault_write",
        json!({"path": "inner.txt", "content": "new", "expected_version": version}),
    )
    .unwrap();
    assert_unchanged(&outside, &before, "版つきの上書き");
    assert_eq!(fs::read_to_string(path.join("inner.txt")).unwrap(), "new");
    let data = path
        .join(".vault")
        .join("trash")
        .join(text(&w, "previous_trash_id"))
        .join("data");
    assert_eq!(fs::read_to_string(&data).unwrap(), "outside");
    assert_still_linked(&outside, &data);
}

/// vault_delete しても外は変わらない。restore で戻すと、外とつながったまま戻る。
#[test]
fn h2_deleting_and_restoring_a_linked_file_keeps_the_outside_file() {
    let dir = TestDir::new("hl-h2");
    let (path, vault) = new_vault(&dir, "v");
    let outside = linked(&dir, &path, "outside.txt", "inner.txt", b"outside");
    let before = snap(&outside);

    let d = call(&vault, "vault_delete", json!({"path": "inner.txt"})).unwrap();
    assert!(!path.join("inner.txt").exists());
    assert_unchanged(&outside, &before, "削除");
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": text(&d, "trash_id")}),
    )
    .unwrap();
    assert_unchanged(&outside, &before, "戻す");
    assert_eq!(
        fs::read_to_string(path.join("inner.txt")).unwrap(),
        "outside"
    );
    assert_still_linked(&outside, &path.join("inner.txt"));
}

/// vault_move しても外は変わらない（移した先も外とつながったまま）。
#[test]
fn h3_moving_a_linked_file_keeps_the_outside_file() {
    let dir = TestDir::new("hl-h3");
    let (path, vault) = new_vault(&dir, "v");
    let outside = linked(&dir, &path, "outside.txt", "inner.txt", b"outside");
    let before = snap(&outside);

    call(&vault, "vault_mkdir", json!({"path": "sub"})).unwrap();
    call(
        &vault,
        "vault_move",
        json!({"from": "inner.txt", "to": "sub/moved.txt"}),
    )
    .unwrap();
    assert_unchanged(&outside, &before, "移動");
    assert!(!path.join("inner.txt").exists());
    assert_still_linked(&outside, &path.join("sub").join("moved.txt"));
}

/// restore の replace:true で、戻す先が外とつながっている → 今あるもの（つながったもの）はゴミ箱へ移り、外は
/// 変わらない。
#[test]
fn h4_restoring_over_a_linked_file_keeps_the_outside_file() {
    let dir = TestDir::new("hl-h4");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "aaa"}),
    )
    .unwrap();
    let d = call(&vault, "vault_delete", json!({"path": "a.txt"})).unwrap();
    let outside = linked(&dir, &path, "outside.txt", "a.txt", b"outside");
    let before = snap(&outside);

    let r = call(
        &vault,
        "vault_restore",
        json!({"trash_id": text(&d, "trash_id"), "replace": true}),
    )
    .unwrap();
    assert_unchanged(&outside, &before, "replace で戻す");
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "aaa");
    let data = path
        .join(".vault")
        .join("trash")
        .join(text(&r, "replaced_trash_id"))
        .join("data");
    assert_still_linked(&outside, &data);
}

/// 上書きの最後の rename がぶつかり、古い中身を取り戻す（take_back）道でも外は変わらない。戻した中の名前は
/// 外とつながったまま。
#[test]
fn h5_taking_back_a_linked_file_keeps_the_outside_file() {
    let dir = TestDir::new("hl-h5");
    let (path, vault) = new_vault(&dir, "v");
    let outside = linked(&dir, &path, "outside.txt", "inner.txt", b"outside");
    let before = snap(&outside);
    let target = path.join("inner.txt");
    let again = target.clone();
    on_next(Step::BeforeRename(Rename::WriteFinal), move || {
        fs::write(&target, "OTHER").unwrap();
    });
    on_next(Step::BeforeRename(Rename::TakeBack), move || {
        fs::remove_file(&again).unwrap();
    });
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "inner.txt", "content": "new"}),
        ),
        ErrorCode::Conflict,
        "ぶつかったあと、取り戻せた",
    );
    assert_unchanged(&outside, &before, "取り戻す");
    assert_eq!(
        fs::read_to_string(path.join("inner.txt")).unwrap(),
        "outside"
    );
    assert_still_linked(&outside, &path.join("inner.txt"));
}

// ---------------------------------------------------------------------------
// 管理用の場所に置かれたハードリンク
// ---------------------------------------------------------------------------

/// .vault/tmp に、外のファイルのハードリンクを一時ファイルの形の名前で置いて開き直す → orphan_tmp として
/// ゴミ箱へ移り、外は消えず変わらない。to を付けて戻せる。
#[test]
fn h6_a_linked_orphan_tmp_goes_to_the_trash_and_the_outside_file_stays() {
    let dir = TestDir::new("hl-h6");
    let (path, vault) = new_vault(&dir, "v");
    drop(vault);
    let outside = linked(
        &dir,
        &path,
        "outside.txt",
        ".vault/tmp/999-0.tmp",
        b"outside",
    );
    let before = snap(&outside);

    let vault = Vault::open(&path, false, &env()).unwrap();
    assert_unchanged(&outside, &before, "開き直す（orphan_tmp の片づけ）");
    assert!(!path.join(".vault").join("tmp").join("999-0.tmp").exists());
    let list = call(&vault, "vault_trash_list", json!({})).unwrap();
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["reason"] == "orphan_tmp")
        .unwrap_or_else(|| panic!("orphan_tmp がない：{list}"))
        .clone();
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": text(&item, "trash_id"), "to": "from-tmp.txt"}),
    )
    .unwrap();
    assert_unchanged(&outside, &before, "to を付けて戻す");
    assert_still_linked(&outside, &path.join("from-tmp.txt"));
}

/// ゴミ箱の data を外のファイルのハードリンクに替えて restore → 戻る。外は書き換わらない。
#[test]
fn h7_restoring_a_linked_trash_data_keeps_the_outside_file() {
    let dir = TestDir::new("hl-h7");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "original"}),
    )
    .unwrap();
    let d = call(&vault, "vault_delete", json!({"path": "a.txt"})).unwrap();
    let id = text(&d, "trash_id");
    let data = path.join(".vault").join("trash").join(&id).join("data");
    fs::remove_file(&data).unwrap();
    let outside = dir.join("outside.txt");
    fs::write(&outside, b"outside!").unwrap();
    fs::hard_link(&outside, &data).unwrap();
    let before = snap(&outside);

    call(&vault, "vault_restore", json!({"trash_id": id})).unwrap();
    assert_unchanged(&outside, &before, "つながった data を戻す");
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "outside!");
}

// ---------------------------------------------------------------------------
// 履歴（その場で中身を書く唯一の道）
// ---------------------------------------------------------------------------

/// 最後の履歴のファイルを外の写しとハードリンクにしてから書く → 写しは変わらない。新しい履歴のファイルが
/// でき、seq は続く。次の書き込みも新しいファイルへ。
#[test]
fn h8a_a_linked_last_journal_file_is_not_appended_to() {
    let dir = TestDir::new("hl-h8a");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let last = journal_files(&path).pop().unwrap();
    let copy = dir.join("journal-copy.jsonl");
    fs::hard_link(&last, &copy).unwrap();
    let before = snap(&copy);
    let n = assert_contiguous(&path);

    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    )
    .unwrap();
    assert_unchanged(&copy, &before, "最後の履歴のファイルの写し");
    let files = journal_files(&path);
    assert_eq!(files.len(), 2, "新しい履歴のファイルができる");
    assert_eq!(
        files[1].file_name().unwrap().to_string_lossy(),
        journal_name(n + 1)
    );
    assert_eq!(assert_contiguous(&path), n + 1);
    let last = journal(&path).pop().unwrap();
    assert_eq!((last.op, last.path.as_deref()), (Op::Write, Some("b.txt")));

    call(
        &vault,
        "vault_write",
        json!({"path": "c.txt", "content": "c"}),
    )
    .unwrap();
    assert_unchanged(&copy, &before, "次の書き込み");
    assert_eq!(journal_files(&path).len(), 2, "新しいファイルに続けて書く");
    assert_eq!(assert_contiguous(&path), n + 2);
}

/// 履歴を開く直前に、最後の履歴の名前を .bak に変え、同じ名前で外の被害のファイルへのハードリンクを置く
/// → 被害のファイルは変わらない（seq が続くことは h8a のテストで確かめる）。
#[test]
fn h8b_a_journal_name_swapped_for_a_link_to_a_victim_is_not_appended_to() {
    let dir = TestDir::new("hl-h8b");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let victim = dir.join("victim.txt");
    fs::write(&victim, b"victim data\n").unwrap();
    let before = snap(&victim);
    let last = journal_files(&path).pop().unwrap();
    let (from, link) = (last.clone(), victim.clone());
    on_next(Step::Open(Place::Journal), move || {
        let mut bak = from.clone().into_os_string();
        bak.push(".bak");
        fs::rename(&from, bak).unwrap();
        fs::hard_link(&link, &from).unwrap();
    });

    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    )
    .unwrap();
    assert_unchanged(&victim, &before, "被害のファイル");
}

/// 履歴のファイルのリンクの数を読めなければ、書かない（開くときなら何も変えずに IO_ERROR）。
#[test]
fn h8d_an_unreadable_link_count_stops_the_tool_before_any_change() {
    let dir = TestDir::new("hl-h8d");
    let (path, vault) = new_vault(&dir, "v");
    let before = journal(&path).len();
    let files = journal_files(&path);
    fake_journal_links(FakeLinks::Fail);
    expect_code(
        call(
            &vault,
            "vault_write",
            json!({"path": "a.txt", "content": "a"}),
        ),
        ErrorCode::IoError,
        "リンクの数を読めない",
    );
    assert!(!path.join("a.txt").exists(), "何も変えない");
    assert_eq!(journal(&path).len(), before, "履歴に書かない");
    assert_eq!(journal_files(&path), files, "履歴のファイルも増えない");
}

/// H8d2：書く直前にリンクの数を読めなければ、履歴に書かない（変更は済んでいる、と文に添える）。
#[test]
fn h8d2_an_unreadable_link_count_at_commit_writes_nothing_to_the_journal() {
    let dir = TestDir::new("hl-h8d2");
    let (path, vault) = new_vault(&dir, "v");
    let last = journal_files(&path).pop().unwrap();
    let bytes = fs::read(&last).unwrap();
    fake_journal_links(FakeLinks::Count(1)); // 開くとき
    fake_journal_links(FakeLinks::Fail); // 書く直前
    let err = call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoError, "{err}");
    assert!(err.message().contains("履歴には残せなかった"), "{err}");
    assert_eq!(fs::read(&last).unwrap(), bytes, "履歴のファイルに書かない");
    assert_eq!(journal_files(&path), [last], "履歴のファイルも増えない");
}

/// 履歴を開いたあと、書く前に最後の履歴のファイルへハードリンクを足される → 書く直前に読み直して、
/// 新しい履歴のファイルに切り替える。写しは変わらず、seq は続く。
#[test]
fn h8e_a_link_added_during_the_tool_switches_the_journal_file_at_commit() {
    let dir = TestDir::new("hl-h8e");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let n = assert_contiguous(&path);
    let last = journal_files(&path).pop().unwrap();
    // 写しは、ツールの途中で作る（同じファイルなので、今の最後の履歴のファイルの値と比べる）
    let before = snap(&last);
    let copy = dir.join("journal-copy.jsonl");
    let (from, to) = (last.clone(), copy.clone());
    on_next(Step::AfterTmpCreated, move || {
        fs::hard_link(&from, &to).unwrap();
    });

    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    )
    .unwrap();
    assert_unchanged(
        &copy,
        &before,
        "ツールの途中でつながった履歴のファイルの写し",
    );
    let files = journal_files(&path);
    assert_eq!(files.len(), 2, "書く直前に新しい履歴のファイルへ切り替える");
    assert_eq!(
        files[1].file_name().unwrap().to_string_lossy(),
        journal_name(n + 1)
    );
    assert_eq!(assert_contiguous(&path), n + 1);
    let last = journal(&path).pop().unwrap();
    assert_eq!((last.op, last.path.as_deref()), (Op::Write, Some("b.txt")));
}

/// 読める行のない最後の履歴のファイルが、書く前につながった → そのファイルの名前と
/// ぶつからないよう、8 MiB のときと同じく最初の seq より1つ進めた seq から付け直して、新しいファイルに書く。
#[test]
fn h8f_switching_at_commit_does_not_reuse_the_name_of_an_empty_last_file() {
    let dir = TestDir::new("hl-h8f");
    let (path, vault) = new_vault(&dir, "v");
    let empty = path.join(".vault").join("journal").join(journal_name(100));
    fs::write(&empty, b"").unwrap();
    let copy = dir.join("journal-copy.jsonl");
    let (from, to) = (empty.clone(), copy.clone());
    on_next(Step::AfterTmpCreated, move || {
        fs::hard_link(&from, &to).unwrap();
    });

    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    assert_eq!(fs::read(&copy).unwrap(), b"", "写しは空のまま");
    let names: Vec<String> = journal_files(&path)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.last().unwrap(), &journal_name(101));
    let last = journal(&path).pop().unwrap();
    assert_eq!((last.seq, last.op), (101, Op::Write));
    // 記録：seq 100 は使われずに1つ飛ぶ（同じときの走査は、行が続かないので1回歩き直す。受け入れる）
    assert!(
        journal(&path).iter().all(|e| e.seq != 100),
        "seq 100 が使われた"
    );
}

/// h8g のテストの形で、切り替える先の名前を 8 個とも先に置かれる → どれも上書きせず、IO_ERROR（変更は済んでいる、と
/// 文に添える）。置かれたファイルはどれも変わらない。
#[test]
fn h8h_all_switch_target_names_taken_is_an_io_error_without_overwriting() {
    let dir = TestDir::new("hl-h8h");
    let (path, vault) = new_vault(&dir, "v");
    let n = assert_contiguous(&path);
    let last = journal_files(&path).pop().unwrap();
    let before = snap(&last);
    let copy = dir.join("journal-copy.jsonl");
    let mut victims = Vec::new();
    for k in 1..=8u64 {
        let victim = dir.join(format!("victim-{k}.txt"));
        fs::write(&victim, format!("victim {k}\n")).unwrap();
        let name = path
            .join(".vault")
            .join("journal")
            .join(journal_name(n + k));
        victims.push((victim.clone(), name, snap(&victim)));
    }
    let placed: Vec<(PathBuf, PathBuf)> = victims
        .iter()
        .map(|(v, name, _)| (v.clone(), name.clone()))
        .collect();
    let (from, to) = (last.clone(), copy.clone());
    on_next(Step::AfterTmpCreated, move || {
        fs::hard_link(&from, &to).unwrap();
        for (victim, name) in &placed {
            fs::hard_link(victim, name).unwrap();
        }
    });

    let err = call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoError, "{err}");
    assert!(err.message().contains("履歴には残せなかった"), "{err}");
    assert_unchanged(&copy, &before, "履歴のファイルの写し");
    for (victim, name, snapped) in &victims {
        assert_unchanged(victim, snapped, "被害のファイル");
        assert_unchanged(name, snapped, "置かれた名前");
    }
}

/// 書く直前に最後の履歴のファイルへリンクを足され、さらに切り替える先の名前（ためた行の最初の seq）に、外の
/// 被害のファイルへのハードリンクを先に置かれる → その名前は上書きせず、seq を進めた名前で新しい履歴のファイルを
/// 作って書く。被害のファイルも写しも変わらず、操作は履歴に残る（seq は進めた番号）。
#[test]
fn h8g_a_switch_target_name_taken_in_advance_is_skipped() {
    let dir = TestDir::new("hl-h8g");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let n = assert_contiguous(&path);
    let last = journal_files(&path).pop().unwrap();
    let before = snap(&last);
    let copy = dir.join("journal-copy.jsonl");
    let victim = dir.join("victim.txt");
    fs::write(&victim, b"victim data\n").unwrap();
    let victim_before = snap(&victim);
    let taken = path
        .join(".vault")
        .join("journal")
        .join(journal_name(n + 1));
    let (from, to, target, name) = (last.clone(), copy.clone(), victim.clone(), taken.clone());
    on_next(Step::AfterTmpCreated, move || {
        fs::hard_link(&from, &to).unwrap();
        fs::hard_link(&target, &name).unwrap();
    });

    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    )
    .unwrap();
    assert_unchanged(
        &copy,
        &before,
        "ツールの途中でつながった履歴のファイルの写し",
    );
    assert_unchanged(
        &victim,
        &victim_before,
        "切り替える先の名前に置かれた被害のファイル",
    );
    assert_unchanged(
        &taken,
        &victim_before,
        "切り替える先の名前（被害のファイルとつながったまま）",
    );
    assert_eq!(
        journal_files(&path)
            .last()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy(),
        journal_name(n + 2)
    );
    let last = journal(&path).pop().unwrap();
    assert_eq!(
        (last.seq, last.op, last.path.as_deref()),
        (n + 2, Op::Write, Some("b.txt")),
        "操作は履歴に残る（seq は進めた番号）"
    );
}

// ---------------------------------------------------------------------------
// lock
// ---------------------------------------------------------------------------

/// .vault/lock を外の写しとハードリンクにしても、書くツールは成功し、写しは空のまま（中身を書かない）。
/// ロックはツールのあと外れている。
#[test]
fn h9_a_linked_lock_file_is_not_written() {
    let dir = TestDir::new("hl-h9");
    let (path, mut vault) = new_vault(&dir, "v");
    let copy = dir.join("lock-copy");
    fs::hard_link(path.join(".vault").join("lock"), &copy).unwrap();
    let before = snap(&copy);

    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    assert_unchanged(&copy, &before, "lock の写し");
    assert!(before.bytes.is_empty());
    vault.set_lock_timeout(Duration::from_millis(200));
    drop(vault.lock().expect("ツールのあと、ロックは外れている"));
    assert_eq!(fs::read(&copy).unwrap(), b"", "写しを読める");
}

// ---------------------------------------------------------------------------
// ハードリンクで残すバックアップ
// ---------------------------------------------------------------------------

/// 保管庫の中のファイルを、.vault の下も含めてすべて `backup` の下へハードリンクする（フォルダは作る）。
fn link_all(from: &Path, backup: &Path, out: &mut Vec<PathBuf>) {
    fs::create_dir_all(backup).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let to = backup.join(entry.file_name());
        if ty.is_dir() {
            link_all(&entry.path(), &to, out);
        } else if ty.is_file() {
            fs::hard_link(entry.path(), &to).unwrap();
            out.push(to);
        }
    }
}

/// 保管庫の中のすべてのファイル（.vault の下の履歴・索引・vault.json・lock・ゴミ箱の entry.json と data も）を
/// 外へハードリンクする。そのあと書く（新しいファイル・上書き）・mkdir・move・delete・restore（replace と to）・走査を
/// しても、外のすべてのファイルのバイトと更新時刻は変わらない。
#[test]
fn h_backup_hard_link_backups_are_never_changed() {
    let dir = TestDir::new("hl-backup");
    let (path, mut vault) = new_vault(&dir, "v");
    vault.set_min_rescan_interval(Duration::ZERO);
    vault.set_rescan_interval_factor(0);
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a1"}),
    )
    .unwrap();
    let w = call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a2"}),
    )
    .unwrap();
    let old_a = text(&w, "previous_trash_id");
    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "bb"}),
    )
    .unwrap();
    call(&vault, "vault_mkdir", json!({"path": "d"})).unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "d/x.txt", "content": "x"}),
    )
    .unwrap();
    call(&vault, "vault_changes", json!({"since_seq": 0})).unwrap();

    let mut backups = Vec::new();
    link_all(&path, &dir.join("backup"), &mut backups);
    assert!(
        backups.iter().any(|p| p.ends_with("lock"))
            && backups
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "jsonl"))
            && backups.iter().any(|p| p.ends_with("index.json"))
            && backups.iter().any(|p| p.ends_with("entry.json"))
            && backups.iter().any(|p| p.ends_with("data")),
        "管理用のファイルもバックアップに入っている：{backups:?}"
    );
    let before: Vec<Snap> = backups.iter().map(|p| snap(p)).collect();

    call(
        &vault,
        "vault_write",
        json!({"path": "n.txt", "content": "new"}),
    )
    .unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a3"}),
    )
    .unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "d/x.txt", "content": "x2"}),
    )
    .unwrap();
    call(&vault, "vault_mkdir", json!({"path": "e"})).unwrap();
    call(
        &vault,
        "vault_move",
        json!({"from": "n.txt", "to": "e/n.txt"}),
    )
    .unwrap();
    call(&vault, "vault_move", json!({"from": "d", "to": "e/d"})).unwrap();
    let del = call(&vault, "vault_delete", json!({"path": "b.txt"})).unwrap();
    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "again"}),
    )
    .unwrap();
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": text(&del, "trash_id"), "replace": true}),
    )
    .unwrap();
    call(
        &vault,
        "vault_restore",
        json!({"trash_id": old_a, "to": "restored.txt"}),
    )
    .unwrap();
    // 外のアプリでの変更と走査（索引の置き換えと、external_* の履歴）
    fs::write(path.join("ext.txt"), b"external").unwrap();
    let c = call(&vault, "vault_changes", json!({"since_seq": 0})).unwrap();
    assert_eq!(c["scanned"], true, "{c}");

    for (p, b) in backups.iter().zip(&before) {
        assert_unchanged(p, b, &p.display().to_string());
    }
    assert_eq!(fs::read_to_string(path.join("b.txt")).unwrap(), "bb");
    assert_eq!(fs::read_to_string(path.join("restored.txt")).unwrap(), "a1");
}

// ---------------------------------------------------------------------------
// 走査での別名からの書き換え
// ---------------------------------------------------------------------------

/// 走査のテスト用の保管庫（続けて走査できるように、間隔を 0 にする）。
fn scan_vault(dir: &TestDir) -> (PathBuf, Vault) {
    let (path, mut vault) = new_vault(dir, "v");
    vault.set_min_rescan_interval(Duration::ZERO);
    vault.set_rescan_interval_factor(0);
    (path, vault)
}

/// 走査してから、`since` より後の外での変更（op, path）を読む。返り値は (変更, next_seq)。
fn rescan(vault: &Vault, since: u64) -> (Vec<(String, String)>, u64) {
    let r = call(vault, "vault_changes", json!({"since_seq": since})).unwrap();
    assert_eq!(r["scanned"], true, "{r}");
    let changes = r["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["op"].as_str().unwrap().to_owned(),
                c["path"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    (changes, r["next_seq"].as_u64().unwrap())
}

fn change(op: &str, path: &str) -> (String, String) {
    (op.to_owned(), path.to_owned())
}

/// 中のファイルを外とハードリンクにしてから走査する（索引に入れるときに開いて、つながっていると分かる）。
/// 外の名前から書き換えて走査すると、external_modify が1回だけ出る。もう一度走査しても何も出ない。
/// 回り番は止めて（parts＝0）、印だけで見つけることを確かめる。
#[test]
fn h10_a_known_link_written_through_the_outside_name_is_found() {
    let dir = TestDir::new("hl-h10");
    let (path, mut vault) = scan_vault(&dir);
    vault.set_link_sweep_parts(0);
    let inner = path.join("x.txt");
    fs::write(&inner, b"before").unwrap();
    let outside = dir.join("outside.txt");
    fs::hard_link(&inner, &outside).unwrap();

    let (found, seq) = rescan(&vault, 1); // 1 は init の行
    assert_eq!(found, [change("external_create", "x.txt")]);
    assert_eq!(link_marks(&vault), ["x.txt"], "つながっていると分かった");

    fs::write(&outside, b"written through the outside name").unwrap();
    let (found, seq) = rescan(&vault, seq);
    assert_eq!(found, [change("external_modify", "x.txt")]);
    let (found, _) = rescan(&vault, seq);
    assert!(found.is_empty(), "{found:?}");
}

/// 索引に入ったあとでリンクを作り、外の名前から書き換える（まだ分からないリンク）。回り番（parts＝16）で、
/// 間隔 0 で 16 回まで走査すると、ちょうど1回 external_modify が出る。回り番を止めると（parts＝0）、16 回走査
/// しても出ない（記録として固定する）。
#[test]
fn h10b_an_unknown_link_is_found_by_the_sweep_within_its_parts() {
    for (parts, expected) in [(16u32, 1usize), (0, 0)] {
        let dir = TestDir::new("hl-h10b");
        let (path, mut vault) = scan_vault(&dir);
        vault.set_link_sweep_parts(parts);
        set_sweep_slot(&vault, 0);
        for i in 0..20 {
            fs::write(path.join(format!("f{i:02}.txt")), format!("{i}")).unwrap();
        }
        let inner = path.join("x.txt");
        fs::write(&inner, b"before").unwrap();
        let (_, mut seq) = rescan(&vault, 0);
        let outside = dir.join("outside.txt");
        fs::hard_link(&inner, &outside).unwrap();
        fs::write(&outside, b"written through the outside name").unwrap();

        let mut modified = Vec::new();
        for round in 0..16 {
            let (found, next) = rescan(&vault, seq);
            seq = next;
            for c in found {
                assert_eq!(c, change("external_modify", "x.txt"), "回 {round}");
                modified.push(round);
            }
        }
        println!("H10b：parts={parts} で external_modify が出た回 {modified:?}");
        assert_eq!(modified.len(), expected, "parts={parts}");
        if expected > 0 {
            assert_eq!(link_marks(&vault), ["x.txt"]);
        }
    }
}

/// 外の名前を消したあとの走査では何も出ず、印から外れる。
#[test]
fn h10c_removing_the_outside_name_drops_the_mark_without_changes() {
    let dir = TestDir::new("hl-h10c");
    let (path, mut vault) = scan_vault(&dir);
    vault.set_link_sweep_parts(0);
    let inner = path.join("x.txt");
    fs::write(&inner, b"before").unwrap();
    let outside = dir.join("outside.txt");
    fs::hard_link(&inner, &outside).unwrap();
    let (_, seq) = rescan(&vault, 0);
    assert_eq!(link_marks(&vault), ["x.txt"]);

    fs::remove_file(&outside).unwrap();
    let (found, _) = rescan(&vault, seq);
    assert!(found.is_empty(), "{found:?}");
    assert!(link_marks(&vault).is_empty(), "印から外れる");
}

/// 印の付いたファイルのフォルダを読めない回は、その下の印を残す。読めるように
/// なってから外の名前から書き換えると、回り番なしで（印で）見つかる。
#[test]
fn h10d_marks_under_an_unreadable_folder_are_kept() {
    let dir = TestDir::new("hl-h10d");
    let (path, mut vault) = scan_vault(&dir);
    vault.set_link_sweep_parts(0);
    fs::create_dir(path.join("d")).unwrap();
    let inner = path.join("d").join("x.txt");
    fs::write(&inner, b"before").unwrap();
    let outside = dir.join("outside.txt");
    fs::hard_link(&inner, &outside).unwrap();
    let (_, seq) = rescan(&vault, 0);
    assert_eq!(link_marks(&vault), ["d/x.txt"]);

    let deny = common::DenyAce::new(&path.join("d"), "(RD)");
    let (found, seq) = rescan(&vault, seq);
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(
        link_marks(&vault),
        ["d/x.txt"],
        "読めないフォルダの下の印は残す"
    );
    drop(deny);

    fs::write(&outside, b"written through the outside name").unwrap();
    let (found, _) = rescan(&vault, seq);
    assert_eq!(found, [change("external_modify", "d/x.txt")]);
}

/// 外とつながったファイルを外の名前から書き換えると、vault_read・vault_list は新しい値を返す（ハンドルで読む）。
#[test]
fn h11_read_and_list_see_writes_through_the_outside_name() {
    let dir = TestDir::new("hl-h11");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "x.txt", "content": "before"}),
    )
    .unwrap();
    let outside = dir.join("outside.txt");
    fs::hard_link(path.join("x.txt"), &outside).unwrap();
    fs::write(&outside, b"written through the outside name").unwrap();

    let r = call(&vault, "vault_read", json!({"path": "x.txt"})).unwrap();
    assert_eq!(r["content"], "written through the outside name");
    let list = call(&vault, "vault_list", json!({"path": ""})).unwrap();
    let item = list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "x.txt" || e["path"] == "x.txt")
        .unwrap_or_else(|| panic!("x.txt がない：{list}"))
        .clone();
    assert_eq!(item["size"], 32, "{item}");
    assert_eq!(item["version"], r["version"], "{item}");
}

/// 記録：最悪の形（すべてのファイルが外とハードリンクでつながっている。ハードリンクで残すバックアップ）の
/// 1万ファイルの走査の時間。すべてが印に入り、変更のない走査でも毎回すべてを開く。
/// `cargo test --release -p vault-core --test hard_links -- --ignored --nocapture`
#[test]
#[ignore = "1万ファイルを作ってハードリンクし、時間を測る（--ignored で動かす）"]
fn h12_time_a_scan_of_ten_thousand_linked_files() {
    let dir = TestDir::new("hl-h12");
    let (path, vault) = scan_vault(&dir);
    let backup = dir.join("backup");
    for d in 0..100 {
        let folder = path.join(format!("d{d:03}"));
        let copy = backup.join(format!("d{d:03}"));
        fs::create_dir(&folder).unwrap();
        fs::create_dir_all(&copy).unwrap();
        for f in 0..100 {
            let name = format!("f{f:03}.txt");
            fs::write(folder.join(&name), format!("{d}-{f}")).unwrap();
            fs::hard_link(folder.join(&name), copy.join(&name)).unwrap();
        }
    }
    let start = Instant::now();
    rescan(&vault, 0);
    let first = start.elapsed();
    assert_eq!(link_marks(&vault).len(), 10_000, "すべてが印に入る");
    // 最初の走査の行は既定の limit で切れるので、続きは履歴の最後から読む
    let mut seq = journal(&path).last().unwrap().seq;
    let mut steady = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let (found, next) = rescan(&vault, seq);
        steady.push(start.elapsed());
        assert!(found.is_empty());
        seq = next;
    }
    steady.sort();
    println!(
        "走査（1万ファイル。すべて外とハードリンク）：最初 {first:?}／変更なし：最小 {:?}・中央 {:?}・最大 {:?}",
        steady[0],
        steady[steady.len() / 2],
        steady[steady.len() - 1],
    );
}
