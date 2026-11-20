//! Flutter 向けの C ABI（`vf_open`・`vf_create`・`vf_open_default`・`vf_call`・`vf_close`・`vf_alloc`・`vf_free`）。
//!
//! - 要求と返り値は、どれも UTF-8 の JSON。返り値は `{"ok":…}` か `{"error":{"code","message"}}`。
//! - 保管庫を作るのは、人だけの手続き（`vf_create`・`vf_open` の init・`vf_open_default`）。ツール（`vf_call`）には
//!   作るものがない。`vf_open_default` は、ホームの直下の既定の保管庫を開く（なければ作る）。
//! - 保管庫はハンドル（1 から増える整数）で指す。ハンドルの表は `Mutex<HashMap<u64, Arc<…>>>`（poison しても使う）。
//! - actor は、アプリからの操作なので `{kind:"user", id:"app"}`（init も同じ）。
//! - どの関数も `catch_unwind` で包み、panic を外へ出さない（panic は `INTERNAL`。`vf_alloc` なら null）。
//!   ツールの処理が途中で止まった（panic）ハンドルでは、それ以降の書くツールを止める（vault-mcp と同じ考え）。
//! - 返り値とエラーの文には、根の絶対パス・ユーザー名・OS の文を出さない（vault-core のエラーの文をそのまま使う。
//!   要求の JSON の誤りは、決まった文だけを返し、渡された値を入れない）。
//!
//! メモリの決まり（二重の解放を起こさない作り）：
//! - Rust が返すバッファ（`vf_open`・`vf_create`・`vf_open_default`・`vf_call` の返り値、`vf_alloc`）は、すべて `Box<[u8]>` から作り、
//!   その先頭のアドレスと長さを表（`LIVE`）に登録する。
//! - `vf_free(ptr, len)` は、表に同じアドレスと長さで登録されているときだけ、表から外してから解放する。
//!   登録されていないもの（二重の解放・ほかで確保したもの・長さの違うもの）は、何もしない（解放しない）。
//! - そのため、二重の解放・知らないポインタの解放で、未定義の動作にならない。ただし、解放したあとに同じアドレスが
//!   新しく確保されてから、古いポインタでもう一度 `vf_free` すると、新しいほうを解放してしまう（呼ぶ側の誤り）。
//!
//! unsafe を書いてよいのは、このクレート（と Phase 1 の `os-sys`）だけ。ブロックごとに `// SAFETY:` を書く。

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use serde_json::{Value, json};
use vault_core::api::{self, WRITE_TOOLS};
use vault_core::{
    Actor, ErrorCode, RootEnv, Vault, VaultError, check_parent_dir, open_or_create_in,
    open_or_create_in_home, vault_path_in,
};

/// `vf_alloc` で確保できる大きさの上限（32 MiB）。要求の大きさの上限も同じ。
pub const MAX_ALLOC_BYTES: usize = 32 * 1024 * 1024;
/// 既定の保管庫の名前（ホームの直下のフォルダ）。vault-core は製品名を持たないので、ここに置く。
/// 画面の文の名前（Dart の `defaultVaultName`）と同じ（app の units_test が確かめる）。製品名を変えるときは、両方を直す。
pub const DEFAULT_VAULT_NAME: &str = "Buildoffice";
/// 要求の中で、文字列の外にある `[`・`{`・`,` の数の上限（読み込んだ値のメモリを抑える）。
/// 数え方と値は json-guard にまとめてある（mcp-proto と共有）。
pub const MAX_JSON_ELEMENTS: usize = json_guard::MAX_ELEMENTS;
/// アプリからの操作の actor の id（`{kind:"user", id:"app"}`）。
const APP_ACTOR_ID: &str = "app";
/// テストだけ：debug ビルドで feature `test-panic` のとき、この名前のツールの vf_call が panic する。
#[cfg(all(feature = "test-panic", debug_assertions))]
pub const TEST_PANIC_TOOL: &str = "__vault_ffi_test_panic";

