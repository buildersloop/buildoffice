//! ユニットテストの道具（テストのビルドだけ）。
//!
//! ユニットテストには CARGO_TARGET_TMPDIR がないので、リポジトリの `target\tmp\os-sys-unit` の下にだけ作る
//! （本物のホームや %TEMP% には作らない）。Drop で片づける。

use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn base() -> PathBuf {
    // <リポジトリ>\crates\os-sys から <リポジトリ>\target\tmp\os-sys-unit（absolute が `..` を解く）
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::path::absolute(manifest.join("..").join("..").join("target"))
        .unwrap()
        .join("tmp")
        .join("os-sys-unit")
}

pub(crate) struct UnitDir {
    path: PathBuf,
}

impl UnitDir {
    pub(crate) fn new(name: &str) -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = base().join(format!(
            "{name}-{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for UnitDir {
    fn drop(&mut self) {
        // テストの場所の下だけを消す（std の remove_dir_all は、ジャンクションの先をたどらない）
        let base = base();
        if self.path.starts_with(&base) && self.path != base {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// cmd の内部コマンドに渡すパス：絶対パスにし、`/` を `\` に直す（内部コマンドは `/` からあとをスイッチと読む）。
pub(crate) fn cmd_path(path: &Path) -> OsString {
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

/// `cmd /U` の出力（内部コマンドは UTF-16LE）を文字にする。
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
pub(crate) fn junction(link: &Path, target: &Path) -> bool {
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
        assert!(
            fs::symlink_metadata(link).unwrap().file_type().is_symlink(),
            "mklink /J は成功したのに、ジャンクションになっていない"
        );
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
