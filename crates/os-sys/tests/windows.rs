//! 実機の確かめ（R1〜R13 と R2f。R7①・R7③・R8 の FileFullDirectoryInfo の道・R9・R12 の
//! 分けて呼ぶ道は、中の関数を使うので src のユニットテストにある）。
//!
//! - ファイルは `env!("CARGO_TARGET_TMPDIR")` の下にだけ作る（本物のホームや %TEMP% には作らない）。
//! - 「必ず通ること」は assert で確かめる。「記録すること」は、結果（エラーの番号）を出力に書く
//!   （`cargo test -p os-sys --test windows -- --nocapture --test-threads=1` で見る）。
//! - 開き方の定数は、vault-core の guard に置くものと同じ値をここに書く。

#![cfg(windows)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use os_sys::{
    Access, Dir, EntryKind, InvalidName, KnownFolder, Links, Name, Opened, OpenedPass, Parent,
    PassDir, Share, Want, create_dir_child, create_file_child, delete_by_handle, file_id,
    final_path, free_space, known_folder, open_child, open_pass_dir, open_path, os_random,
    read_dir_by_handle, rename_no_replace, rename_replace,
};

// 開き方の定数（vault-core の guard の定数と同じ値）
const HOLD_DIR: Access = Access::LIST_DIRECTORY
    .with(Access::TRAVERSE)
    .with(Access::READ_ATTRIBUTES);
const CREATE_DIR: Access = HOLD_DIR.with(Access::DELETE);
const MANAGED_DIR: Access = HOLD_DIR.with(Access::DELETE);
const TARGET_FILE: Access = Access::DELETE.with(Access::READ_ATTRIBUTES);
const TARGET_DIR: Access = Access::DELETE
    .with(Access::LIST_DIRECTORY)
    .with(Access::TRAVERSE)
    .with(Access::READ_ATTRIBUTES);
const LEAF_READ: Access = Access::READ_DATA.with(Access::READ_ATTRIBUTES);
const TMP_CREATE: Access = Access::WRITE_DATA
    .with(Access::READ_ATTRIBUTES)
    .with(Access::WRITE_ATTRIBUTES)
    .with(Access::DELETE);
const SHARE_RW: Share = Share::READ.with(Share::WRITE);

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

// ---------------------------------------------------------------------------
// 道具
// ---------------------------------------------------------------------------

fn tests_base() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("os-sys-tests")
}

