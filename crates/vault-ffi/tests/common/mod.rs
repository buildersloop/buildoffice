//! vault-ffi の結合テストの共通の道具。C ABI の関数を、Rust から（rlib として）呼ぶ。
//! 保管庫は `env!("CARGO_TARGET_TMPDIR")` の下にだけ作り、`TestDir` の Drop で片づける。

// テストのファイルごとに使う道具が違うので、使わないものがあっても警告にしない
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use vault_ffi::{DEFAULT_VAULT_NAME, vf_call, vf_create, vf_free, vf_open, vf_open_default};

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

/// バイト列の要求で vf_create を呼ぶ。
pub fn create_raw(request: &[u8]) -> Value {
    let mut len = 0usize;
    // SAFETY: request は request.len() バイトを読める。len は書ける usize。
    let ptr = unsafe { vf_create(request.as_ptr(), request.len(), &mut len) };
    // SAFETY: ptr と len は、いま vf_create が返したもの。
    unsafe { take(ptr, len) }
}

/// 置き場所と名前で vf_create を呼ぶ。
pub fn create(parent: &Path, name: &str) -> Value {
    create_raw(
        json!({"parent": parent.to_str().unwrap(), "name": name})
            .to_string()
            .as_bytes(),
    )
}

/// バイト列の要求で vf_open_default を呼ぶ。
///
/// テストでは、要求の形の誤りの道だけに使う（形が正しい要求を渡すと、本物のホームに既定の保管庫を作る・開くので渡さない。
/// 成功の道は、src のテストが偽のホームで見る）。
pub fn open_default_raw(request: &[u8]) -> Value {
    let mut len = 0usize;
    // SAFETY: request は request.len() バイトを読める。len は書ける usize。
    let ptr = unsafe { vf_open_default(request.as_ptr(), request.len(), &mut len) };
    // SAFETY: ptr と len は、いま vf_open_default が返したもの。
    unsafe { take(ptr, len) }
}

/// 本物のホームの既定の保管庫の場所（%USERPROFILE% の下の既定の名前）の有無と種類（読むだけ。パスは持たない）。
#[derive(Debug, PartialEq, Eq)]
pub enum PlaceState {
    Missing,
    Dir,
    File,
    Link,
    /// 調べられない（種類だけを持つ）。
    Unknown(std::io::ErrorKind),
}

/// 本物のホームの既定の保管庫の場所の、今の有無と種類。テストの前後で同じ（ホームに触れていない）ことを確かめる。
pub fn real_default_state() -> PlaceState {
    let home = std::env::var_os("USERPROFILE").expect("USERPROFILE がある");
    match fs::symlink_metadata(Path::new(&home).join(DEFAULT_VAULT_NAME)) {
        Ok(meta) if meta.file_type().is_symlink() => PlaceState::Link,
        Ok(meta) if meta.is_dir() => PlaceState::Dir,
        Ok(_) => PlaceState::File,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PlaceState::Missing,
        Err(e) => PlaceState::Unknown(e.kind()),
    }
}

/// 本物のホームの既定の保管庫の場所がすでにあるとき、「SKIP：…」を出して true を返す（種類は問わない。リンクも、
/// 調べられないときも「ある」とみなす）。呼ぶ側は、本物のホームを使う部分（形の誤りの要求で vf_open_default を呼ぶ
/// テスト）を飛ばす。使っている人の本物の保管庫を、このテストが開かないため。
///
/// DefaultRequest の決まりを変えるときは、これを使うテストを見直す（形の誤りの要求が通るようになると、本物のホームに
/// 保管庫を作るため）。
///
/// 出すのは、標準エラーへの直接の書き込み（テストの出力の取り込みに隠されず、通ったテストでも見える）。
pub fn skip_when_real_default_exists() -> bool {
    use std::io::Write;
    if real_default_state() == PlaceState::Missing {
        return false;
    }
    let _ = writeln!(
        std::io::stderr(),
        "SKIP：既定の保管庫がすでにあるので、本物のホームを使うテストを飛ばした"
    );
    true
}

/// フォルダの中の名前の一覧（並べ替えたもの。ないフォルダなら空）。何も作られていないことを確かめるのに使う。
pub fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
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

/// この PC に残らないよう、呼ぶたびに違う名前（守る場所に作ってしまっても、見つけられる）。
pub fn unique_name(prefix: &str) -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// 飛ばしたテストを出力する（`cargo test -- --nocapture` で見える）。
pub fn skip(test: &str, reason: &str) {
    eprintln!("SKIP {test}: {reason}");
}

/// テストで作ったジャンクション。Drop で、リンクだけを外す（指す先は消さない・中身にも触れない）。
/// `TestDir` より後に作る（先に外れるように）。
#[cfg(windows)]
pub struct Junction {
    link: PathBuf,
}

#[cfg(windows)]
impl Junction {
    /// `cmd /C mklink /J` でジャンクションを作る。作れたら、本当にリンクになったことも確かめて `Some`。
    /// ボリュームがジャンクションに対応しないときだけ、飛ばしたことを出力して `None`。それ以外の失敗は、テストの失敗にする。
    pub fn new(test: &str, link: &Path, target: &Path) -> Option<Self> {
        let (link_arg, target_arg) = (cmd_path(link), cmd_path(target));
        let (code, text) = run_cmd(&[
            "mklink".as_ref(),
            "/J".as_ref(),
            link_arg.as_os_str(),
            target_arg.as_os_str(),
        ]);
        if code == Some(0) {
            // 作れたので、Drop が外せるようにしてから確かめる
            let made = Self {
                link: link.to_path_buf(),
            };
            let meta = fs::symlink_metadata(link)
                .unwrap_or_else(|e| panic!("mklink /J は成功したのに、リンクがない：{e}"));
            assert!(
                meta.file_type().is_symlink(),
                "mklink /J は成功したのに、ジャンクションになっていない"
            );
            return Some(made);
        }
        let lower = text.to_lowercase();
        if text.contains("NTFS")
            || text.contains("サポートされていません")
            || lower.contains("not supported")
        {
            skip(
                test,
                &format!(
                    "このボリュームはジャンクションに対応しない（終了コード {code:?}）：{}",
                    text.trim()
                ),
            );
            return None;
        }
        panic!(
            "mklink /J が失敗した（終了コード {code:?}）：{}",
            text.trim()
        );
    }
}

#[cfg(windows)]
impl Drop for Junction {
    fn drop(&mut self) {
        // リンクのままのときだけ、リンクを外す（remove_dir はリンクそのものを消す。先の中身は消さない）
        if fs::symlink_metadata(&self.link).is_ok_and(|m| m.file_type().is_symlink()) {
            let _ = fs::remove_dir(&self.link);
        }
    }
}

/// `cmd /U /C ...` を動かして、終了コードと出力（UTF-16 を読んだ文字）を返す。起動できなければ失敗にする。
#[cfg(windows)]
fn run_cmd(args: &[&std::ffi::OsStr]) -> (Option<i32>, String) {
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

/// cmd の内部コマンド（mklink）に渡すパス：絶対パスにし、`/` を `\` に直す（内部コマンドは、パスの中の `/` からあとを
/// スイッチと読むため）。
#[cfg(windows)]
fn cmd_path(path: &Path) -> std::ffi::OsString {
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
