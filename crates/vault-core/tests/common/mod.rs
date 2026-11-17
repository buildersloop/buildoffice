//! 結合テストの共通の道具。保管庫は `env!("CARGO_TARGET_TMPDIR")` の下にだけ作る
//! （本物のホームや %TEMP% では init も open も呼ばない）。
//! テストで作ったものは、`TestDir` の Drop で片づける（`CARGO_TARGET_TMPDIR/vault-core-tests` の下だけ）。

// テストのファイルごとに使う道具が違うので、使わないものがあっても警告にしない
#![allow(dead_code)]

use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use vault_core::{Actor, ErrorCode, RootEnv, Vault, VaultError};

/// テストの保管庫を置く場所。
pub fn tests_base() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("vault-core-tests")
}

/// テストごとのフォルダ（`<tests_base>/<名前>-<pid>-<時刻>-<連番>`）。Drop で中身ごと消す。
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new(name: &str) -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = tests_base().join(format!(
            "{name}-{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        // テストの場所の下だけを消す（std の remove_dir_all は、リンク・ジャンクションの先をたどらない）
        let base = tests_base();
        if self.path.starts_with(&base) && self.path != base {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// この PC の環境変数から作った RootEnv（根の判定を外さない）。
pub fn env() -> RootEnv {
    RootEnv::from_system().expect("RootEnv を作れない")
}

/// テストで使う actor（`{kind:"user", id:"test"}`）。
pub fn actor() -> Actor {
    Actor::user("test")
}

/// ツールを呼ぶ（`api::call`）。
pub fn call(
    vault: &Vault,
    tool: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, VaultError> {
    vault_core::api::call(vault, tool, &args, &actor())
}

/// 保管庫の履歴をすべて読む（ファイルの順・行の順）。読めない行は飛ばす。
pub fn journal(vault_path: &Path) -> Vec<vault_core::JournalEntry> {
    let dir = vault_path.join(".vault").join("journal");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let mut entries = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for line in text.lines() {
            if let Ok(entry) = serde_json::from_str(line) {
                entries.push(entry);
            }
        }
    }
    entries
}

/// ゴミ箱の entry（trash_id → entry.json の中身）。
pub fn trash_entry(vault_path: &Path, trash_id: &str) -> vault_core::TrashEntry {
    let path = vault_path
        .join(".vault")
        .join("trash")
        .join(trash_id)
        .join("entry.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// `dir` の下に新しい保管庫 `name` を作って、書ける状態で開く。
pub fn new_vault(dir: &TestDir, name: &str) -> (PathBuf, Vault) {
    let path = dir.join(name);
    Vault::init(&path, &env(), &actor()).unwrap_or_else(|e| panic!("init: {e}"));
    let vault = Vault::open(&path, false, &env()).unwrap_or_else(|e| panic!("open: {e}"));
    (path, vault)
}

/// エラーの種類を確かめ、エラーの文に絶対パス（テストの場所）が入っていないことも確かめる。
pub fn expect_code<T: Debug>(result: Result<T, VaultError>, code: ErrorCode, what: &str) {
    match result {
        Ok(v) => panic!("{what}: {code} になるべきところが成功した（{v:?}）"),
        Err(e) => {
            assert_eq!(e.code(), code, "{what}: {e}");
            assert_no_absolute_path(&e);
        }
    }
}

/// エラーの文に、テストの場所の絶対パスが入っていないこと。
pub fn assert_no_absolute_path(e: &VaultError) {
    let text = e.to_string();
    for needle in [
        env!("CARGO_TARGET_TMPDIR"),
        "vault-core-tests",
        r"\\?\",
        "target",
    ] {
        assert!(!text.contains(needle), "エラーの文に絶対パスがある：{text}");
    }
}

/// 飛ばしたテストを出力する（`cargo test -- --nocapture` で見える）。
pub fn skip(test: &str, reason: &str) {
    eprintln!("SKIP {test}: {reason}");
}

/// `cmd /U /C ...` を動かして、終了コードと出力（UTF-16 を読んだ文字）を返す。起動できなければ失敗にする。
#[cfg(windows)]
pub fn run_cmd(args: &[&std::ffi::OsStr]) -> (Option<i32>, String) {
    let out = std::process::Command::new("cmd")
        .arg("/U")
        .arg("/C")
        .args(args)
        .output()
        .expect("cmd を起動できない");
    let mut text = decode_cmd_output(&out.stdout);
    text.push_str(&decode_cmd_output(&out.stderr));
    (out.status.code(), text)
}

/// `cmd /U` の出力（内部コマンドは UTF-16LE）を文字にする。UTF-16 でなければ UTF-8 として読む。
#[cfg(windows)]
fn decode_cmd_output(bytes: &[u8]) -> String {
    let (pairs, rest) = bytes.as_chunks::<2>();
    let looks_utf16 = rest.is_empty() && pairs.iter().any(|p| p[1] == 0);
    if looks_utf16 {
        let units: Vec<u16> = pairs.iter().map(|&p| u16::from_le_bytes(p)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// cmd の内部コマンド（mklink・dir）に渡すパス：絶対パスにし、`/` を `\` に直す。
/// 内部コマンドは、パスの中の `/` からあとをスイッチと読む（CARGO_TARGET_DIR を `target/xxx` のように
/// `/` 区切りの相対にすると、mklink が「パラメーターの形式が誤っています」で落ちた）。
#[cfg(windows)]
pub fn cmd_path(path: &Path) -> std::ffi::OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let absolute = std::path::absolute(path).unwrap_or_else(|e| panic!("絶対パスにできない：{e}"));
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
    std::ffi::OsString::from_wide(&wide)
}

/// ジャンクションを作る（`cmd /C mklink /J`。パスは `cmd_path` で直してから渡す）。
///
/// 作れたら、本当にリンクになったことも確かめて true。ボリュームがジャンクションに対応しないときだけ、
/// 終了コードと出力を出して false（テストを飛ばす）。それ以外の失敗は、テストの失敗にする。
#[cfg(windows)]
pub fn junction(link: &Path, target: &Path) -> bool {
    let (link_arg, target_arg) = (cmd_path(link), cmd_path(target));
    let (code, text) = run_cmd(&[
        "mklink".as_ref(),
        "/J".as_ref(),
        link_arg.as_os_str(),
        target_arg.as_os_str(),
    ]);
    if code == Some(0) {
        let meta = fs::symlink_metadata(link)
            .unwrap_or_else(|e| panic!("mklink /J は成功したのに、リンクがない：{e}"));
        assert!(
            meta.file_type().is_symlink(),
            "mklink /J は成功したのに、ジャンクションになっていない"
        );
        return true;
    }
    let lower = text.to_lowercase();
    if text.contains("NTFS")
        || text.contains("サポートされていません")
        || lower.contains("not supported")
    {
        skip(
            "junction",
            &format!(
                "このボリュームはジャンクションに対応しない（終了コード {code:?}）：{}",
                text.trim()
            ),
        );
        return false;
    }
    panic!(
        "mklink /J が失敗した（終了コード {code:?}）：{}",
        text.trim()
    );
}

/// `dir /x` で読んだ短い名前。
#[derive(Debug, PartialEq, Eq)]
pub enum ShortName {
    /// 短い名前がある。
    Name(String),
    /// 一覧には出たが、短い名前の欄が空（このボリュームでは短い名前が作られない）。
    None,
}

/// `dir /x /a` で、フォルダの中の `long` の短い名前を取る。
/// `dir` が失敗したとき、一覧に `long` が出なかったときは、テストの失敗にする（「作れない」と取り違えない）。
#[cfg(windows)]
pub fn short_name(dir: &Path, long: &str) -> ShortName {
    let dir_arg = cmd_path(dir);
    let (code, text) = run_cmd(&[
        "dir".as_ref(),
        "/x".as_ref(),
        "/a".as_ref(),
        dir_arg.as_os_str(),
    ]);
    assert_eq!(code, Some(0), "dir /x が失敗した：{}", text.trim());
    match parse_short_name(&text, long) {
        Some(Some(name)) => ShortName::Name(name),
        Some(None) => ShortName::None,
        None => panic!("dir /x の出力に {long:?} がない：\n{text}"),
    }
}

/// `dir /x` の出力から、`long` の行の短い名前を読む。
/// 行がなければ None、行はあるが短い名前の欄が空なら Some(None)。
pub fn parse_short_name(text: &str, long: &str) -> Option<Option<String>> {
    for line in text.lines() {
        let Some(before) = line.trim_end().strip_suffix(long) else {
            continue;
        };
        if !before.ends_with(' ') {
            continue; // 別の名前の末尾が一致しただけ
        }
        let last = before.split_whitespace().last()?;
        return Some(last.contains('~').then(|| last.to_owned()));
    }
    None
}

/// フォルダの「子のフォルダ」の中に、フォルダを作れないようにする（Everyone に、継承だけの AD の拒否を付ける）。
/// Drop で外す。icacls が失敗したら、テストの失敗にする。
#[cfg(windows)]
pub struct DenySubdirsInChildren {
    path: PathBuf,
}

#[cfg(windows)]
impl DenySubdirsInChildren {
    const ACE: &str = "*S-1-1-0:(CI)(IO)(AD)";

    pub fn new(path: &Path) -> Self {
        let out = std::process::Command::new("icacls")
            .arg(path)
            .arg("/deny")
            .arg(Self::ACE)
            .output()
            .expect("icacls を起動できない");
        assert!(
            out.status.success(),
            "icacls /deny が失敗した（終了コード {:?}）：{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout)
        );
        Self {
            path: path.to_path_buf(),
        }
    }
}

#[cfg(windows)]
impl Drop for DenySubdirsInChildren {
    fn drop(&mut self) {
        let _ = std::process::Command::new("icacls")
            .arg(&self.path)
            .arg("/remove:d")
            .arg("*S-1-1-0")
            .output();
    }
}

/// フォルダに、Everyone への拒否（icacls の形。例：`(WD)` ＝ ファイルを作れない）を付ける。Drop で外す。
/// icacls が失敗したら、テストの失敗にする。
#[cfg(windows)]
pub struct DenyAce {
    path: PathBuf,
}

#[cfg(windows)]
impl DenyAce {
    pub fn new(path: &Path, rights: &str) -> Self {
        let out = std::process::Command::new("icacls")
            .arg(path)
            .arg("/deny")
            .arg(format!("*S-1-1-0:{rights}"))
            .output()
            .expect("icacls を起動できない");
        assert!(
            out.status.success(),
            "icacls /deny が失敗した（終了コード {:?}）：{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout)
        );
        Self {
            path: path.to_path_buf(),
        }
    }
}

#[cfg(windows)]
impl Drop for DenyAce {
    fn drop(&mut self) {
        let _ = std::process::Command::new("icacls")
            .arg(&self.path)
            .arg("/remove:d")
            .arg("*S-1-1-0")
            .output();
    }
}

/// ファイルの読み取り専用の印を付ける・外す。
pub fn set_read_only(path: &Path, read_only: bool) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(read_only);
    fs::set_permissions(path, perms).unwrap();
}

/// 保管庫の根の長さ（先頭の \\?\ を除いた形）を、UTF-16 で数える。
pub fn root_len(vault_path: &Path) -> usize {
    let canon = fs::canonicalize(vault_path).unwrap();
    let text = canon.to_string_lossy().into_owned();
    let shown = text.strip_prefix(r"\\?\").unwrap_or(&text);
    shown.encode_utf16().count()
}
