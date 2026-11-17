//! 根の判定が守る場所を、環境変数を差し替えられても本物の場所で決めること（E1・E1b・K6）。
//!
//! - 子プロセスのテストは、親の環境変数を変えない（Command の env・env_remove・env_clear で、子にだけ渡す）。
//! - 子の側（e_child_root_env）は、VAULT_CORE_TEST_CHILD がないときは何もせずに通る。テスト用の環境変数を読むのは、
//!   このファイルの中だけ（「根の判定を外す環境変数は作らない」。製品のコードは読まない）。
//! - 結果は、パスを出さずに、断ったか・通したかだけを書く。ファイルは作らない（check_root はファイルを読まない）。

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use vault_core::{ErrorCode, RootEnv, check_root};

/// 子の側を動かす印（値は動き方：`check` か `no-home`）。
const CHILD: &str = "VAULT_CORE_TEST_CHILD";
/// 本物のホーム（親の USERPROFILE）を子に渡す。
const REAL_HOME: &str = "VAULT_CORE_TEST_REAL_HOME";
/// 本物の ProgramData（親の SystemRoot のドライブ＋\ProgramData）を子に渡す。
const REAL_PROGRAM_DATA: &str = "VAULT_CORE_TEST_REAL_PROGRAMDATA";
/// 本物の Windows のフォルダ（親の SystemRoot）を子に渡す。
const REAL_WINDOWS: &str = "VAULT_CORE_TEST_REAL_WINDOWS";
/// 本物の Program Files（親の ProgramFiles）を子に渡す。
const REAL_PROGRAM_FILES: &str = "VAULT_CORE_TEST_REAL_PROGRAMFILES";

/// 子の側：VAULT_CORE_TEST_CHILD があるときだけ動く。
#[test]
fn e_child_root_env() {
    let Some(mode) = std::env::var_os(CHILD) else {
        return;
    };
    println!("E-CHILD-START");
    let env = RootEnv::from_system();
    if mode == "no-home" {
        let code = env.as_ref().err().map(vault_core::VaultError::code);
        println!("E-CHILD-RESULT from_system：{code:?}");
        assert_eq!(code, Some(ErrorCode::InvalidRoot));
        return;
    }
    let env = env.unwrap_or_else(|e| panic!("from_system が失敗した：{:?}", e.code()));
    let real = |name: &str| PathBuf::from(std::env::var_os(name).expect("親が渡す"));
    let home = real(REAL_HOME);
    let program_data = real(REAL_PROGRAM_DATA);
    let windows = real(REAL_WINDOWS);
    let program_files = real(REAL_PROGRAM_FILES);
    let cases: Vec<(&str, PathBuf, bool)> = vec![
        ("ホーム", home.clone(), false),
        (
            "ホームの親",
            home.parent().expect("ホームの親").to_path_buf(),
            false,
        ),
        (
            "ホーム\\AppData\\Roaming\\x",
            home.join("AppData").join("Roaming").join("x"),
            false,
        ),
        (
            "ホーム\\AppData\\Local\\x",
            home.join("AppData").join("Local").join("x"),
            false,
        ),
        ("ホーム\\.ssh", home.join(".ssh"), false),
        (
            "ホーム\\Documents\\PowerShell",
            home.join("Documents").join("PowerShell"),
            false,
        ),
        ("ホーム\\Vault", home.join("Vault"), true),
        ("ProgramData", program_data.clone(), false),
        ("ProgramData\\x", program_data.join("x"), false),
        ("ProgramFiles\\x", program_files.join("x"), false),
        ("Windows\\System32", windows.join("System32"), false),
    ];
    let mut wrong = Vec::new();
    for (label, path, ok) in cases {
        let got = check_root(&path, &env);
        let line = match &got {
            Ok(()) => "通した".to_owned(),
            Err(e) => format!("断った（{:?}）", e.code()),
        };
        println!("E-CHILD {label}：{line}");
        let right = match got {
            Ok(()) => ok,
            Err(e) => !ok && e.code() == ErrorCode::InvalidRoot,
        };
        if !right {
            wrong.push(label);
        }
    }
    println!(
        "E-CHILD-RESULT {}",
        if wrong.is_empty() {
            "どれも期待どおり".to_owned()
        } else {
            format!("期待と違う：{}", wrong.join("、"))
        }
    );
    assert!(wrong.is_empty(), "{wrong:?}");
}

/// このテストの実行ファイルを子プロセスで起動し、`setup` で環境変数を変えて子の側を `mode` で流す。
/// 子の側のテストが動いて成功したかと、結果の行を返す。
fn run_child(mode: &str, setup: impl FnOnce(&mut Command)) -> (bool, String) {
    let real_home = std::env::var_os("USERPROFILE").expect("親の USERPROFILE");
    let program_files = std::env::var_os("ProgramFiles").expect("親の ProgramFiles");
    let system_root = PathBuf::from(std::env::var_os("SystemRoot").expect("親の SystemRoot"));
    let mut drive = system_root
        .components()
        .next()
        .expect("ドライブ")
        .as_os_str()
        .to_owned();
    drive.push("\\ProgramData");
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "e_child_root_env",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ]);
    setup(&mut cmd);
    cmd.env(CHILD, mode)
        .env(REAL_HOME, &real_home)
        .env(REAL_PROGRAM_DATA, &drive)
        .env(REAL_WINDOWS, &system_root)
        .env(REAL_PROGRAM_FILES, &program_files);
    let out = cmd.output().expect("子を起動できない");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("E-CHILD-START"), "子の側が動かなかった");
    for line in stdout.lines().filter(|l| l.starts_with("E-CHILD ")) {
        println!("  {line}");
    }
    let result = stdout
        .lines()
        .find(|l| l.starts_with("E-CHILD-RESULT"))
        .unwrap_or("E-CHILD-RESULT （結果の行なし）")
        .to_owned();
    (out.status.success(), result)
}