/// テストごとのフォルダ。Drop で中身ごと消す（`os-sys-tests` の下だけ）。
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let path = tests_base().join(format!(
            "{name}-{}-{}-{}",
            std::process::id(),
            nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let base = tests_base();
        if self.path.starts_with(&base) && self.path != base {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn name(s: &str) -> Name {
    Name::new(OsStr::new(s)).unwrap()
}

/// 根を std で開く（リンクをたどらない。HOLD_DIR）。
fn root(path: &Path) -> Dir {
    match open_path(path, Links::DontFollow, HOLD_DIR, SHARE_RW).unwrap() {
        Opened::Dir(d) => d,
        _ => panic!("根がフォルダでない"),
    }
}

fn child_dir(parent: &impl Parent, n: &str, access: Access) -> Dir {
    match open_child(parent, &name(n), Want::Dir, access, SHARE_RW).unwrap() {
        Opened::Dir(d) => d,
        _ => panic!("{n} がフォルダでない"),
    }
}

fn child_file(parent: &impl Parent, n: &str, access: Access) -> File {
    match open_child(parent, &name(n), Want::Any, access, SHARE_RW).unwrap() {
        Opened::File(f) => f,
        _ => panic!("{n} がファイルでない"),
    }
}

/// 移すフォルダを TARGET_DIR で開く（種類を求めず、ハンドルで確かめる）。
fn child_target_dir(parent: &impl Parent, n: &str) -> Dir {
    match open_child(parent, &name(n), Want::Any, TARGET_DIR, SHARE_RW).unwrap() {
        Opened::Dir(d) => d,
        _ => panic!("{n} がフォルダでない"),
    }
}

/// 通り抜けるだけのフォルダとして開く（PassDir）。
fn pass(parent: &impl Parent, n: &str) -> PassDir {
    match open_pass_dir(parent, &name(n)).unwrap() {
        OpenedPass::Dir(p) => p,
        OpenedPass::Link(_) => panic!("{n} がリンク"),
    }
}

/// 結果を「ok」か「err(番号)」の文字にする（記録のため）。
fn code<T>(r: &io::Result<T>) -> String {
    match r {
        Ok(_) => "ok".to_string(),
        Err(e) => match e.raw_os_error() {
            Some(c) => format!("err({c})"),
            None => format!("err({:?})", e.kind()),
        },
    }
}

fn raw<T: std::fmt::Debug>(r: io::Result<T>) -> Option<i32> {
    r.unwrap_err().raw_os_error()
}

/// cmd の内部コマンドと icacls に渡すパス：絶対パスにし、`/` を `\` に直す。
fn cmd_path(path: &Path) -> OsString {
    let absolute = std::path::absolute(path).unwrap();
    let wide: Vec<u16> = absolute
        .as_os_str()
        .encode_wide()
        .map(|c| {
            if c == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                c
            }
        })
        .collect();
    OsString::from_wide(&wide)
}

fn decode(bytes: &[u8]) -> String {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if rest.is_empty() && pairs.iter().any(|p| p[1] == 0) {
        let units: Vec<u16> = pairs.iter().map(|&p| u16::from_le_bytes(p)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// ジャンクションを作る（`cmd /U /C mklink /J`）。ボリュームが対応しないときだけ、出力して false。
fn junction(link: &Path, target: &Path) -> bool {
    let out = Command::new("cmd")
        .arg("/U")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(cmd_path(link))
        .arg(cmd_path(target))
        .output()
        .expect("cmd を起動できない");
    if out.status.success() {
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        return true;
    }
    let text = decode(&out.stdout) + &decode(&out.stderr);
    if text.contains("NTFS")
        || text.contains("サポートされていません")
        || text.to_lowercase().contains("not supported")
    {
        println!("飛ばした：このボリュームはジャンクションに対応しない");
        return false;
    }
    panic!(
        "mklink /J が失敗した（終了コード {:?}）：{}",
        out.status.code(),
        text.trim()
    );
}

/// フォルダに Everyone（S-1-1-0）への拒否を付ける。Drop で外す。
struct DenyAce {
    path: OsString,
}

impl DenyAce {
    fn new(path: &Path, rights: &str) -> Self {
        let path = cmd_path(path);
        let out = Command::new("icacls")
            .arg(&path)
            .arg("/deny")
            .arg(format!("*S-1-1-0:{rights}"))
            .output()
            .expect("icacls を起動できない");
        assert!(
            out.status.success(),
            "icacls /deny が失敗した（終了コード {:?}）",
            out.status.code()
        );
        Self { path }
    }
}

impl Drop for DenyAce {
    fn drop(&mut self) {
        let _ = Command::new("icacls")
            .arg(&self.path)
            .arg("/remove:d")
            .arg("*S-1-1-0")
            .output();
    }
}

fn set_read_only(path: &Path, read_only: bool) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(read_only);
    fs::set_permissions(path, perms).unwrap();
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

// ---------------------------------------------------------------------------
// 名前・権利の型
// ---------------------------------------------------------------------------

/// Name の表：断るもの・通すもの。
#[test]
fn name_table() {
    let reject: [(&str, InvalidName); 9] = [
        ("", InvalidName::Empty),
        (".", InvalidName::Dots),
        ("..", InvalidName::Dots),
        ("a\\b", InvalidName::Separator),
        ("a/b", InvalidName::Separator),
        ("C:x", InvalidName::Separator),
        ("\\??\\C:\\x", InvalidName::Separator),
        ("a\0b", InvalidName::Nul),
        ("", InvalidName::TooLong), // 下で 256 文字に置き換える
    ];
    for (s, why) in reject.iter().take(8) {
        assert_eq!(Name::new(OsStr::new(s)), Err(*why), "{s:?}");
    }
    assert_eq!(
        Name::new(OsStr::new(&"x".repeat(256))),
        Err(InvalidName::TooLong)
    );
    assert_eq!(
        Name::new(OsStr::new(&"あ".repeat(256))),
        Err(InvalidName::TooLong)
    );
    for s in ["日本語の名前.txt", ".vault", "1234-5.tmp", "a"] {
        assert_eq!(
            Name::new(OsStr::new(s)).unwrap().to_os_string(),
            OsString::from(s)
        );
    }
    assert!(Name::new(OsStr::new(&"x".repeat(255))).is_ok());
    assert!(Name::new(OsStr::new(&"あ".repeat(255))).is_ok());
}

/// 権利と共有は、定数から組むだけ。開き方の定数のフォルダの分は、DELETE を共有しない。
#[test]
fn access_and_share_constants() {
    assert_eq!(HOLD_DIR.bits(), 0x1 | 0x20 | 0x80);
    assert_eq!(CREATE_DIR.bits(), 0x1 | 0x20 | 0x80 | 0x10000);
    assert!(TARGET_DIR.contains(Access::DELETE));
    assert!(!HOLD_DIR.contains(Access::DELETE));
    assert_eq!(Access::READ_DATA | Access::READ_ATTRIBUTES, LEAF_READ);
    assert_eq!(SHARE_RW.bits(), 0x3);
    assert!(!SHARE_RW.contains(Share::DELETE));
    assert_eq!((Share::READ | Share::DELETE).bits(), 0x5);
}

// ---------------------------------------------------------------------------
// R1 RootDirectory が効く（必ず通る）
// ---------------------------------------------------------------------------

#[test]
fn r1_root_directory_is_used() {
    let t = TestDir::new("r1");
    fs::create_dir(t.join("d1")).unwrap();
    fs::create_dir(t.join("d2")).unwrap();
    fs::write(t.join("d1").join("a.txt"), b"a").unwrap();
    let moved = format!("r1-moved-{}-{}.txt", std::process::id(), nanos());

    let top = root(t.path());
    let d1 = child_dir(&top, "d1", HOLD_DIR);
    let d2 = child_dir(&top, "d2", HOLD_DIR);
    let a = child_file(&d1, "a.txt", TARGET_FILE);
    rename_no_replace(&a, &d2, &name(&moved)).unwrap();

    assert!(t.join("d2").join(&moved).is_file());
    assert!(!t.join("d1").join("a.txt").exists());
    assert!(!t.join("d1").join(&moved).exists());
    assert!(
        !Path::new(&moved).exists(),
        "プロセスの今のフォルダにできてはいけない"
    );
    assert_eq!(
        final_path(&a).unwrap(),
        final_path(d2.as_file()).unwrap().join(&moved)
    );
    println!("R1：RootDirectory に渡した親（d2）へ移った。ok");
}

// ---------------------------------------------------------------------------
// R2 共有の違反にならない
// ---------------------------------------------------------------------------

/// R2a の1回：`s` のファイル f.txt とフォルダ g（中に x.txt）を、親 `p` へ置き換えない rename で移す。
fn move_file_and_dir_into(s: &impl Parent, p: &impl Parent) -> (io::Result<()>, io::Result<()>) {
    let f = child_file(s, "f.txt", TARGET_FILE);
    let file_result = rename_no_replace(&f, p, &name("f.txt"));
    let g = child_target_dir(s, "g");
    let dir_result = rename_no_replace(g.as_file(), p, &name("g"));
    (file_result, dir_result)
}

/// R2a の1回：`p` へ移し、`p` の中にファイルとフォルダを作る。結果を出力に書き、どれも成功することを確かめる。
fn move_and_create_into(label: &str, s: &impl Parent, p: &impl Parent) {
    let (file_result, dir_result) = move_file_and_dir_into(s, p);
    let made_file = create_file_child(p, &name("new.txt"), TMP_CREATE, Share::READ);
    let made_dir = create_dir_child(p, &name("newdir"), CREATE_DIR, SHARE_RW);
    println!(
        "R2a {label}：ファイルの rename={}、フォルダの rename={}、ファイルを作る={}、フォルダを作る={}",
        code(&file_result),
        code(&dir_result),
        code(&made_file),
        code(&made_dir)
    );
    file_result.unwrap();
    dir_result.unwrap();
    made_file.unwrap();
    made_dir.unwrap();
}

/// R2a（必ず通る）：親を RootDirectory に渡すと、ファイルもフォルダも rename でき、中に作れる。親は4通り：
/// HOLD_DIR で開いたフォルダ、PassDir で開いたフォルダ、「CREATE_DIR で作って閉じ、HOLD_DIR で開き直した
/// フォルダ」、同じく PassDir で開き直したフォルダ。
#[test]
fn r2a_rename_into_held_parent() {
    let t = TestDir::new("r2a");
    for s in ["s1", "s2", "s3", "s4"] {
        fs::create_dir_all(t.join(s).join("g")).unwrap();
        fs::write(t.join(s).join("g").join("x.txt"), b"x").unwrap();
        fs::write(t.join(s).join("f.txt"), b"f").unwrap();
    }
    fs::create_dir(t.join("p")).unwrap();
    fs::create_dir(t.join("pp")).unwrap();

    let top = root(t.path());
    let p = child_dir(&top, "p", HOLD_DIR);
    move_and_create_into("HOLD_DIR の親", &child_dir(&top, "s1", HOLD_DIR), &p);
    let pp = pass(&top, "pp");
    move_and_create_into("PassDir の親", &pass(&top, "s2"), &pp);

    // 作って閉じ、開き直した親（ID も作ったときと同じ）
    let created = create_dir_child(&top, &name("pc"), CREATE_DIR, SHARE_RW).unwrap();
    let created_id = created.file_id().unwrap();
    drop(created);
    let reopened = child_dir(&top, "pc", HOLD_DIR);
    assert_eq!(file_id(reopened.as_file()).unwrap(), created_id);
    move_and_create_into(
        "作って閉じ、HOLD_DIR で開き直した親",
        &child_dir(&top, "s3", HOLD_DIR),
        &reopened,
    );

    let created = create_dir_child(&top, &name("pcp"), CREATE_DIR, SHARE_RW).unwrap();
    let created_id = created.file_id().unwrap();
    drop(created);
    let reopened_pass = pass(&top, "pcp");
    assert_eq!(file_id(reopened_pass.as_file()).unwrap(), created_id);
    move_and_create_into(
        "作って閉じ、PassDir で開き直した親",
        &child_dir(&top, "s4", HOLD_DIR),
        &reopened_pass,
    );

    for parent in ["p", "pp", "pc", "pcp"] {
        assert_eq!(read_text(&t.join(parent).join("f.txt")), "f");
        assert_eq!(read_text(&t.join(parent).join("g").join("x.txt")), "x");
        assert!(t.join(parent).join("new.txt").is_file());
        assert!(t.join(parent).join("newdir").is_dir());
    }
}

/// R2b の1回：同じ親を2つのハンドルで持ち、1つめへ rename、2つめの中に作る。
fn two_holders(label: &str, p1: &impl Parent, p2: &impl Parent, s: &impl Parent, f: &str) {
    let file = child_file(s, f, TARGET_FILE);
    let renamed = rename_no_replace(&file, p1, &name(f));
    let created_file = create_file_child(p2, &name(&format!("new-{f}")), TMP_CREATE, Share::READ);
    let created_dir = create_dir_child(p2, &name(&format!("dir-{f}")), CREATE_DIR, SHARE_RW);
    println!(
        "R2b {label}：rename={}、ファイルを作る={}、フォルダを作る={}",
        code(&renamed),
        code(&created_file),
        code(&created_dir)
    );
    renamed.unwrap();
    created_file.unwrap();
    created_dir.unwrap();
}

/// R2b（必ず通る）：別のハンドルが同じ親を持っていても（HOLD_DIR どうし・PassDir どうし）、そこへの rename と、
/// そこでの作成ができる（走査と書き込みが同時に動くため）。
#[test]
fn r2b_two_holders_rename_and_create() {
    let t = TestDir::new("r2b");
    fs::create_dir(t.join("p")).unwrap();
    fs::create_dir(t.join("s")).unwrap();
    fs::write(t.join("s").join("f1.txt"), b"f").unwrap();
    fs::write(t.join("s").join("f2.txt"), b"f").unwrap();

    let top = root(t.path());
    let s = child_dir(&top, "s", HOLD_DIR);
    let p1 = child_dir(&top, "p", HOLD_DIR);
    let p2 = child_dir(&top, "p", HOLD_DIR);
    two_holders("HOLD_DIR どうし", &p1, &p2, &s, "f1.txt");
    let q1 = pass(&top, "p");
    let q2 = pass(&top, "p");
    two_holders(
        "PassDir どうし（HOLD_DIR も持ったまま）",
        &q1,
        &q2,
        &s,
        "f2.txt",
    );
    assert!(t.join("p").join("f1.txt").is_file());
    assert!(t.join("p").join("f2.txt").is_file());
}

/// R2c（必ず通る）：持っている親の中のものを、ほかへ rename できる（fs::rename でも、ハンドルの rename でも）。
/// ハンドルで消せる。
#[test]
fn r2c_move_out_of_held_parent() {
    let t = TestDir::new("r2c");
    fs::create_dir(t.join("p")).unwrap();
    fs::create_dir(t.join("q")).unwrap();
    for n in ["c1.txt", "c2.txt", "c3.txt"] {
        fs::write(t.join("p").join(n), n).unwrap();
    }
    for n in ["k1", "k2", "k3"] {
        fs::create_dir(t.join("p").join(n)).unwrap();
    }

    let top = root(t.path());
    let p = child_dir(&top, "p", HOLD_DIR);
    let q = child_dir(&top, "q", HOLD_DIR);

    let std_file = fs::rename(t.join("p").join("c1.txt"), t.join("q").join("c1.txt"));
    let std_dir = fs::rename(t.join("p").join("k1"), t.join("q").join("k1"));
    let c2 = child_file(&p, "c2.txt", TARGET_FILE);
    let handle_file = rename_no_replace(&c2, &q, &name("c2.txt"));
    let k2 = child_target_dir(&p, "k2");
    let handle_dir = rename_no_replace(k2.as_file(), &q, &name("k2"));
    let c3 = child_file(&p, "c3.txt", TARGET_FILE);
    let del_file = delete_by_handle(&c3);
    let k3 = child_target_dir(&p, "k3");
    let del_dir = delete_by_handle(k3.as_file());
    println!(
        "R2c：fs::rename ファイル={}・フォルダ={}、ハンドルの rename ファイル={}・フォルダ={}、ハンドルで消す ファイル={}・フォルダ={}",
        code(&std_file),
        code(&std_dir),
        code(&handle_file),
        code(&handle_dir),
        code(&del_file),
        code(&del_dir)
    );
    for r in [
        std_file,
        std_dir,
        handle_file,
        handle_dir,
        del_file,
        del_dir,
    ] {
        r.unwrap();
    }
    drop((c2, k2, c3, k3));
    for n in ["c1.txt", "c2.txt", "k1", "k2"] {
        assert!(t.join("q").join(n).exists(), "{n}");
    }
    assert!(!t.join("p").join("c3.txt").exists());
    assert!(!t.join("p").join("k3").exists());
    drop((p, q));
}

/// R2e（必ず通る）：親を HOLD_DIR か PassDir で持っているとき、その中のものを TARGET_FILE・TARGET_DIR で開ける。
#[test]
fn r2e_open_target_inside_held_parent() {
    let t = TestDir::new("r2e");
    fs::create_dir_all(t.join("p").join("g")).unwrap();
    fs::write(t.join("p").join("f.txt"), b"f").unwrap();
    let top = root(t.path());
    let p = child_dir(&top, "p", HOLD_DIR);
    let f = open_child(&p, &name("f.txt"), Want::Any, TARGET_FILE, SHARE_RW);
    let g = open_child(&p, &name("g"), Want::Any, TARGET_DIR, SHARE_RW);
    println!(
        "R2e HOLD_DIR の親：TARGET_FILE={}、TARGET_DIR={}",
        code(&f),
        code(&g)
    );
    assert!(matches!(f.unwrap(), Opened::File(_)));
    assert!(matches!(g.unwrap(), Opened::Dir(_)));
    drop(p);
    let pp = pass(&top, "p");
    let f = open_child(&pp, &name("f.txt"), Want::Any, TARGET_FILE, SHARE_RW);
    let g = open_child(&pp, &name("g"), Want::Any, TARGET_DIR, SHARE_RW);
    println!(
        "R2e PassDir の親：TARGET_FILE={}、TARGET_DIR={}",
        code(&f),
        code(&g)
    );
    assert!(matches!(f.unwrap(), Opened::File(_)));
    assert!(matches!(g.unwrap(), Opened::Dir(_)));
}

/// R2d（記録）：一覧を拒否（RD）・通り抜けを拒否（X）したフォルダを、HOLD_DIR・データの権利を1つにした開き方・
/// PassDir で開けるか。その下のファイルを std で開けるか（今の Phase 0 の道）。
#[test]
fn r2d_denied_folder_record() {
    let t = TestDir::new("r2d");
    fs::create_dir(t.join("x")).unwrap();
    fs::write(t.join("x").join("f.txt"), b"f").unwrap();
    let top = root(t.path());
    let cases = [
        (
            "一覧を拒否（RD）",
            "(RD)",
            "TRAVERSE・READ_ATTRIBUTES",
            Access::TRAVERSE | Access::READ_ATTRIBUTES,
        ),
        (
            "通り抜けを拒否（X）",
            "(X)",
            "LIST_DIRECTORY・READ_ATTRIBUTES",
            Access::LIST_DIRECTORY | Access::READ_ATTRIBUTES,
        ),
    ];
    for (label, rights, alt_label, alt) in cases {
        let deny = DenyAce::new(&t.join("x"), rights);
        let hold = open_child(&top, &name("x"), Want::Dir, HOLD_DIR, SHARE_RW);
        let other = open_child(&top, &name("x"), Want::Dir, alt, SHARE_RW);
        let passed = open_pass_dir(&top, &name("x"));
        let under = File::open(t.join("x").join("f.txt"));
        let under_relative = match &other {
            Ok(Opened::Dir(d)) => code(&open_child(
                d,
                &name("f.txt"),
                Want::Any,
                LEAF_READ,
                SHARE_RW,
            )),
            _ => "（開けなかったので試さない）".to_string(),
        };
        let under_pass = match &passed {
            Ok(OpenedPass::Dir(p)) => code(&open_child(
                p,
                &name("f.txt"),
                Want::Any,
                LEAF_READ,
                SHARE_RW,
            )),
            _ => "（開けなかったので試さない）".to_string(),
        };
        println!(
            "R2d {label}：(i) HOLD_DIR={}、(ii) {alt_label}={}、PassDir={}、(iii) std で下のファイル={}、(ii) を基準に下のファイル={}、PassDir を基準に下のファイル={}",
            code(&hold),
            code(&other),
            code(&passed),
            code(&under),
            under_relative,
            under_pass
        );
        drop((hold, other, passed, under));
        drop(deny);
    }
}

/// R2f（記録。設計の決定で「必ず通る」から外した）：移す先の親を、自分の DELETE つきの
/// ハンドル（MANAGED_DIR＝CREATE_DIR と同じ権利、TARGET_DIR）で持ったまま、そこへファイルとフォルダを、
/// 置き換えない rename で移せるか、中に作れるか。`Created<Dir>` そのものは親として渡せない（型で断る）。
#[test]
fn r2f_rename_into_parent_held_with_delete_record() {
    let t = TestDir::new("r2f");
    fs::create_dir(t.join("s")).unwrap();
    for n in ["pm", "pt"] {
        fs::create_dir(t.join(n)).unwrap();
    }
    let top = root(t.path());
    let s = child_dir(&top, "s", HOLD_DIR);
    let managed = child_dir(&top, "pm", MANAGED_DIR);
    let target = child_target_dir(&top, "pt");
    let parents: [(&str, &Dir); 2] = [
        ("MANAGED_DIR（CREATE_DIR と同じ権利）", &managed),
        ("TARGET_DIR", &target),
    ];
    for (i, (label, parent)) in parents.iter().enumerate() {
        let f = format!("f{i}.txt");
        let g = format!("g{i}");
        fs::write(t.join("s").join(&f), b"f").unwrap();
        fs::create_dir(t.join("s").join(&g)).unwrap();
        let file = child_file(&s, &f, TARGET_FILE);
        let dir = child_target_dir(&s, &g);
        let file_result = rename_no_replace(&file, *parent, &name(&f));
        let dir_result = rename_no_replace(dir.as_file(), *parent, &name(&g));
        let created_inside = create_file_child(
            *parent,
            &name(&format!("c{i}.txt")),
            TMP_CREATE,
            Share::READ,
        );
        println!(
            "R2f {label} で持つ親へ：ファイルの rename={}、フォルダの rename={}、その中に作る={}",
            code(&file_result),
            code(&dir_result),
            code(&created_inside)
        );
    }
}

/// R18（必ず通る）：PassDir で持った親への rename が成功し、その親は、持っているあいだ fs::rename で 32。
#[test]
fn r18_pass_dir_parent_holds_name() {
    let t = TestDir::new("r18");
    fs::create_dir(t.join("s")).unwrap();
    fs::create_dir(t.join("p")).unwrap();
    fs::write(t.join("s").join("f.txt"), b"f").unwrap();
    fs::create_dir(t.join("s").join("g")).unwrap();
    let top = root(t.path());
    let s = child_dir(&top, "s", HOLD_DIR);
    let p = pass(&top, "p");
    let f = child_file(&s, "f.txt", TARGET_FILE);
    let file_result = rename_no_replace(&f, &p, &name("f.txt"));
    let g = child_target_dir(&s, "g");
    let dir_result = rename_no_replace(g.as_file(), &p, &name("g"));
    let held_rename = fs::rename(t.join("p"), t.join("p2"));
    println!(
        "R18 PassDir の親へ：ファイル={}、フォルダ={}。持っている親の fs::rename={}",
        code(&file_result),
        code(&dir_result),
        code(&held_rename)
    );
    file_result.unwrap();
    dir_result.unwrap();
    assert_eq!(raw(held_rename), Some(32));
    drop((f, g, p));
    assert!(t.join("p").join("f.txt").is_file());
    assert!(t.join("p").join("g").is_dir());
}

/// R19（記録）：ほかのボリュームのフォルダのハンドルを RootDirectory に渡すと、どう断るか（17＝
/// ERROR_NOT_SAME_DEVICE の見込み）。書けない場所（Windows のフォルダ）を使い、移ってしまうことがないようにする。
#[test]
fn r19_other_volume_root_record() {
    let t = TestDir::new("r19");
    fs::write(t.join("a.txt"), b"a").unwrap();
    let top = root(t.path());
    let windows = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let other = match open_path(&windows, Links::DontFollow, HOLD_DIR, SHARE_RW) {
        Ok(Opened::Dir(d)) => d,
        other => {
            println!(
                "R19 飛ばした：Windows のフォルダを開けない（{}）",
                code(&other)
            );
            return;
        }
    };
    if file_id(other.as_file()).unwrap().volume == file_id(top.as_file()).unwrap().volume {
        println!("R19 飛ばした：Windows のフォルダが、テストの場所と同じボリューム");
        return;
    }
    let unique = format!("os-sys-r19-{}.txt", std::process::id());
    let a = child_file(&top, "a.txt", TARGET_FILE);
    let result = rename_no_replace(&a, &other, &name(&unique));
    println!("R19 ほかのボリュームの親へ：{}", code(&result));
    assert!(result.is_err(), "ほかのボリュームへ移ってはいけない");
    drop(a);
    assert!(t.join("a.txt").is_file());
    assert!(!windows.join(&unique).exists());
}

// ---------------------------------------------------------------------------
// R3〜R5 rename
// ---------------------------------------------------------------------------

/// R3：置き換えない rename は、同じ名前のファイルかフォルダがあると 183 で、どちらも変わらない。
#[test]
fn r3_no_replace_rejects_existing() {
    let t = TestDir::new("r3");
    fs::write(t.join("a.txt"), b"a").unwrap();
    fs::write(t.join("b.txt"), b"b").unwrap();
    fs::create_dir(t.join("d")).unwrap();
    fs::create_dir(t.join("e")).unwrap();
    let top = root(t.path());
    let a = child_file(&top, "a.txt", TARGET_FILE);
    let onto_file = rename_no_replace(&a, &top, &name("b.txt"));
    let onto_dir = rename_no_replace(&a, &top, &name("d"));
    let e = child_target_dir(&top, "e");
    let dir_onto_file = rename_no_replace(e.as_file(), &top, &name("b.txt"));
    println!(
        "R3：ファイル→ファイル={}、ファイル→フォルダ={}、フォルダ→ファイル={}",
        code(&onto_file),
        code(&onto_dir),
        code(&dir_onto_file)
    );
    assert_eq!(raw(onto_file), Some(183));
    assert_eq!(raw(onto_dir), Some(183));
    assert_eq!(raw(dir_onto_file), Some(183));
    drop((a, e));
    assert_eq!(read_text(&t.join("a.txt")), "a");
    assert_eq!(read_text(&t.join("b.txt")), "b");
    assert!(t.join("d").is_dir());
    assert!(t.join("e").is_dir());
}

/// R4 の場面。
#[derive(Clone, Copy, Debug)]
enum ReplaceCase {
    /// 移す先はふつうのファイル。
    Plain,
    /// 移す先を、ほかのハンドルが FILE_SHARE_DELETE 付きで開いている。
    OpenWithShareDelete,
    /// 移す先に読み取り専用の印。
    ReadOnly,
    /// 移す先がフォルダ。
    Folder,
}

/// R4 の1回：`dir` に src.txt（"new"）と移す先（dst.txt）を用意し、std か os-sys で置き換える。
/// 返すのは、結果と、そのあとの dst.txt の中身（ファイルなら）。
fn replace_once(dir: &Path, case: ReplaceCase, use_std: bool) -> (String, Option<String>) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("src.txt"), b"new").unwrap();
    let dst = dir.join("dst.txt");
    if matches!(case, ReplaceCase::Folder) {
        fs::create_dir(&dst).unwrap();
    } else {
        fs::write(&dst, b"old").unwrap();
    }
    if matches!(case, ReplaceCase::ReadOnly) {
        set_read_only(&dst, true);
    }
    let holder = matches!(case, ReplaceCase::OpenWithShareDelete).then(|| {
        OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2 | 0x4)
            .open(&dst)
            .unwrap()
    });
    let result = if use_std {
        code(&fs::rename(dir.join("src.txt"), &dst))
    } else {
        let top = root(dir);
        let src = child_file(&top, "src.txt", TARGET_FILE);
        code(&rename_replace(&src, &top, &name("dst.txt")))
    };
    drop(holder);
    if matches!(case, ReplaceCase::ReadOnly) {
        set_read_only(&dst, false);
    }
    let content = dst.is_file().then(|| read_text(&dst));
    (result, content)
}

/// R4：置き換える rename（索引と vault.json の道）。ファイルは置き換えられ、移す先がフォルダなら Err。
/// ほかのハンドルが FILE_SHARE_DELETE 付きで開いているとき・読み取り専用の印のときの結果が、std::fs::rename と同じ。
#[test]
fn r4_replace_matches_std() {
    let t = TestDir::new("r4");
    for (i, case) in [
        ReplaceCase::Plain,
        ReplaceCase::OpenWithShareDelete,
        ReplaceCase::ReadOnly,
        ReplaceCase::Folder,
    ]
    .into_iter()
    .enumerate()
    {
        let theirs = replace_once(&t.join(format!("std-{i}")), case, true);
        let ours = replace_once(&t.join(format!("os-{i}")), case, false);
        println!("R4 {case:?}：std={theirs:?}、os-sys={ours:?}");
        assert_eq!(ours, theirs, "{case:?}");
        match case {
            ReplaceCase::Plain => assert_eq!(ours, ("ok".to_string(), Some("new".to_string()))),
            ReplaceCase::Folder => assert_ne!(ours.0, "ok"),
            _ => {}
        }
    }
}

/// R5：1文字と 255 文字（ASCII と日本語）の名前で、作成も rename もできる。
#[test]
fn r5_one_and_255_unit_names() {
    let t = TestDir::new("r5");
    for n in ["f", "d", "r", "src"] {
        fs::create_dir(t.join(n)).unwrap();
    }
    let top = root(t.path());
    let f = child_dir(&top, "f", HOLD_DIR);
    let d = child_dir(&top, "d", HOLD_DIR);
    let r = child_dir(&top, "r", HOLD_DIR);
    let src = child_dir(&top, "src", HOLD_DIR);
    let names = ["a".to_string(), "a".repeat(255), "あ".repeat(255)];
    for (i, n) in names.iter().enumerate() {
        let n = name(n);
        drop(create_file_child(&f, &n, TMP_CREATE, Share::READ).unwrap());
        drop(create_dir_child(&d, &n, CREATE_DIR, SHARE_RW).unwrap());
        let x = create_file_child(&src, &name(&format!("x{i}")), TMP_CREATE, Share::READ).unwrap();
        rename_no_replace(x.get(), &r, &n).unwrap();
    }
    for dir in ["f", "d", "r"] {
        let mut got: Vec<OsString> = fs::read_dir(t.join(dir))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        got.sort();
        let mut want: Vec<OsString> = names.iter().map(OsString::from).collect();
        want.sort();
        assert_eq!(got, want, "{dir}");
    }
    println!("R5：1文字・255 文字（ASCII・日本語）の名前で、作る・rename ができた。ok");
}

// ---------------------------------------------------------------------------
// R6 持つと名前を変えられない
// ---------------------------------------------------------------------------

/// R6a で持つフォルダ（HOLD_DIR か PassDir。落とすまで持つ）。
#[allow(dead_code)]
enum Holder {
    Hold(Dir),
    Pass(PassDir),
}

/// R6a（必ず通る）：HOLD_DIR か PassDir で開いたフォルダは、fs::rename・ハンドルの rename・消すが、どれも Err。
#[test]
fn r6a_held_dir_cannot_be_renamed_or_deleted() {
    let t = TestDir::new("r6a");
    fs::create_dir(t.join("dest")).unwrap();
    let top = root(t.path());
    let dest = child_dir(&top, "dest", HOLD_DIR);
    for label in ["HOLD_DIR", "PassDir"] {
        let (h, e) = (format!("h-{label}"), format!("e-{label}"));
        fs::create_dir(t.join(&h)).unwrap();
        fs::write(t.join(&h).join("x.txt"), b"x").unwrap();
        fs::create_dir(t.join(&e)).unwrap();
        let holders = if label == "HOLD_DIR" {
            [
                Holder::Hold(child_dir(&top, &h, HOLD_DIR)),
                Holder::Hold(child_dir(&top, &e, HOLD_DIR)),
            ]
        } else {
            [Holder::Pass(pass(&top, &h)), Holder::Pass(pass(&top, &e))]
        };

        let std_rename = fs::rename(t.join(&h), t.join(format!("{h}2")));
        let open_for_rename = open_child(&top, &name(&h), Want::Any, TARGET_DIR, SHARE_RW);
        let handle_rename = match &open_for_rename {
            Ok(Opened::Dir(d)) => code(&rename_no_replace(d.as_file(), &dest, &name(&h))),
            _ => "（DELETE で開けないので rename まで行かない）".to_string(),
        };
        let std_remove = fs::remove_dir(t.join(&e));
        let open_for_delete = open_child(&top, &name(&e), Want::Any, Access::DELETE, SHARE_RW);
        println!(
            "R6a {label} で持つ：fs::rename={}、DELETE で開く={}、ハンドルの rename={}、fs::remove_dir（空）={}、消すために DELETE で開く={}",
            code(&std_rename),
            code(&open_for_rename),
            handle_rename,
            code(&std_remove),
            code(&open_for_delete)
        );
        assert!(std_rename.is_err(), "{label}");
        assert!(open_for_rename.is_err(), "{label}");
        assert!(std_remove.is_err(), "{label}");
        assert!(open_for_delete.is_err(), "{label}");
        drop(holders);
        assert!(t.join(&h).join("x.txt").is_file());
        assert!(t.join(&e).is_dir());
    }
}

/// R6f（必ず通る）：自分の TARGET_DIR のハンドルで列挙し、下のフォルダを HOLD_DIR で開いて閉じたあと、
/// その TARGET_DIR のハンドルで rename できる（列挙してから移す道）。
#[test]
fn r6f_enumerate_then_rename_with_target_dir() {
    let t = TestDir::new("r6f");
    fs::create_dir_all(t.join("m").join("s1")).unwrap();
    fs::create_dir_all(t.join("m").join("s2")).unwrap();
    fs::write(t.join("m").join("s1").join("x.txt"), b"x").unwrap();
    fs::write(t.join("m").join("f.txt"), b"f").unwrap();
    fs::create_dir(t.join("dest")).unwrap();
    let top = root(t.path());
    let dest = child_dir(&top, "dest", HOLD_DIR);
    let mut m = child_target_dir(&top, "m");
    let entries = read_dir_by_handle(&mut m).unwrap();
    assert_eq!(entries.len(), 3);
    for entry in &entries {
        if entry.kind() == EntryKind::Dir {
            let sub = Name::new(entry.name()).unwrap();
            drop(open_child(&m, &sub, Want::Dir, HOLD_DIR, SHARE_RW).unwrap());
        }
    }
    let result = rename_no_replace(m.as_file(), &dest, &name("m"));
    println!(
        "R6f：列挙して下を開いて閉じたあとの rename={}",
        code(&result)
    );
    result.unwrap();
    drop(m);
    assert!(t.join("dest").join("m").join("s1").join("x.txt").is_file());
}

/// R6b・R6c（記録）：READ_ATTRIBUTES だけ（と SYNCHRONIZE）、TRAVERSE・READ_ATTRIBUTES だけ、
/// LIST_DIRECTORY・READ_ATTRIBUTES だけで持ったフォルダの名前を変えられるか。
#[test]
fn r6bc_hold_with_fewer_rights_record() {
    let t = TestDir::new("r6bc");
    let cases = [
        ("R6b READ_ATTRIBUTES だけ", Access::READ_ATTRIBUTES),
        (
            "R6c TRAVERSE・READ_ATTRIBUTES",
            Access::TRAVERSE | Access::READ_ATTRIBUTES,
        ),
        (
            "R6c LIST_DIRECTORY・READ_ATTRIBUTES",
            Access::LIST_DIRECTORY | Access::READ_ATTRIBUTES,
        ),
        ("（比べ）HOLD_DIR", HOLD_DIR),
    ];
    let top = root(t.path());
    for (i, (label, access)) in cases.into_iter().enumerate() {
        let n = format!("h{i}");
        fs::create_dir(t.join(&n)).unwrap();
        let held = child_dir(&top, &n, access);
        let renamed = fs::rename(t.join(&n), t.join(format!("{n}-renamed")));
        let open_delete = open_child(&top, &name(&n), Want::Any, Access::DELETE, SHARE_RW);
        println!(
            "{label} で持つ：fs::rename={}、DELETE で開く={}",
            code(&renamed),
            code(&open_delete)
        );
        drop((held, open_delete));
    }
}

/// R6d・R6e（記録）：持っているフォルダの祖先の名前を変えられるか。下のもの（ファイル・フォルダ）を
/// ほかのハンドルが持っているフォルダの名前を、DELETE のハンドルで変えられるか。
#[test]
fn r6de_ancestor_and_parent_of_held_record() {
    let t = TestDir::new("r6de");
    fs::create_dir_all(t.join("a").join("b")).unwrap();
    fs::create_dir_all(t.join("pf")).unwrap();
    fs::write(t.join("pf").join("c.txt"), b"c").unwrap();
    fs::create_dir_all(t.join("pd").join("k")).unwrap();
    let top = root(t.path());

    // R6d：a\b を HOLD_DIR で持ち、祖先 a の名前を変える
    let a_for_b = child_dir(&top, "a", HOLD_DIR);
    let b = child_dir(&a_for_b, "b", HOLD_DIR);
    drop(a_for_b);
    let std_rename = fs::rename(t.join("a"), t.join("a2"));
    let handle_rename = match open_child(&top, &name("a"), Want::Any, TARGET_DIR, SHARE_RW) {
        Ok(Opened::Dir(a)) => code(&rename_no_replace(a.as_file(), &top, &name("a3"))),
        other => format!("DELETE で開けない：{}", code(&other)),
    };
    println!(
        "R6d 下の b を HOLD_DIR で持つとき、祖先 a の名前の変更：fs::rename={}、ハンドルの rename={}",
        code(&std_rename),
        handle_rename
    );
    drop(b);

    // R6e：下のファイルを std（共有は READ・WRITE・DELETE）で開いたまま、親 pf の名前を DELETE のハンドルで変える
    let c = File::open(t.join("pf").join("c.txt")).unwrap();
    let file_case = match open_child(&top, &name("pf"), Want::Any, TARGET_DIR, SHARE_RW) {
        Ok(Opened::Dir(p)) => code(&rename_no_replace(p.as_file(), &top, &name("pf2"))),
        other => format!("DELETE で開けない：{}", code(&other)),
    };
    drop(c);
    // R6e：下のフォルダを HOLD_DIR で持ったまま、親 pd の名前を DELETE のハンドルで変える
    let pd_for_k = child_dir(&top, "pd", HOLD_DIR);
    let k = child_dir(&pd_for_k, "k", HOLD_DIR);
    drop(pd_for_k);
    let dir_case = match open_child(&top, &name("pd"), Want::Any, TARGET_DIR, SHARE_RW) {
        Ok(Opened::Dir(p)) => code(&rename_no_replace(p.as_file(), &top, &name("pd2"))),
        other => format!("DELETE で開けない：{}", code(&other)),
    };
    drop(k);
    println!(
        "R6e 下のものを持たれているフォルダの、DELETE のハンドルでの名前の変更：下のファイル={file_case}、下のフォルダ={dir_case}"
    );
}

// ---------------------------------------------------------------------------
// R7 NtCreateFile（必ず通る。①③ は src のユニットテスト）
// ---------------------------------------------------------------------------

#[test]
fn r7_create_and_open_relative() {
    let t = TestDir::new("r7");
    let outside = TestDir::new("r7-outside");
    fs::create_dir(t.join("p")).unwrap();
    fs::write(t.join("p").join("f.txt"), b"f").unwrap();
    let has_junction = junction(&t.join("p").join("j"), outside.path());
    let top = root(t.path());
    let p = child_dir(&top, "p", HOLD_DIR);

    // 作る（フォルダ・ファイル）。作ったハンドルで最終のパスと ID を読める。ファイルは書いて sync できる
    let d = create_dir_child(&p, &name("nd"), CREATE_DIR, SHARE_RW).unwrap();
    assert_eq!(
        d.final_path().unwrap(),
        final_path(p.as_file()).unwrap().join("nd")
    );
    d.file_id().unwrap();
    let f = create_file_child(&p, &name("nf.txt"), TMP_CREATE, Share::READ).unwrap();
    let mut w: &File = f.get();
    w.write_all(b"hello").unwrap();
    w.sync_all().unwrap();
    assert_eq!(
        final_path(f.get()).unwrap(),
        final_path(p.as_file()).unwrap().join("nf.txt")
    );
    drop((d, f));
    assert!(t.join("p").join("nd").is_dir());
    assert_eq!(read_text(&t.join("p").join("nf.txt")), "hello");

    // 同じ名前があれば 183
    let again_dir = create_dir_child(&p, &name("nd"), CREATE_DIR, SHARE_RW);
    let again_file = create_file_child(&p, &name("nf.txt"), TMP_CREATE, Share::READ);
    let dir_over_file = create_dir_child(&p, &name("nf.txt"), CREATE_DIR, SHARE_RW);
    // フォルダを求めてファイルを開くと 267、ないものは 2
    let want_dir_on_file = open_child(&p, &name("f.txt"), Want::Dir, HOLD_DIR, SHARE_RW);
    let missing = open_child(&p, &name("none"), Want::Any, HOLD_DIR, SHARE_RW);
    println!(
        "R7：作り直し フォルダ={}・ファイル={}・ファイルの名前でフォルダ={}、フォルダを求めてファイル={}、ないもの={}",
        code(&again_dir),
        code(&again_file),
        code(&dir_over_file),
        code(&want_dir_on_file),
        code(&missing)
    );
    assert_eq!(raw(again_dir), Some(183));
    assert_eq!(raw(again_file), Some(183));
    assert_eq!(raw(dir_over_file), Some(183));
    assert_eq!(raw(want_dir_on_file), Some(267));
    assert_eq!(raw(missing), Some(2));

    // ジャンクションの名前を開くと、リンクそのもの（属性に REPARSE_POINT）
    if has_junction {
        for want in [Want::Any, Want::Dir] {
            match open_child(&p, &name("j"), want, HOLD_DIR, SHARE_RW).unwrap() {
                Opened::Link(link) => {
                    let attrs = link.metadata().unwrap().file_attributes();
                    assert_ne!(attrs & FILE_ATTRIBUTE_REPARSE_POINT, 0);
                    assert_eq!(
                        final_path(&link).unwrap(),
                        final_path(p.as_file()).unwrap().join("j"),
                        "リンクの先でなく、リンクそのもの"
                    );
                }
                _ => panic!("ジャンクションがリンクとして開かない（{want:?}）"),
            }
        }
        println!("R7：ジャンクションの名前は、リンクそのものとして開いた（FILE_OPENED）。ok");
    }
}

// ---------------------------------------------------------------------------
// R8〜R13
// ---------------------------------------------------------------------------

/// R8：NTFS で FileIdExtdDirectoryInfo が使え（ファイルの ID がある）、結果が std::fs::read_dir と同じ
/// （項目 2000 個・日本語・255 文字・ジャンクション）。
#[test]
fn r8_enumeration_matches_std() {
    let t = TestDir::new("r8");
    let outside = TestDir::new("r8-outside");
    for i in 0..2000 {
        fs::write(t.join(format!("f{i:04}.txt")), vec![b'x'; i % 7]).unwrap();
    }
    fs::write(t.join("日本語の名前.txt"), b"abc").unwrap();
    fs::write(t.join("a".repeat(255)), b"long").unwrap();
    fs::write(t.join("あ".repeat(255)), b"long").unwrap();
    fs::create_dir(t.join("sub")).unwrap();
    let has_junction = junction(&t.join("link"), outside.path());

    let mut dir = root(t.path());
    let ours = read_dir_by_handle(&mut dir).unwrap();
    let mut theirs: Vec<(OsString, fs::Metadata)> = fs::read_dir(t.path())
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name(), e.metadata().unwrap())
        })
        .collect();
    theirs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut ours_sorted = ours.clone();
    ours_sorted.sort_by(|a, b| a.name().cmp(b.name()));
    assert_eq!(ours_sorted.len(), 2000 + 4 + usize::from(has_junction));
    assert_eq!(ours_sorted.len(), theirs.len());
    let mut ids = std::collections::HashSet::new();
    for (e, (n, meta)) in ours_sorted.iter().zip(&theirs) {
        assert_eq!(e.name(), n);
        let ft = meta.file_type();
        let kind = if ft.is_symlink() {
            EntryKind::Link
        } else if ft.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        assert_eq!(e.kind(), kind, "{n:?}");
        assert_eq!(e.end_of_file(), meta.len(), "{n:?}");
        assert_eq!(e.attributes(), meta.file_attributes(), "{n:?}");
        let std_ns = match meta.modified().unwrap().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).unwrap(),
            Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap(),
        };
        assert_eq!(e.modified_unix_ns(), std_ns, "{n:?}");
        let id = e
            .file_id()
            .expect("FileIdExtdDirectoryInfo ならファイルの ID がある");
        assert!(ids.insert(id), "ファイルの ID が重なった");
    }
    // 2回めも頭から（Restart の種類）読む
    assert_eq!(read_dir_by_handle(&mut dir).unwrap().len(), ours.len());
    println!(
        "R8：FileIdExtdDirectoryInfo で {} 項目を読み、std の read_dir と一致（ジャンクション {}）",
        ours.len(),
        if has_junction {
            "あり"
        } else {
            "なし（作れない）"
        }
    );
}