/// 開いた保管庫。
struct Opened {
    vault: Vault,
    /// このハンドルで、ツールの処理が途中で止まった（panic）か。止まったあとは、書くツールを止める。
    writes_stopped: AtomicBool,
}

/// ハンドルの表。
static HANDLES: LazyLock<Mutex<HashMap<u64, Arc<Opened>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 次に使うハンドル（0 は使わない）。
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
/// Rust が返したバッファ（先頭のアドレス → 長さ）。
static LIVE: LazyLock<Mutex<HashMap<usize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// poison しても中身を使う（表は、どの操作でも1回の挿入か削除なので、途中の状態にならない）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------- 公開の関数

/// 保管庫を開く（`init:true` なら、先に作る）。要求は `{"root":"<絶対パス>","init":bool,"read_only"?:bool}`。
/// 返り値は `{"ok":{"handle":n}}` か `{"error":{"code","message"}}` の JSON（長さは `*out_len`）。
///
/// - `out_len` が null なら、何もせずに null を返す。
/// - `req_ptr` が null で `req_len > 0`、要求が 32 MiB を超える、JSON として読めない、形が違うときは `INVALID_ARGUMENT`。
/// - `root` は、ドライブ文字から始まる絶対パス（UNC やデバイスの形は、vault-core が `INVALID_ROOT` にする）。
/// - 返り値のバッファは、`vf_free(ptr, *out_len)` で解放する。
///
/// # Safety
/// - `req_ptr` は、null でなければ、`req_len` バイトを読める場所を指し、この呼び出しのあいだ変わらないこと。
/// - `req_ptr` から `req_len` バイトは、すべて初期化されていること（値の書かれていない場所を含まない）。
/// - `out_len` は、null でなければ、`usize` を1つ書ける場所を指すこと。
/// - `out_len` は、`usize` の並び（64 ビットでは 8 バイト境界）にそろった場所であること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vf_open(
    req_ptr: *const u8,
    req_len: usize,
    out_len: *mut usize,
) -> *mut u8 {
    if out_len.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: req_ptr と req_len の条件（読める・初期化されている・変わらない）は、この関数の # Safety のとおり呼ぶ側が守る。
    let request = unsafe { request_bytes(req_ptr, req_len) };
    let response = guarded(|| open(request?));
    // SAFETY: out_len は null でなく、usize の並びにそろった、書ける場所を指す（この関数の # Safety）。
    unsafe { respond(response, out_len) }
}

/// 置き場所（親のフォルダ）の中に、名前を付けて保管庫を作り、書ける状態で開く。要求は
/// `{"parent":"<置き場所の絶対パス>","name":"<保管庫の名前>"}`。返り値は `vf_open` と同じ形（`{"ok":{"handle":n}}` か
/// `{"error":{"code","message"}}`。長さは `*out_len`）。履歴の init の actor は `{user, app}`。
///
/// - `out_len` が null なら、何もせずに null を返す（何も作らない）。
/// - 要求の誤り（null で長さがある・32 MiB を超える・JSON として読めない・形が違う）は `INVALID_ARGUMENT`。
/// - 名前と置き場所の誤りは `INVALID_ROOT`（vault-core の `vault_path_in`。名前の誤りの文は「保管庫の名前：…」）。
///   置き場所のフォルダがすでにあるときだけ作る：ない・フォルダでない（ファイル）は `INVALID_ROOT`（「置き場所のフォルダがない」。
///   vault-core の `check_parent_dir`）で、何も作らない（まだない祖先を作ってよいのは、`vault-mcp init` だけ）。
///   根の判定（守る場所・入れ子・空でないフォルダ・すでに保管庫）も `INVALID_ROOT` で、どれも何も作らない。
/// - 作ったあと開けなかったときは、作った保管庫は残す（消さない）。エラーは開くときのもの。
/// - エラーの文に、絶対パスとユーザー名は入らない。置き場所も入らない。作ろうとした相対の名前（保管庫のフォルダの名前と、
///   その中の `.vault/…`）が入るのは、競合（作ったフォルダが、確かめたあとで変えられた）のときと、失敗のあとの片づけで
///   残したものを知らせるときだけ。名前の誤り（「保管庫の名前：…」）の文には、名前も入らない。
/// - 返り値のバッファは、`vf_free(ptr, *out_len)` で解放する。
///
/// # Safety
/// - `req_ptr` は、null でなければ、`req_len` バイトを読める場所を指し、この呼び出しのあいだ変わらないこと。
/// - `req_ptr` から `req_len` バイトは、すべて初期化されていること（値の書かれていない場所を含まない）。
/// - `out_len` は、null でなければ、`usize` を1つ書ける場所を指すこと。
/// - `out_len` は、`usize` の並び（64 ビットでは 8 バイト境界）にそろった場所であること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vf_create(
    req_ptr: *const u8,
    req_len: usize,
    out_len: *mut usize,
) -> *mut u8 {
    if out_len.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: req_ptr と req_len の条件（読める・初期化されている・変わらない）は、この関数の # Safety のとおり呼ぶ側が守る。
    let request = unsafe { request_bytes(req_ptr, req_len) };
    let response = guarded(|| create(request?));
    // SAFETY: out_len は null でなく、usize の並びにそろった、書ける場所を指す（この関数の # Safety）。
    unsafe { respond(response, out_len) }
}

