//! 操作の履歴。

mod common;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;

use common::{TestDir, actor, call, env, journal, new_vault};
use serde_json::json;
use vault_core::{ActorKind, MAX_JOURNAL_FILE_BYTES, Op, Vault};

fn journal_files(vault_path: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(vault_path.join(".vault").join("journal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
}

fn seqs(vault_path: &Path) -> Vec<u64> {
    journal(vault_path).iter().map(|e| e.seq).collect()
}

fn append_raw(file: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(file)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

#[test]
fn init_is_the_first_entry() {
    let dir = TestDir::new("journal-init");
    let (path, _vault) = new_vault(&dir, "v");
    let files = journal_files(&path);
    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0].file_name().unwrap().to_str().unwrap(),
        "00000000000000000001.jsonl"
    );
    let entries = journal(&path);
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!((e.seq, e.op), (1, Op::Init));
    assert_eq!(e.actor, actor());
    assert_eq!(e.actor.kind(), ActorKind::User);
    assert!(e.ts.ends_with('Z'));
    // 1行で、改行で終わる
    let text = fs::read_to_string(&files[0]).unwrap();
    assert_eq!(text.matches('\n').count(), 1);
    assert!(text.ends_with('\n'));
}

/// 2つの Vault を、スレッドで並べて書いても、seq が重ならず、抜けもない。
#[test]
fn two_vaults_writing_in_parallel_get_unique_contiguous_seqs() {
    let dir = TestDir::new("journal-parallel");
    let (path, _vault) = new_vault(&dir, "v");
    const PER_THREAD: usize = 25;
    let handles: Vec<_> = (0..2)
        .map(|t| {
            let vault = Vault::open(&path, false, &env()).unwrap();
            thread::spawn(move || {
                for i in 0..PER_THREAD {
                    call(
                        &vault,
                        "vault_write",
                        json!({"path": format!("t{t}-{i}.txt"), "content": format!("{t}-{i}")}),
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let got = seqs(&path);
    let expected: Vec<u64> = (1..=(1 + 2 * PER_THREAD as u64)).collect();
    assert_eq!(got, expected);
    let writes = journal(&path).iter().filter(|e| e.op == Op::Write).count();
    assert_eq!(writes, 2 * PER_THREAD);
}

/// 履歴の最後の行が壊れていても（改行で終わっていなくても）、続きの seq から書ける。
#[test]
fn a_broken_last_line_does_not_stop_the_journal() {
    let dir = TestDir::new("journal-broken");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let file = journal_files(&path).pop().unwrap();

    // 改行で終わらない壊れた行
    append_raw(&file, br#"{"seq":99,"ts":"20"#);
    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    )
    .unwrap();
    assert_eq!(seqs(&path), [1, 2, 3]);
    let text = fs::read_to_string(&file).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(
        lines[2], r#"{"seq":99,"ts":"20"#,
        "壊れた行は1行に閉じ込める"
    );

    // 改行で終わる壊れた行
    append_raw(&file, b"not json at all\n");
    call(
        &vault,
        "vault_write",
        json!({"path": "c.txt", "content": "c"}),
    )
    .unwrap();
    assert_eq!(seqs(&path), [1, 2, 3, 4]);
}

/// 8 MiB を超えたら、次のファイル（名前は最初の seq の20桁）に分ける。
#[test]
fn the_journal_moves_to_a_new_file_after_8_mib() {
    let dir = TestDir::new("journal-rotate");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let first = journal_files(&path).pop().unwrap();
    // 8 MiB の大きな行（読めない行）を足して、ファイルを大きくする
    let mut pad = vec![b'x'; MAX_JOURNAL_FILE_BYTES as usize];
    pad.push(b'\n');
    append_raw(&first, &pad);

    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    )
    .unwrap();
    let files = journal_files(&path);
    assert_eq!(files.len(), 2);
    assert_eq!(
        files[1].file_name().unwrap().to_str().unwrap(),
        "00000000000000000003.jsonl"
    );
    call(
        &vault,
        "vault_write",
        json!({"path": "c.txt", "content": "c"}),
    )
    .unwrap();
    assert_eq!(journal_files(&path).len(), 2, "新しいファイルに続けて書く");
    assert_eq!(seqs(&path), [1, 2, 3, 4]);
}

/// 8 MiB 以上の最後の履歴ファイルに読める行が1つもなくても、次のファイルの名前がぶつからずに書ける。
#[test]
fn a_large_last_file_without_readable_lines_does_not_block_the_journal() {
    let dir = TestDir::new("journal-unreadable-large");
    let (path, vault) = new_vault(&dir, "v");
    let journal_dir = path.join(".vault").join("journal");
    // 最後のファイル（最初の seq が 2）に、読めない行だけで 8 MiB
    let mut garbage = vec![b'x'; MAX_JOURNAL_FILE_BYTES as usize];
    garbage.push(b'\n');
    fs::write(journal_dir.join("00000000000000000002.jsonl"), garbage).unwrap();

    call(
        &vault,
        "vault_write",
        json!({"path": "a.txt", "content": "a"}),
    )
    .unwrap();
    let names: Vec<String> = journal_files(&path)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "00000000000000000001.jsonl",
            "00000000000000000002.jsonl",
            "00000000000000000003.jsonl"
        ]
    );
    let last = journal(&path).pop().unwrap();
    assert_eq!((last.seq, last.op), (3, Op::Write));
    // そのあとも続けて書ける
    call(
        &vault,
        "vault_write",
        json!({"path": "b.txt", "content": "b"}),
    )
    .unwrap();
    assert_eq!(journal(&path).pop().unwrap().seq, 4);
}

/// ツールの呼び出し1回の行（途中のフォルダの mkdir と write）は、続いた seq でまとめて書かれる。
#[test]
fn one_call_writes_its_lines_together() {
    let dir = TestDir::new("journal-batch");
    let (path, vault) = new_vault(&dir, "v");
    call(
        &vault,
        "vault_write",
        json!({"path": "p/q/r/x.txt", "content": "x"}),
    )
    .unwrap();
    let entries = journal(&path);
    let ops: Vec<(u64, Op)> = entries.iter().map(|e| (e.seq, e.op)).collect();
    assert_eq!(
        ops,
        [
            (1, Op::Init),
            (2, Op::Mkdir),
            (3, Op::Mkdir),
            (4, Op::Mkdir),
            (5, Op::Write)
        ]
    );
}