/// R10：ファイル（読み取り専用の印のものも）と空のフォルダを消せる。空でないフォルダは 145 で残る。
#[test]
fn r10_delete_by_handle() {
    let t = TestDir::new("r10");
    fs::write(t.join("a.txt"), b"a").unwrap();
    fs::write(t.join("ro.txt"), b"ro").unwrap();
    set_read_only(&t.join("ro.txt"), true);
    fs::create_dir(t.join("e")).unwrap();
    fs::create_dir(t.join("n")).unwrap();
    fs::write(t.join("n").join("x.txt"), b"x").unwrap();
    let top = root(t.path());
    for f in ["a.txt", "ro.txt"] {
        let h = child_file(&top, f, TARGET_FILE);
        delete_by_handle(&h).unwrap();
        drop(h);
        assert!(!t.join(f).exists(), "{f}");
    }
    let e = child_target_dir(&top, "e");
    delete_by_handle(e.as_file()).unwrap();
    drop(e);
    assert!(!t.join("e").exists());
    let n = child_target_dir(&top, "n");
    let non_empty = delete_by_handle(n.as_file());
    println!(
        "R10：ファイル・読み取り専用・空のフォルダは消えた。空でないフォルダ={}",
        code(&non_empty)
    );
    assert_eq!(raw(non_empty), Some(145));
    drop(n);
    assert!(t.join("n").join("x.txt").is_file());
}