/// 既定の保管庫（OS に問い合わせたホームの直下の `Buildoffice`）を、書ける状態で開く。なければ作ってから開く。
/// 要求は `{}`（ほかのキーは誤り）。返り値は `{"ok":{"handle":n,"created":bool}}` か `{"error":{"code","message"}}`
/// （長さは `*out_len`）。`created` は、この呼び出しで作ったか。履歴の init の actor は `{user, app}`。
///
/// - `out_len` が null なら、何もせずに null を返す（ホームに触れない）。
/// - 要求を先に読む：要求の誤り（null で長さがある・空・32 MiB を超える・JSON として読めない・形が違う）は
///   `INVALID_ARGUMENT` で、ホームに触れない。
/// - 場所がふつうのフォルダで保管庫でないときは `NOT_A_VAULT`、リンク（ジャンクション・シンボリックリンク。指す先が
///   保管庫でも）・ファイル・ホームがない・守る場所などは `INVALID_ROOT`。どれも作らず、変えない。
/// - 作ったあと開けなかったときは、作った保管庫は残す（消さない）。エラーは開くときのもの。
/// - 返り値とエラーの文に、絶対パス・ユーザー名は入らない（返り値は handle と created だけ）。
/// - 返り値のバッファは、`vf_free(ptr, *out_len)` で解放する。
///
/// # Safety
/// - `req_ptr` は、null でなければ、`req_len` バイトを読める場所を指し、この呼び出しのあいだ変わらないこと。
/// - `req_ptr` から `req_len` バイトは、すべて初期化されていること（値の書かれていない場所を含まない）。
/// - `out_len` は、null でなければ、`usize` を1つ書ける場所を指すこと。
/// - `out_len` は、`usize` の並び（64 ビットでは 8 バイト境界）にそろった場所であること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vf_open_default(
    req_ptr: *const u8,
    req_len: usize,
    out_len: *mut usize,
) -> *mut u8 {
    if out_len.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: req_ptr と req_len の条件（読める・初期化されている・変わらない）は、この関数の # Safety のとおり呼ぶ側が守る。
    let request = unsafe { request_bytes(req_ptr, req_len) };
    let response = guarded(|| open_default(request?));
    // SAFETY: out_len は null でなく、usize の並びにそろった、書ける場所を指す（この関数の # Safety）。
    unsafe { respond(response, out_len) }
}

