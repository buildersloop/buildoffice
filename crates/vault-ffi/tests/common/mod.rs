//! vault-ffi の結合テストの共通の道具。C ABI の関数を、Rust から（rlib として）呼ぶ。
//! 保管庫は `env!("CARGO_TARGET_TMPDIR")` の下にだけ作り、`TestDir` の Drop で片づける。

// テストのファイルごとに使う道具が違うので、使わないものがあっても警告にしない
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use vault_ffi::{vf_call, vf_free, vf_open};

pub fn tests_base() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("vault-ffi-tests")
}

/// テストごとのフォルダ。Drop で中身ごと消す（テストの場所の下だけ）。
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
        let base = tests_base();
        if self.path.starts_with(&base) && self.path != base {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// vault-ffi が返したバッファを読んで JSON にし、vf_free で解放する。
///
/// # Safety
/// `ptr` は vf_open・vf_call が返したもので、`len` はそのときの `*out_len`。
pub unsafe fn take(ptr: *mut u8, len: usize) -> Value {
    assert!(!ptr.is_null(), "返り値が null");
    // SAFETY: ptr は vault-ffi が返した、len バイトのバッファ（この関数の # Safety）。読むあいだは解放しない。
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    // SAFETY: 同じ ptr と len を1回だけ返す（このあと ptr を使わない）。
    unsafe { vf_free(ptr, len) };
    serde_json::from_slice(&bytes).expect("返り値が JSON でない")
}

/// バイト列の要求で vf_open を呼ぶ。
pub fn open_raw(request: &[u8]) -> Value {
    let mut len = 0usize;
    // SAFETY: request は request.len() バイトを読める。len は書ける usize。
    let ptr = unsafe { vf_open(request.as_ptr(), request.len(), &mut len) };
    // SAFETY: ptr と len は、いま vf_open が返したもの。
    unsafe { take(ptr, len) }
}

/// JSON の要求で vf_open を呼ぶ。
pub fn open(request: Value) -> Value {
    open_raw(request.to_string().as_bytes())
}

/// バイト列の要求で vf_call を呼ぶ。
pub fn call_raw(handle: u64, request: &[u8]) -> Value {
    let mut len = 0usize;
    // SAFETY: request は request.len() バイトを読める。len は書ける usize。
    let ptr = unsafe { vf_call(handle, request.as_ptr(), request.len(), &mut len) };
    // SAFETY: ptr と len は、いま vf_call が返したもの。
    unsafe { take(ptr, len) }
}

/// ツールを呼ぶ。
pub fn call(handle: u64, tool: &str, args: Value) -> Value {
    call_raw(
        handle,
        json!({"tool": tool, "args": args}).to_string().as_bytes(),
    )
}

/// 成功を確かめて、中身を返す。
pub fn ok(reply: &Value) -> Value {
    assert!(reply.get("error").is_none(), "失敗した：{reply}");
    reply["ok"].clone()
}

/// 失敗を確かめて、コードを返す。
pub fn error_code(reply: &Value) -> String {
    assert!(reply.get("ok").is_none(), "成功した：{reply}");
    reply["error"]["code"].as_str().unwrap().to_owned()
}

/// `dir/name` に保管庫を作って開き、ハンドルを返す。
pub fn new_vault(dir: &TestDir, name: &str) -> (PathBuf, u64) {
    let path = dir.join(name);
    let reply = open(json!({"root": path.to_str().unwrap(), "init": true}));
    let handle = ok(&reply)["handle"].as_u64().unwrap();
    (path, handle)
}

/// 保管庫の履歴をすべて読む。
pub fn journal(vault: &Path) -> Vec<Value> {
    let dir = vault.join(".vault").join("journal");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|file| {
            fs::read_to_string(file)
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// 出力に出てはいけない文字（保管庫の絶対パス・テストの場所・ユーザー名）。
pub fn secrets(vault: &Path) -> Vec<String> {
    let mut needles = vec![
        vault.display().to_string(),
        vault.display().to_string().replace('\\', "/"),
        env!("CARGO_TARGET_TMPDIR").to_owned(),
        "vault-ffi-tests".to_owned(),
        r"\\?\".to_owned(),
    ];
    if let Ok(canon) = fs::canonicalize(vault) {
        needles.push(canon.display().to_string());
    }
    for var in ["USERNAME", "USER"] {
        if let Ok(name) = std::env::var(var)
            && name.len() >= 3
        {
            needles.push(name);
        }
    }
    needles
}