/// R11：空き容量は 0 より大きい。
#[test]
fn r11_free_space() {
    let t = TestDir::new("r11");
    let top = root(t.path());
    let free = free_space(&top).unwrap();
    println!("R11：空き容量 {} GiB", free >> 30);
    assert!(free > 0);
}

/// R12：OS の乱数。長さ 0・1・32・1 MiB。2回で違う値。0 ばかりでない。
#[test]
fn r12_os_random() {
    for len in [0usize, 1, 32, 1 << 20] {
        let mut buf = vec![0u8; len];
        os_random(&mut buf).unwrap();
        if len >= 32 {
            assert!(buf.iter().any(|&b| b != 0), "長さ {len}");
        }
    }
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    os_random(&mut a).unwrap();
    os_random(&mut b).unwrap();
    assert_ne!(a, b);
    println!("R12：0・1・32・1 MiB で取れ、2回で違う値。ok");
}

/// R13（記録）：外のファイルとハードリンクした中の名前を開くと、そのハンドルの最終のパスは中の名前か。
#[test]
fn r13_hard_link_final_path_record() {
    let t = TestDir::new("r13");
    let outside = TestDir::new("r13-outside");
    fs::create_dir(t.join("in")).unwrap();
    fs::write(outside.join("h.txt"), b"h").unwrap();
    fs::hard_link(outside.join("h.txt"), t.join("in").join("h.txt")).unwrap();
    let top = root(t.path());
    let inside = child_dir(&top, "in", HOLD_DIR);
    let opened = child_file(&inside, "h.txt", LEAF_READ);
    let got = final_path(&opened).unwrap();
    let inside_name = final_path(inside.as_file()).unwrap().join("h.txt");
    let outside_name = final_path(&File::open(outside.join("h.txt")).unwrap()).unwrap();
    let which = if got == inside_name {
        "中の名前"
    } else if got == outside_name {
        "外の名前"
    } else {
        "どちらでもない"
    };
    let mut content = String::new();
    (&opened).read_to_string(&mut content).unwrap();
    println!("R13：ハードリンクを中の名前で開いたハンドルの最終のパス＝{which}");
    assert_eq!(content, "h");
}