/// ツールを呼ぶ。要求は `{"tool":"<名前>","args":{…}}`（args は省ける）。actor は `{user, app}`。
/// 返り値は `{"ok":<ツールの返り値>}` か `{"error":{"code","message"}}` の JSON（長さは `*out_len`）。
///
/// - `out_len` が null なら、何もせずに null を返す。
/// - 知らないハンドル・閉じたハンドルは `INVALID_ARGUMENT`。`tool_specs()` にない名前は `UNKNOWN_TOOL`。
/// - 返り値のバッファは、`vf_free(ptr, *out_len)` で解放する。
///
/// # Safety
/// - `req_ptr` は、null でなければ、`req_len` バイトを読める場所を指し、この呼び出しのあいだ変わらないこと。
/// - `req_ptr` から `req_len` バイトは、すべて初期化されていること（値の書かれていない場所を含まない）。
/// - `out_len` は、null でなければ、`usize` を1つ書ける場所を指すこと。
/// - `out_len` は、`usize` の並び（64 ビットでは 8 バイト境界）にそろった場所であること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vf_call(
    handle: u64,
    req_ptr: *const u8,
    req_len: usize,
    out_len: *mut usize,
) -> *mut u8 {
    if out_len.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: req_ptr と req_len の条件（読める・初期化されている・変わらない）は、この関数の # Safety のとおり呼ぶ側が守る。
    let request = unsafe { request_bytes(req_ptr, req_len) };
    let response = guarded(|| call(handle, request?));
    // SAFETY: out_len は null でなく、usize の並びにそろった、書ける場所を指す（この関数の # Safety）。
    unsafe { respond(response, out_len) }
}

/// 保管庫を閉じる（表から外す）。知らないハンドル・閉じたハンドルなら、何もしない。
/// ほかのスレッドで同じハンドルの vf_call が動いていれば、それが終わってから保管庫が片づく。
#[unsafe(no_mangle)]
pub extern "C" fn vf_close(handle: u64) {
    let _ = catch_unwind(|| {
        let removed = lock(&HANDLES).remove(&handle);
        drop(removed);
    });
}

/// `len` バイトのバッファを確保する（0 で埋める）。要求を書くのに使う。`vf_free(ptr, len)` で解放する。
/// `len` が 0 か、32 MiB を超えるか、確保できないときは null。
#[unsafe(no_mangle)]
pub extern "C" fn vf_alloc(len: usize) -> *mut u8 {
    catch_unwind(|| {
        if len == 0 || len > MAX_ALLOC_BYTES {
            return ptr::null_mut();
        }
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(len).is_err() {
            return ptr::null_mut();
        }
        buffer.resize(len, 0u8);
        into_registered(buffer).0
    })
    .unwrap_or(ptr::null_mut())
}

/// `vf_alloc` か、`vf_open`・`vf_create`・`vf_open_default`・`vf_call` が返したバッファを解放する。`len` は、確保したときの長さ（`vf_alloc` の `len`、
/// 返り値の `*out_len`）。null なら何もしない。
///
/// 登録されていないもの（二重の解放・ほかで確保したもの・長さの違うもの）は、何もしない（未定義の動作にしない）。
///
/// # Safety
/// - `ptr` は、vault-ffi が返して、まだ解放していないバッファの先頭であること（同じポインタを2回渡さない）。
///   守られなかったときも、表にないものは解放しないが、解放したあとに同じアドレスが新しく確保されてから古いポインタを
///   渡すと、新しいほうを解放してしまう。
/// - 解放したあとは、そのバッファを読み書きしないこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vf_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let _ = catch_unwind(|| {
        let registered = {
            let mut live = lock(&LIVE);
            match live.get(&ptr.addr()) {
                Some(&known) if known == len => live.remove(&ptr.addr()).is_some(),
                _ => false,
            }
        };
        if registered {
            // SAFETY: ptr と len は、into_registered が Box<[u8]>（長さ len）から作って登録したもので、表から外したのは
            // この呼び出しだけ（表の操作は Mutex の中）なので、まだ解放されていない。同じ長さの Box<[u8]> に戻して解放する。
            drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(ptr, len)) });
        }
    });
}

// ---------------- 中身

