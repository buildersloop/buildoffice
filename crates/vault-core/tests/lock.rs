//! 書き込みのロック。

mod common;

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use common::{TestDir, env, expect_code, new_vault};
use vault_core::{ErrorCode, Vault};

#[test]
fn another_handle_gets_locked_while_the_lock_is_held() {
    let dir = TestDir::new("lock-two");
    let (path, a) = new_vault(&dir, "v");
    let mut b = Vault::open(&path, false, &env()).unwrap();
    // 時間切れを短く差し替える
    let timeout = Duration::from_millis(300);
    b.set_lock_timeout(timeout);

    let guard = a.lock().unwrap();
    let started = Instant::now();
    expect_code(b.lock(), ErrorCode::Locked, "ほかのハンドルが持っている");
    let waited = started.elapsed();
    assert!(waited >= timeout, "待った時間 {waited:?}");
    assert!(waited < Duration::from_secs(5), "待った時間 {waited:?}");

    // 外したあとは取れる
    drop(guard);
    let _guard = b.lock().unwrap();
}

#[test]
fn the_same_vault_opens_a_new_handle_each_time() {
    let dir = TestDir::new("lock-same");
    let (_path, mut a) = new_vault(&dir, "v");
    a.set_lock_timeout(Duration::from_millis(100));
    let guard = a.lock().unwrap();
    // 同じ Vault から2回目を取ると、新しいハンドルなので、時間切れで LOCKED
    expect_code(a.lock(), ErrorCode::Locked, "同じ Vault から2回目");
    drop(guard);
    let _guard = a.lock().unwrap();
}

#[test]
fn a_waiting_lock_succeeds_when_the_holder_releases_it() {
    let dir = TestDir::new("lock-wait");
    let (path, a) = new_vault(&dir, "v");
    let mut b = Vault::open(&path, false, &env()).unwrap();
    b.set_lock_timeout(Duration::from_secs(5));

    let guard = a.lock().unwrap();
    let holder = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        drop(guard);
    });
    let started = Instant::now();
    let _guard = b.lock().unwrap();
    assert!(started.elapsed() >= Duration::from_millis(150));
    holder.join().unwrap();
}

#[test]
fn read_only_vaults_can_lock_and_a_missing_lock_file_is_created() {
    let dir = TestDir::new("lock-read-only");
    let (path, _a) = new_vault(&dir, "v");
    let lock_file = path.join(".vault").join("lock");
    fs::remove_file(&lock_file).unwrap();
    let ro = Vault::open(&path, true, &env()).unwrap();
    {
        let _guard = ro.lock().unwrap();
        assert!(lock_file.is_file());
    }
    // 作ったあとは、ある lock をそのまま使う
    let _guard = ro.lock().unwrap();
}

#[test]
fn a_lock_file_that_is_not_a_file_is_rejected() {
    let dir = TestDir::new("lock-not-file");
    let (path, a) = new_vault(&dir, "v");
    let lock_file = path.join(".vault").join("lock");
    fs::remove_file(&lock_file).unwrap();
    fs::create_dir(&lock_file).unwrap();
    expect_code(a.lock(), ErrorCode::NotAVault, "lock がフォルダ");
}

#[cfg(windows)]
#[test]
fn a_lock_file_that_is_a_junction_is_rejected() {
    use common::junction;

    let dir = TestDir::new("lock-junction");
    let (path, a) = new_vault(&dir, "v");
    let lock_file = path.join(".vault").join("lock");
    let outside = dir.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::remove_file(&lock_file).unwrap();
    if !junction(&lock_file, &outside) {
        return;
    }
    expect_code(a.lock(), ErrorCode::NotAVault, "lock がジャンクション");
    // リンクの先に何も作っていない
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

/// lock がないときは create_new で作るので、リンクの先がなくても、リンクの先にファイルを作らない。
#[cfg(any(windows, unix))]
#[test]
fn a_dangling_link_at_the_lock_file_is_not_followed() {
    use common::skip;

    let dir = TestDir::new("lock-dangling");
    let (path, a) = new_vault(&dir, "v");
    let lock_file = path.join(".vault").join("lock");
    let outside_file = dir.join("outside-lock");
    fs::remove_file(&lock_file).unwrap();

    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&outside_file, &lock_file);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside_file, &lock_file);

    if let Err(e) = made {
        if cfg!(windows) && e.raw_os_error() == Some(1314) {
            skip(
                "a_dangling_link_at_the_lock_file_is_not_followed",
                "シンボリックリンクを作る権限がない（OS のエラー 1314）",
            );
            return;
        }
        panic!("シンボリックリンクを作れない：{e}");
    }
    expect_code(a.lock(), ErrorCode::NotAVault, "lock が先のないリンク");
    assert!(!outside_file.exists(), "リンクの先にファイルを作らない");
}