// ---------------------------------------------------------------------------
// R14 file_id（a〜d は必ず通る、e は記録）
// ---------------------------------------------------------------------------

const LEAF_ATTRS: Access = Access::READ_ATTRIBUTES;

/// R14a：同じものを別のハンドル（HOLD_DIR と LEAF_ATTRS）で開くと同じ値。
/// R14b：違うものは違う値。
#[test]
fn r14ab_same_object_same_id_different_object_different_id() {
    let t = TestDir::new("r14ab");
    fs::create_dir(t.join("d")).unwrap();
    fs::create_dir(t.join("e")).unwrap();
    fs::write(t.join("f.txt"), b"f").unwrap();
    fs::write(t.join("g.txt"), b"g").unwrap();
    let top = root(t.path());

    let d_hold = child_dir(&top, "d", HOLD_DIR);
    let d_attrs = match open_child(&top, &name("d"), Want::Any, LEAF_ATTRS, SHARE_RW).unwrap() {
        Opened::Dir(d) => d,
        _ => panic!("d がフォルダでない"),
    };
    let d1 = file_id(d_hold.as_file()).unwrap();
    let d2 = file_id(d_attrs.as_file()).unwrap();
    let f_read = child_file(&top, "f.txt", LEAF_READ);
    let f_attrs = child_file(&top, "f.txt", LEAF_ATTRS);
    let f1 = file_id(&f_read).unwrap();
    let f2 = file_id(&f_attrs).unwrap();
    let e = file_id(child_dir(&top, "e", HOLD_DIR).as_file()).unwrap();
    let g = file_id(&child_file(&top, "g.txt", LEAF_ATTRS)).unwrap();
    let r = file_id(top.as_file()).unwrap();
    println!(
        "R14a：フォルダ HOLD_DIR と LEAF_ATTRS で同じ={}、ファイル LEAF_READ と LEAF_ATTRS で同じ={}",
        d1 == d2,
        f1 == f2
    );
    assert_eq!(d1, d2);
    assert_eq!(f1, f2);
    let all = [d1, e, f1, g, r];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b, "違うものの ID が同じ");
        }
        assert_eq!(a.volume, r.volume, "同じボリューム");
    }
    println!("R14b：違う5つ（フォルダ2・ファイル2・根）の ID はすべて違う。ok");
}