/// 要求のバイト列。null で長さがある・大きすぎるときは `INVALID_ARGUMENT`。
///
/// # Safety
/// - `req_ptr` は、null でなければ、`req_len` バイトを読める場所を指し、返した値を使うあいだ変わらないこと。
/// - `req_ptr` から `req_len` バイトは、すべて初期化されていること。
unsafe fn request_bytes<'a>(req_ptr: *const u8, req_len: usize) -> Result<&'a [u8], FfiError> {
    if req_len > MAX_ALLOC_BYTES {
        return Err(invalid_argument("要求が大きすぎる（32 MiB まで）"));
    }
    if req_len == 0 {
        return Ok(&[]);
    }
    if req_ptr.is_null() {
        return Err(invalid_argument(
            "req_ptr が null なのに、req_len が 0 でない",
        ));
    }
    // SAFETY: req_ptr は null でなく、req_len（1 以上 32 MiB 以下＝isize::MAX 以下）バイトを読める場所を指し、
    // そのバイトはすべて初期化されていて、そのあいだ変わらない（この関数の # Safety）。u8 は並びの制約がない。
    Ok(unsafe { std::slice::from_raw_parts(req_ptr, req_len) })
}

/// 処理を catch_unwind で包む。panic なら `INTERNAL`。
fn guarded(f: impl FnOnce() -> Result<Value, FfiError>) -> Result<Value, FfiError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(internal()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    root: String,
    init: bool,
    #[serde(default)]
    read_only: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    parent: String,
    name: String,
}

/// vf_open_default の要求（`{}` だけ。キーのある要求・配列などは形の誤り）。
///
/// derive の空の構造体は、空の配列 `[]` も受け付けるので、オブジェクトだけを読む形を手で書く（キーが1つでもあれば誤り。
/// 誤りの文に、渡された値を入れない）。
struct DefaultRequest {}

impl<'de> Deserialize<'de> for DefaultRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EmptyObject;

        impl<'de> serde::de::Visitor<'de> for EmptyObject {
            type Value = DefaultRequest;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("{}")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<DefaultRequest, A::Error> {
                match map.next_key::<serde::de::IgnoredAny>()? {
                    None => Ok(DefaultRequest {}),
                    Some(_) => Err(serde::de::Error::custom("キーは要らない")),
                }
            }
        }

        deserializer.deserialize_map(EmptyObject)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallRequest {
    tool: String,
    #[serde(default)]
    args: Value,
}

/// 要求の JSON を読む。誤りの文には、渡された値を入れない（絶対パスなどを返さないため）。
///
/// 読む前に、文字列の外の `[`・`{`・`,` の数を数え、100 万を超えたら読まない（読み込んだ値は、入力より何十倍も
/// 大きくなるため。mcp-proto と同じ）。読むときは、受け取る形へ直接読む（全体を一度 Value にしない）。
fn parse<T: for<'de> Deserialize<'de>>(bytes: &[u8], shape: &str) -> Result<T, FfiError> {
    if bytes.is_empty() {
        return Err(invalid_argument("要求が空"));
    }
    if json_guard::too_many_elements(bytes, MAX_JSON_ELEMENTS) {
        return Err(invalid_argument(
            "要求の要素が多すぎる（文字列の外の [ と { と , は、100 万個まで）",
        ));
    }
    serde_json::from_slice(bytes).map_err(|e| match e.classify() {
        serde_json::error::Category::Data => {
            invalid_argument(&format!("要求の形が違う（{shape}）"))
        }
        _ => invalid_argument("要求が JSON として読めない（UTF-8 の JSON で渡す）"),
    })
}

fn open(request: &[u8]) -> Result<Value, FfiError> {
    let request: OpenRequest = parse(
        request,
        r#"{"root":"<絶対パス>","init":true か false,"read_only":true か false（省ける）}"#,
    )?;
    let path = Path::new(&request.root);
    let env = RootEnv::from_system()?;
    if request.init {
        Vault::init(path, &env, &Actor::user(APP_ACTOR_ID))?;
    }
    let vault = Vault::open(path, request.read_only, &env)?;
    Ok(register(vault))
}