/// ユーザーの変数（K1 と同じ 12 個）と、ProgramData・Windows・Program Files の変数を偽にする。
fn fake_variables(cmd: &mut Command) {
    let fake = Path::new(env!("CARGO_TARGET_TMPDIR")).join("root-env-fake");
    let home = fake.join("home");
    cmd.env("SystemRoot", fake.join("windows"))
        .env("windir", fake.join("windows"))
        .env("ProgramFiles", fake.join("pf"))
        .env("ProgramFiles(x86)", fake.join("pf86"))
        .env("ProgramW6432", fake.join("pf"));
    cmd.env("USERPROFILE", &home)
        .env("HOMEDRIVE", "Q:")
        .env("HOMEPATH", r"\fake-home")
        .env("APPDATA", home.join("AppData").join("Roaming"))
        .env("LOCALAPPDATA", home.join("AppData").join("Local"))
        .env("OneDrive", fake.join("onedrive"))
        .env("OneDriveConsumer", fake.join("onedrive"))
        .env("OneDriveCommercial", fake.join("onedrive"))
        .env("USERNAME", "fake-user")
        .env("USERDOMAIN", "FAKE-DOMAIN")
        .env("HOMESHARE", r"\\fake-server.invalid\fake-share")
        .env("ProgramData", fake.join("programdata"))
        .env("ALLUSERSPROFILE", fake.join("programdata"));
}

/// E1（以前のコードで落ちる）：子プロセスでユーザーの変数と OS の変数を偽にしても、本物の場所について
/// check_root が INVALID_ROOT（ホーム・ホームの親・ホーム\AppData\Roaming\x・ホーム\AppData\Local\x・ホーム\.ssh・
/// ホーム\Documents\PowerShell・<ProgramFiles>\x・<Windows>\System32 を断り、ホーム\Vault は通す）。SystemDrive を
/// 偽に替えても・消しても（known_folder(ProgramData) が Err でも）、本物の <Windows のドライブ>\ProgramData と
/// その下を断る（設計の決定）。
///
/// この PC の形で、ほかの値だけで断られるもの（OS の値の効き目は、ユニットテストで確かめる）：
/// - Windows が C: の PC では、ProgramData の2つと Windows・Program Files の下は、既定の値（C:\ProgramData など）
///   だけで断られる → root.rs の program_data_on_the_windows_drive_is_protected・os_values_are_added。
/// - リポジトリがホームの下にある PC では、偽のホーム（CARGO_TARGET_TMPDIR の下）が本物のホームの下になるので、
///   「ホーム」と「ホームの親」は、OS のホームがなくても（偽のホームの祖先として）断られる
///   → root.rs の u1_both_the_os_home_and_the_variable_home_are_protected・
///   u6_a_linked_home_protects_the_real_folder（と、8.3 の名前があるドライブでは
///   u6_short_name_home_is_checked_only_where_8dot3_names_exist）。
#[test]
fn e1_fake_user_variables_do_not_unprotect_the_real_places() {
    let (ok, result) = run_child("check", |cmd| {
        fake_variables(cmd);
        cmd.env("SystemDrive", "Q:");
    });
    println!("E1 ユーザーと OS の変数・SystemDrive を偽に替える：{result}");
    assert!(ok, "E1（SystemDrive を替える）：{result}");
    let (ok, result) = run_child("check", |cmd| {
        fake_variables(cmd);
        cmd.env_remove("SystemDrive");
    });
    println!("E1 ユーザーと OS の変数を偽にし、SystemDrive を消す：{result}");
    assert!(ok, "E1（SystemDrive を消す）：{result}");
}

/// E1b：環境変数をすべて消した子では、from_system が INVALID_ROOT（ホームの環境変数がないときは、OS のホームが
/// 取れても今までどおり断る。設計の決定）。
#[test]
fn e1b_cleared_environment_is_invalid_root() {
    let (ok, result) = run_child("no-home", |cmd| {
        cmd.env_clear();
    });
    println!("E1b 環境変数をすべて消す：{result}");
    assert!(ok, "E1b：{result}");
}

/// K6（記録）：RootEnv::from_system の時間（20 回の中央値）。
/// `cargo test --release -p vault-core --test root_env -- --ignored --nocapture k6` で流す。
#[test]
#[ignore = "記録（時間を測る）"]
fn k6_from_system_time() {
    let mut times: Vec<Duration> = (0..20)
        .map(|_| {
            let start = Instant::now();
            let env = RootEnv::from_system();
            let took = start.elapsed();
            assert!(env.is_ok());
            took
        })
        .collect();
    let first = times[0];
    times.sort();
    // 1 回目は、DLL の読み込みと初回の問い合わせを含む（起動の時間は、release の vault-mcp で別に測る）
    println!(
        "K6 RootEnv::from_system：中央値 {:?}（最小 {:?}・最大 {:?}・1 回目 {first:?}）",
        times[10], times[0], times[19]
    );
}