/// R14c（fs::rename の分）：同じボリュームの中で、ファイルとフォルダを fs::rename しても ID は変わらない。
#[test]
fn r14c_id_survives_std_rename() {
    let t = TestDir::new("r14c");
    fs::create_dir(t.join("a")).unwrap();
    fs::create_dir(t.join("b")).unwrap();
    fs::write(t.join("a").join("f.txt"), b"f").unwrap();
    fs::create_dir(t.join("a").join("k")).unwrap();
    let top = root(t.path());
    let a = child_dir(&top, "a", HOLD_DIR);
    let b = child_dir(&top, "b", HOLD_DIR);

    let f_before = file_id(&child_file(&a, "f.txt", LEAF_ATTRS)).unwrap();
    let k_before = file_id(child_dir(&a, "k", HOLD_DIR).as_file()).unwrap();
    fs::rename(t.join("a").join("f.txt"), t.join("b").join("f2.txt")).unwrap();
    fs::rename(t.join("a").join("k"), t.join("b").join("k2")).unwrap();
    let f_after = file_id(&child_file(&b, "f2.txt", LEAF_ATTRS)).unwrap();
    let k_after = file_id(child_dir(&b, "k2", HOLD_DIR).as_file()).unwrap();
    println!(
        "R14c fs::rename：ファイル＝同じ {}、フォルダ＝同じ {}",
        f_before == f_after,
        k_before == k_after
    );
    assert_eq!(f_before, f_after);
    assert_eq!(k_before, k_after);
}

/// R14c（ハンドルの rename の分）：ハンドルで rename しても、同じハンドルと開き直したハンドルで ID は変わらない。
#[test]
fn r14c_id_survives_handle_rename() {
    let t = TestDir::new("r14c-handle");
    fs::create_dir(t.join("a")).unwrap();
    fs::create_dir(t.join("b")).unwrap();
    fs::write(t.join("a").join("f.txt"), b"f").unwrap();
    let top = root(t.path());
    let a = child_dir(&top, "a", HOLD_DIR);
    let b = child_dir(&top, "b", HOLD_DIR);

    let f = child_file(&a, "f.txt", TARGET_FILE);
    let before = file_id(&f).unwrap();
    rename_no_replace(&f, &b, &name("f2.txt")).unwrap();
    let same_handle = file_id(&f).unwrap();
    drop(f);
    let reopened = file_id(&child_file(&b, "f2.txt", LEAF_ATTRS)).unwrap();
    println!(
        "R14c ハンドルの rename：同じハンドルで同じ {}・開き直して同じ {}",
        before == same_handle,
        before == reopened
    );
    assert_eq!(before, same_handle);
    assert_eq!(before, reopened);
}