fn create(request: &[u8]) -> Result<Value, FfiError> {
    let request: CreateRequest = parse(
        request,
        r#"{"parent":"<置き場所の絶対パス>","name":"<保管庫の名前>"}"#,
    )?;
    // 名前の確かめとパスのつなぎは vault-core（呼ぶ側では組み立てない）
    let parent = Path::new(&request.parent);
    let path = vault_path_in(parent, &request.name)?;
    // 置き場所がすでにあるときだけ作る（打ち間違いで、知らないフォルダを作らない。何も作る前に確かめる）
    check_parent_dir(parent)?;
    let env = RootEnv::from_system()?;
    Vault::init(&path, &env, &Actor::user(APP_ACTOR_ID))?;
    let vault = Vault::open(&path, false, &env)?;
    Ok(register(vault))
}

fn open_default(request: &[u8]) -> Result<Value, FfiError> {
    // 要求を先に読む（形が違えば、ホームに触れない）
    let DefaultRequest {} = parse(request, "{}")?;
    default_reply(None)
}

/// 既定の保管庫を開き（なければ作る）、返事 `{"handle","created"}` を作る（vf_open_default の本体）。
///
/// `parent` が None なら、OS に問い合わせたホーム（vault-core の `open_or_create_in_home`。ホームのパスは vault-ffi に
/// 出てこない）。Some（置き場所を渡す）は、src のテストが偽のホームで使うだけ（本物のホームに作らないため。ホームの
/// 問い合わせとのつなぎ目は、手で確かめる）。
fn default_reply(parent: Option<&Path>) -> Result<Value, FfiError> {
    let env = RootEnv::from_system()?;
    let actor = Actor::user(APP_ACTOR_ID);
    let opened = match parent {
        None => open_or_create_in_home(DEFAULT_VAULT_NAME, &env, &actor)?,
        Some(parent) => open_or_create_in(parent, DEFAULT_VAULT_NAME, &env, &actor)?,
    };
    let created = opened.created;
    Ok(json!({ "handle": add_handle(opened.vault), "created": created }))
}

/// 開いた保管庫を表に登録して、ハンドルを返す（`{"handle":n}`）。
fn register(vault: Vault) -> Value {
    json!({ "handle": add_handle(vault) })
}

/// 開いた保管庫を表に登録して、ハンドルの番号を返す。
fn add_handle(vault: Vault) -> u64 {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let opened = Arc::new(Opened {
        vault,
        writes_stopped: AtomicBool::new(false),
    });
    lock(&HANDLES).insert(handle, opened);
    handle
}

fn call(handle: u64, request: &[u8]) -> Result<Value, FfiError> {
    let request: CallRequest = parse(request, r#"{"tool":"<ツールの名前>","args":{…}（省ける）}"#)?;
    // 表から写して、すぐにロックを外す（ツールの処理のあいだ、ほかのハンドルを止めない）
    let opened = lock(&HANDLES)
        .get(&handle)
        .cloned()
        .ok_or_else(|| invalid_argument("知らないハンドル（閉じたか、開いていない）"))?;
    let tool = request.tool.as_str();
    if opened.writes_stopped.load(Ordering::Relaxed) && WRITE_TOOLS.contains(&tool) {
        return Err(FfiError::new(
            ErrorCode::Internal,
            "前の呼び出しで内部のエラーが起きたので、この保管庫では書き込みを止めた。開き直す（vf_close のあと vf_open）",
        ));
    }
    let actor = Actor::user(APP_ACTOR_ID);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        #[cfg(all(feature = "test-panic", debug_assertions))]
        if tool == TEST_PANIC_TOOL {
            panic!("テスト用の panic：C:\\Profiles\\secret-user\\PANIC-SECRET.txt");
        }
        api::call(&opened.vault, tool, &request.args, &actor)
    }));
    match outcome {
        Ok(result) => result.map_err(FfiError::from),
        Err(_) => {
            // 途中で止まった：保管庫の中が、履歴と索引に合っていないかもしれないので、このハンドルでは書き込みを止める
            opened.writes_stopped.store(true, Ordering::Relaxed);
            Err(internal())
        }
    }
}