/// R14d：CREATE_DIR で作って閉じ、名前で HOLD_DIR で開き直すと同じ値。
#[test]
fn r14d_created_then_reopened_same_id() {
    let t = TestDir::new("r14d");
    let top = root(t.path());
    let created = create_dir_child(&top, &name("made"), CREATE_DIR, SHARE_RW).unwrap();
    let at_create = created.file_id().unwrap();
    drop(created);
    let reopened = child_dir(&top, "made", HOLD_DIR);
    let again = file_id(reopened.as_file()).unwrap();
    println!(
        "R14d：作ったときと開き直したときで同じ={}",
        at_create == again
    );
    assert_eq!(at_create, again);
}

/// R14e（記録）：ジャンクション（リンクそのもの）の ID と、その先のフォルダの ID。
#[test]
fn r14e_junction_id_record() {
    let t = TestDir::new("r14e");
    let outside = TestDir::new("r14e-outside");
    if !junction(&t.join("j"), outside.path()) {
        return;
    }
    let top = root(t.path());
    let link = match open_child(&top, &name("j"), Want::Any, LEAF_ATTRS, SHARE_RW).unwrap() {
        Opened::Link(l) => l,
        _ => panic!("ジャンクションがリンクとして開かない"),
    };
    let link_id = file_id(&link).unwrap();
    let target_id = file_id(root(outside.path()).as_file()).unwrap();
    println!(
        "R14e：ジャンクションそのものの ID と、その先のフォルダの ID は {}",
        if link_id == target_id {
            "同じ"
        } else {
            "違う"
        }
    );
}

// ---------------------------------------------------------------------------
// K：既知のフォルダ（W5。 のK0〜K3）
//
// 子プロセスのテストは、親の環境変数を変えない（Command の env・env_remove・env_clear で、子にだけ渡す）。
// 子の側（k_child_known_folders）は、OS_SYS_TEST_CHILD がないときは何もせずに通る。結果は、パスを出さずに
// 一致したかだけを書く。
// ---------------------------------------------------------------------------

/// K の子プロセスに渡す印（この変数があるときだけ、子の側のテストが動く。読むのはテストのコードの中だけ）。
const K_CHILD: &str = "OS_SYS_TEST_CHILD";
/// 親が求めた値を子に渡す変数の名前の頭（`OS_SYS_TEST_EXPECT_<番号>`。番号は KnownFolder::ALL の順）。
const K_EXPECT: &str = "OS_SYS_TEST_EXPECT_";

/// K1・K2 で替える・消すユーザーの変数（USERNAME・USERDOMAIN・HOMESHARE も）。
const K_USER_VARS: [&str; 12] = [
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "SystemDrive",
    "OneDrive",
    "OneDriveConsumer",
    "OneDriveCommercial",
    "USERNAME",
    "USERDOMAIN",
    "HOMESHARE",
];

/// K3 で替える・消す OS の変数。
const K_SYSTEM_VARS: [&str; 8] = [
    "SystemRoot",
    "windir",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    "ALLUSERSPROFILE",
    "PUBLIC",
];

/// パスが同じ場所か（Windows の比べ方：大文字小文字を問わず、終わりの区切りを除いて比べる）。
fn same_place(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.as_os_str()
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// K0（必ず）：8つの場所が Ok で絶対パス。環境変数を変えていないとき、7つは対応する環境変数と同じ場所
/// （変数がなければ飛ばす。FOLDERID の取り違えを捕まえる）。
#[test]
fn k0_known_folders_match_the_environment() {
    for folder in KnownFolder::ALL {
        let place = known_folder(folder);
        assert!(place.is_ok(), "K0 {folder:?}：{}", code(&place));
        assert!(
            place.unwrap().is_absolute(),
            "K0 {folder:?}：絶対パスでない"
        );
    }
    let pairs = [
        (KnownFolder::Profile, "USERPROFILE"),
        (KnownFolder::RoamingAppData, "APPDATA"),
        (KnownFolder::LocalAppData, "LOCALAPPDATA"),
        (KnownFolder::Windows, "SystemRoot"),
        (KnownFolder::ProgramFiles, "ProgramFiles"),
        (KnownFolder::ProgramFilesX86, "ProgramFiles(x86)"),
        (KnownFolder::ProgramData, "ProgramData"),
    ];
    for (folder, var) in pairs {
        let Some(value) = std::env::var_os(var) else {
            println!("K0 {folder:?}：{var} がないので飛ばした");
            continue;
        };
        let place = known_folder(folder).unwrap();
        assert!(
            same_place(&place, Path::new(&value)),
            "K0 {folder:?} が {var} と違う場所"
        );
        println!("K0 {folder:?}：{var} と一致");
    }
}

/// K の子の側：OS_SYS_TEST_CHILD があるときだけ、8つの場所を問い合わせ、親が渡した値と比べる。
#[test]
fn k_child_known_folders() {
    if std::env::var_os(K_CHILD).is_none() {
        return;
    }
    println!("K-CHILD-START");
    let mut differ = Vec::new();
    let mut recorded = Vec::new();
    for (i, folder) in KnownFolder::ALL.iter().enumerate() {
        let expected = std::env::var_os(format!("{K_EXPECT}{i}")).expect("親が渡す値");
        let place = known_folder(*folder);
        let found = match &place {
            Ok(p) if same_place(p, Path::new(&expected)) => continue,
            Ok(_) => format!("{folder:?}：違う場所"),
            // known_folder のエラーの文にパスは入らない
            Err(e) => format!("{folder:?}：{}（{e}）", code(&place)),
        };
        if *folder == K_RECORD_ONLY {
            recorded.push(found);
        } else {
            differ.push(found);
        }
    }
    let line = |items: &[String], all: &str| {
        if items.is_empty() {
            all.to_owned()
        } else {
            items.join("、")
        }
    };
    println!(
        "K-CHILD-RESULT {}（記録：{}）",
        line(&differ, "7つとも一致"),
        line(&recorded, "ProgramData も一致")
    );
    assert!(differ.is_empty(), "{differ:?}");
}

/// 環境変数で変わることが分かっている場所（記録だけ）：ProgramData は、自分のトークンを渡しても、場所の値の中の
/// `%SystemDrive%` をプロセスの環境変数で広げる（SystemDrive を替えると違う場所、消すと絶対パスでなくなり Err）。
/// 守り方は、あとで決める。
const K_RECORD_ONLY: KnownFolder = KnownFolder::ProgramData;

/// 子プロセスの結果。
enum ChildRun {
    /// 子の側のテストが動いた（成功したか、結果の行）。
    Ran(bool, String),
    /// 子の側のテストが始まらなかった（起動できない・テストの前に止まった）。
    NotStarted(String),
}

/// このテストの実行ファイルを子プロセスで起動し、`setup` で環境変数を変えて K の子の側を流す。
/// 親の値（8つ）は、子にだけ渡す。
fn k_run_child(setup: impl FnOnce(&mut Command)) -> ChildRun {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "k_child_known_folders",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ]);
    setup(&mut cmd);
    cmd.env(K_CHILD, "1");
    for (i, folder) in KnownFolder::ALL.iter().enumerate() {
        cmd.env(format!("{K_EXPECT}{i}"), known_folder(*folder).unwrap());
    }
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => return ChildRun::NotStarted(format!("起動できない：{}", code(&Err::<(), _>(e)))),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.contains("K-CHILD-START") {
        return ChildRun::NotStarted(format!(
            "テストが始まらなかった（終わりの値 {:?}）",
            out.status.code()
        ));
    }
    let result = stdout
        .lines()
        .find(|l| l.starts_with("K-CHILD-RESULT"))
        .unwrap_or("K-CHILD-RESULT （結果の行なし）")
        .to_owned();
    ChildRun::Ran(out.status.success(), result)
}

/// 子の側が動いて、ProgramData のほかの7つが親と同じだったことを確かめる（K1・K2・K3 の「必ず」）。
fn k_expect_same(label: &str, run: ChildRun) {
    match run {
        ChildRun::Ran(ok, result) => {
            println!("{label}：{result}");
            assert!(ok, "{label}：{result}");
        }
        ChildRun::NotStarted(why) => panic!("{label}：子の側が動かなかった：{why}"),
    }
}

/// K1（必ず・止める決まり S1）：子プロセスで、ユーザーの変数を偽のフォルダ（と偽の名前）に替えても、8つの場所は
/// 親と同じ（ProgramData は記録。K_RECORD_ONLY）。
#[test]
fn k1_user_variables_replaced() {
    let fake = TestDir::new("k1-fake");
    let fake_home = fake.join("home");
    fs::create_dir_all(fake_home.join("AppData").join("Roaming")).unwrap();
    fs::create_dir_all(fake_home.join("AppData").join("Local")).unwrap();
    let run = k_run_child(|cmd| {
        cmd.env("USERPROFILE", &fake_home)
            .env("HOMEDRIVE", "Q:")
            .env("HOMEPATH", r"\fake-home")
            .env("APPDATA", fake_home.join("AppData").join("Roaming"))
            .env("LOCALAPPDATA", fake_home.join("AppData").join("Local"))
            .env("SystemDrive", "Q:")
            .env("OneDrive", fake.join("onedrive"))
            .env("OneDriveConsumer", fake.join("onedrive"))
            .env("OneDriveCommercial", fake.join("onedrive"))
            .env("USERNAME", "fake-user")
            .env("USERDOMAIN", "FAKE-DOMAIN")
            .env("HOMESHARE", r"\\fake-server.invalid\fake-share");
    });
    k_expect_same("K1 ユーザーの変数を替える", run);
}

/// K1b（記録）：SystemDrive だけを替える・消す。ProgramData が変わる（記録の行に出る）のが、SystemDrive による
/// ことを確かめる。ほかの7つは親と同じ（必ず）。
#[test]
fn k1b_only_system_drive_changed() {
    let run = k_run_child(|cmd| {
        cmd.env("SystemDrive", "Q:");
    });
    k_expect_same("K1b SystemDrive だけを替える", run);
    let run = k_run_child(|cmd| {
        cmd.env_remove("SystemDrive");
    });
    k_expect_same("K1b SystemDrive だけを消す", run);
}

/// K2（必ず・止める決まり S1）：子プロセスで、ユーザーの変数を消しても、8つの場所は親と同じ（ProgramData は記録）。
#[test]
fn k2_user_variables_removed() {
    let run = k_run_child(|cmd| {
        for var in K_USER_VARS {
            cmd.env_remove(var);
        }
    });
    k_expect_same("K2 ユーザーの変数を消す", run);
}

/// K3（子が起動できれば必ず）：OS の変数（SystemRoot・windir・ProgramFiles など）を替える・消す、と環境変数を
/// すべて消す（env_clear）。子の側が動かなかったときは記録だけ。
#[test]
fn k3_system_variables_replaced_removed_and_cleared() {
    let fake = TestDir::new("k3-fake");
    type Setup<'a> = Box<dyn FnOnce(&mut Command) + 'a>;
    let cases: [(&str, Setup<'_>); 3] = [
        (
            "K3 OS の変数を替える",
            Box::new(|cmd: &mut Command| {
                for var in K_SYSTEM_VARS {
                    cmd.env(var, fake.join(var.replace(['(', ')'], "")));
                }
            }),
        ),
        (
            "K3 OS の変数を消す",
            Box::new(|cmd: &mut Command| {
                for var in K_SYSTEM_VARS {
                    cmd.env_remove(var);
                }
            }),
        ),
        (
            "K3 環境変数をすべて消す",
            Box::new(|cmd: &mut Command| {
                cmd.env_clear();
            }),
        ),
    ];
    for (label, setup) in cases {
        match k_run_child(setup) {
            ChildRun::NotStarted(why) => println!("{label}：子の側が動かなかった（記録）：{why}"),
            run => k_expect_same(label, run),
        }
    }
}

// ---------------------------------------------------------------------------
// R20（ACL の2つめの門の開き方。止める決まり S2）
// ---------------------------------------------------------------------------

/// 利用者のファイルを上書き・移動・削除・置き換えるときの2つめの門の開き方（vault-core の guard の TARGET_FILE_W と
/// 同じ値）。
const TARGET_FILE_W: Access = Access::DELETE
    .with(Access::WRITE_DATA)
    .with(Access::READ_ATTRIBUTES);

/// ファイルの値：中身・大きさ・更新時刻・作成時刻・属性（std で読む）と、`fsutil file layout` の変更時刻
/// （ChangeTime。秒まで）と LastUsn（変われば必ず増える番号）。fsutil を使えないときは None。
#[derive(Debug, PartialEq)]
struct FileState {
    content: Vec<u8>,
    len: u64,
    last_write_time: u64,
    creation_time: u64,
    attributes: u32,
    layout: Option<(String, String)>,
}

/// `fsutil file layout` の、変更時刻の行と LastUsn の行（LastUsn の行のすぐ前が変更時刻。言語に依らない）。
fn layout(path: &Path) -> Option<(String, String)> {
    let out = Command::new("fsutil")
        .args(["file", "layout"])
        .arg(cmd_path(path))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = decode(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    let usn = lines
        .iter()
        .position(|l| l.trim_start().starts_with("LastUsn"))?;
    let change = lines.get(usn.checked_sub(1)?)?;
    Some((change.trim().to_owned(), lines[usn].trim().to_owned()))
}

fn file_state(path: &Path) -> FileState {
    // 先に fsutil で読む（中身を読む前。読むことで値が変わらないことは、下の「開かずに2回」で確かめる）
    let layout = layout(path);
    let meta = fs::metadata(path).unwrap();
    FileState {
        content: fs::read(path).unwrap(),
        len: meta.len(),
        last_write_time: meta.last_write_time(),
        creation_time: meta.creation_time(),
        attributes: meta.file_attributes(),
        layout,
    }
}

/// R20（必ず。止める決まり S2）：利用者のファイルを DELETE・WRITE_DATA・READ_ATTRIBUTES（共有 READ・WRITE）で開いて
/// 閉じても、中身・大きさ・更新時刻・作成時刻・属性（アーカイブの印）・変更時刻（ChangeTime）・LastUsn が変わらない
/// fsutil を使えないときは失敗にする（確かめずに通らないように）。fsutil のない
/// 環境では、環境変数 `OS_SYS_R20_NO_FSUTIL=1` を付けたときだけ、変更時刻と LastUsn を確かめずに進め、そのことを
/// 出力に書く。変更時刻は秒までなので、開く前と閉じたあとに少し待つ。
#[test]
fn r20_opening_for_write_and_closing_changes_nothing() {
    let dir = TestDir::new("r20");
    let path = dir.join("f.txt");
    fs::write(&path, "hello").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let before = file_state(&path);
    if before.layout.is_none() {
        assert!(
            std::env::var_os("OS_SYS_R20_NO_FSUTIL").as_deref() == Some(OsStr::new("1")),
            "fsutil file layout を使えない（fsutil のない環境では OS_SYS_R20_NO_FSUTIL=1 を付けて動かす。そのときは変更時刻と LastUsn を確かめない）"
        );
    }
    // 開かずにもう一度読んでも同じ（読むことで値が変わらない＝比べ方の確かめ）
    assert_eq!(file_state(&path), before, "読むだけで値が変わった");
    let parent = root(dir.path());
    let opened = open_child(&parent, &name("f.txt"), Want::Any, TARGET_FILE_W, SHARE_RW);
    assert!(opened.is_ok(), "開けない：{}", code(&opened));
    drop(opened);
    drop(parent);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let after = file_state(&path);
    match &before.layout {
        Some(_) => println!("R20：fsutil で変更時刻と LastUsn も比べた"),
        None => println!(
            "R20：fsutil file layout を使えない（OS_SYS_R20_NO_FSUTIL=1）ので、変更時刻と LastUsn は確かめていない"
        ),
    }
    assert_eq!(after, before, "開いて閉じただけで値が変わった");
}

/// R20（記録）：TARGET_FILE_W で開けないもの。(WD) の拒否・読み取り専用の印・READ・DELETE の共有で開かれたファイル・
/// 動いているテストの exe・(D) の拒否・(DE) の拒否（親の権利で消せるか）。icacls の (D) は DELETE と SYNCHRONIZE の
/// 拒否で、(DE) は DELETE だけの拒否。
#[test]
fn r20_denied_and_busy_record() {
    let dir = TestDir::new("r20-record");
    let parent = root(dir.path());
    let open_w = |n: &str| {
        code(&open_child(
            &parent,
            &name(n),
            Want::Any,
            TARGET_FILE_W,
            SHARE_RW,
        ))
    };

    fs::write(dir.join("wd.txt"), "x").unwrap();
    let deny = DenyAce::new(&dir.join("wd.txt"), "(WD)");
    println!("R20 (WD) の拒否：{}", open_w("wd.txt"));
    drop(deny);

    fs::write(dir.join("ro.txt"), "x").unwrap();
    set_read_only(&dir.join("ro.txt"), true);
    println!("R20 読み取り専用の印：{}", open_w("ro.txt"));
    set_read_only(&dir.join("ro.txt"), false);

    fs::write(dir.join("shared.txt"), "x").unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x4)
        .open(dir.join("shared.txt"))
        .unwrap();
    println!(
        "R20 READ・DELETE の共有で開かれている：{}",
        open_w("shared.txt")
    );
    drop(held);

    let exe = std::env::current_exe().unwrap();
    let exe_parent = root(exe.parent().unwrap());
    let exe_name = exe.file_name().unwrap().to_string_lossy().into_owned();
    println!(
        "R20 動いているテストの exe：{}",
        code(&open_child(
            &exe_parent,
            &name(&exe_name),
            Want::Any,
            TARGET_FILE_W,
            SHARE_RW
        ))
    );

    fs::write(dir.join("d.txt"), "x").unwrap();
    let deny = DenyAce::new(&dir.join("d.txt"), "(D)");
    println!(
        "R20 (D) の拒否（親の権利で開けるか）：TARGET_FILE_W {}・DELETE だけ {}",
        open_w("d.txt"),
        code(&open_child(
            &parent,
            &name("d.txt"),
            Want::Any,
            Access::DELETE.with(Access::READ_ATTRIBUTES),
            SHARE_RW
        ))
    );
    drop(deny);

    fs::write(dir.join("de.txt"), "x").unwrap();
    let deny = DenyAce::new(&dir.join("de.txt"), "(DE)");
    println!(
        "R20 (DE) の拒否（親の権利で開けるか）：TARGET_FILE_W {}・DELETE だけ {}",
        open_w("de.txt"),
        code(&open_child(
            &parent,
            &name("de.txt"),
            Want::Any,
            Access::DELETE.with(Access::READ_ATTRIBUTES),
            SHARE_RW
        ))
    );
    drop(deny);
}