/// 返り値を JSON にして、登録したバッファで返す（長さは `*out_len`）。作れなければ null（`*out_len` は 0）。
///
/// # Safety
/// - `out_len` は null でなく、`usize` を1つ書ける場所を指すこと。
/// - `out_len` は、`usize` の並び（64 ビットでは 8 バイト境界）にそろった場所であること。
unsafe fn respond(response: Result<Value, FfiError>, out_len: *mut usize) -> *mut u8 {
    let body = match response {
        Ok(value) => json!({ "ok": value }),
        Err(e) => json!({ "error": { "code": e.code.as_str(), "message": e.message } }),
    };
    let (ptr, len) = catch_unwind(AssertUnwindSafe(|| {
        let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| {
            r#"{"error":{"code":"INTERNAL","message":"返り値を作れない"}}"#
                .as_bytes()
                .to_vec()
        });
        into_registered(bytes)
    }))
    .unwrap_or((ptr::null_mut(), 0));
    // SAFETY: out_len は null でなく、usize の並びにそろった、usize を1つ書ける場所を指す（この関数の # Safety）。
    unsafe { out_len.write(len) };
    ptr
}

/// バイト列を `Box<[u8]>` にして、先頭のアドレスと長さを表に登録し、ポインタと長さを返す。
fn into_registered(bytes: Vec<u8>) -> (*mut u8, usize) {
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed).cast::<u8>();
    lock(&LIVE).insert(ptr.addr(), len);
    (ptr, len)
}

/// FFI の返り値のエラー（vault-core のエラーと、FFI の受付口で作るエラー）。
#[derive(Debug)]
struct FfiError {
    code: ErrorCode,
    message: String,
}

impl FfiError {
    fn new(code: ErrorCode, message: &str) -> Self {
        Self {
            code,
            message: message.to_owned(),
        }
    }
}

impl From<VaultError> for FfiError {
    fn from(e: VaultError) -> Self {
        Self {
            code: e.code(),
            message: e.message().to_owned(),
        }
    }
}

fn invalid_argument(message: &str) -> FfiError {
    FfiError::new(ErrorCode::InvalidArgument, message)
}

fn internal() -> FfiError {
    FfiError::new(ErrorCode::Internal, "内部のエラー（処理が途中で止まった）")
}

/// 今、登録されている（まだ解放されていない）バッファの数（テストで、解放の漏れを確かめるため）。
#[doc(hidden)]
pub fn live_buffers() -> usize {
    lock(&LIVE).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU32;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 上限は json-guard と同じ値（数え方のテストは json-guard にある）。
    #[test]
    fn element_limit_is_shared_with_json_guard() {
        assert_eq!(MAX_JSON_ELEMENTS, 1_000_000);
        assert_eq!(MAX_JSON_ELEMENTS, json_guard::MAX_ELEMENTS);
    }

    /// 偽のホーム（ユニットテストには CARGO_TARGET_TMPDIR がないので、リポジトリの `target\tmp\vault-ffi-unit` の下に
    /// だけ作る。本物のホームや %TEMP% には作らない）。Drop で片づける（その下だけ）。
    struct FakeHome {
        path: PathBuf,
    }

    fn unit_base() -> PathBuf {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::path::absolute(manifest.join("..").join("..").join("target"))
            .unwrap()
            .join("tmp")
            .join("vault-ffi-unit")
    }

    impl FakeHome {
        fn new(name: &str) -> Self {
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = unit_base().join(format!(
                "{name}-{}-{nanos}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for FakeHome {
        fn drop(&mut self) {
            let base = unit_base();
            if self.path.starts_with(&base) && self.path != base {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }

    fn tool(handle: u64, request: Value) -> Value {
        call(handle, request.to_string().as_bytes()).unwrap_or_else(|e| panic!("{}", e.message))
    }

    /// vf_open_default の本体（default_reply）を、偽のホームで：1回目は作って開き（created:true）、2回目は作らずに
    /// 開く（created:false）。返事のキーは handle と created の2つだけで、パスを含まない。handle で書いて読め、履歴の
    /// init の actor は {user, app}。
    #[test]
    fn default_reply_creates_then_opens_in_a_fake_home() {
        let home = FakeHome::new("default");
        let first = default_reply(Some(&home.path)).unwrap_or_else(|e| panic!("{}", e.message));
        let keys: Vec<&str> = first
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["created", "handle"]);
        assert_eq!(first["created"], true);
        let handle = first["handle"].as_u64().unwrap();
        let text = first.to_string();
        for needle in [
            home.path.display().to_string(),
            r"\\?\".to_owned(),
            "vault-ffi-unit".to_owned(),
            DEFAULT_VAULT_NAME.to_owned(),
        ] {
            assert!(!text.contains(&needle), "返事にパスや名前がある：{text}");
        }

        let info = tool(handle, json!({"tool": "vault_info"}));
        assert_eq!(info["name"], DEFAULT_VAULT_NAME);
        assert_eq!(info["read_only"], false);
        tool(
            handle,
            json!({"tool": "vault_write", "args": {"path": "a.txt", "content": "a"}}),
        );
        let read = tool(
            handle,
            json!({"tool": "vault_read", "args": {"path": "a.txt"}}),
        );
        assert_eq!(read["content"], "a");
        let vault = home.path.join(DEFAULT_VAULT_NAME);
        let journal_dir = vault.join(".vault").join("journal");
        let mut files: Vec<PathBuf> = fs::read_dir(&journal_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        let text = fs::read_to_string(&files[0]).unwrap();
        let first_line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first_line["op"], "init");
        assert_eq!(first_line["actor"]["kind"], "user");
        assert_eq!(first_line["actor"]["id"], APP_ACTOR_ID);
        vf_close(handle);

        let second = default_reply(Some(&home.path)).unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(second.as_object().unwrap().len(), 2, "{second}");
        assert_eq!(second["created"], false);
        let again = second["handle"].as_u64().unwrap();
        assert_ne!(again, handle);
        let read = tool(
            again,
            json!({"tool": "vault_read", "args": {"path": "a.txt"}}),
        );
        assert_eq!(read["content"], "a");
        vf_close(again);
    }

    /// 偽のホームで、場所が保管庫でないフォルダなら NOT_A_VAULT（作らない）。エラーの文にパスがない。
    #[test]
    fn default_reply_refuses_a_folder_that_is_not_a_vault() {
        let home = FakeHome::new("default-not-vault");
        let folder = home.path.join(DEFAULT_VAULT_NAME);
        fs::create_dir(&folder).unwrap();
        let e = default_reply(Some(&home.path)).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotAVault);
        assert!(!e.message.contains(&home.path.display().to_string()));
        assert!(!e.message.contains(r"\\?\"));
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 0, "何も作らない");
    }

    /// vf_open_default の要求は `{}` だけ（空白はよい）。キーのある要求・配列・ほかの型・空・壊れた JSON は
    /// INVALID_ARGUMENT で、渡された値を文に入れない。
    #[test]
    fn the_default_request_is_only_an_empty_object() {
        for ok in [b"{}".as_slice(), b" { }\n"] {
            assert!(parse::<DefaultRequest>(ok, "{}").is_ok(), "{ok:?}");
        }
        for bad in [
            b"[]".as_slice(),
            br#"{"x":"secret-value"}"#,
            br#"{"root":"C:\\Profiles\\secret-value"}"#,
            b"null",
            b"1",
            br#""{}""#,
            b"{} {}",
            b"",
            b"{",
            b"\xff\xfe",
        ] {
            let e = match parse::<DefaultRequest>(bad, "{}") {
                Ok(DefaultRequest {}) => panic!("断るべき：{bad:?}"),
                Err(e) => e,
            };
            assert_eq!(e.code, ErrorCode::InvalidArgument, "{bad:?}");
            assert!(!e.message.contains("secret-value"), "{}", e.message);
        }
    }

    /// 既定の保管庫の名前（製品名を変えるときは、Dart の defaultVaultName も直す）。
    #[test]
    fn the_default_vault_name() {
        assert_eq!(DEFAULT_VAULT_NAME, "Buildoffice");
        assert!(vault_path_in(Path::new(r"C:\Profiles\u"), DEFAULT_VAULT_NAME).is_ok());
    }
}
