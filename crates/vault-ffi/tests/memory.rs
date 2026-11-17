//! vf_alloc・vf_free と、返り値のバッファの扱い（二重の解放などを起こさない作り）。
//! 登録しているバッファの数（`live_buffers`）を数えるので、このファイルのテストは1つだけにする（並んで動かさない）。

mod common;

use std::ptr;

use common::{TestDir, open_raw, take};
use serde_json::json;
use vault_ffi::{MAX_ALLOC_BYTES, live_buffers, vf_alloc, vf_close, vf_free, vf_open};

#[test]
fn buffers_are_allocated_freed_once_and_never_double_freed() {
    let base = live_buffers();

    // 上限：0 と 32 MiB を超えるものは null。32 MiB ちょうどは確保できる
    assert!(vf_alloc(0).is_null());
    assert!(vf_alloc(MAX_ALLOC_BYTES + 1).is_null());
    assert!(vf_alloc(usize::MAX).is_null());
    let big = vf_alloc(MAX_ALLOC_BYTES);
    assert!(!big.is_null());
    assert_eq!(live_buffers(), base + 1);
    // SAFETY: big は vf_alloc(MAX_ALLOC_BYTES) が返したもので、まだ解放していない。
    unsafe { vf_free(big, MAX_ALLOC_BYTES) };
    assert_eq!(live_buffers(), base);

    // vf_alloc のバッファに要求を書いて、vf_open に渡す（Dart と同じ使い方）
    let dir = TestDir::new("ffi-memory");
    let request = json!({"root": dir.join("v").to_str().unwrap(), "init": true}).to_string();
    let buffer = vf_alloc(request.len() + 10);
    assert!(!buffer.is_null());
    // SAFETY: buffer は request.len() + 10 バイトの、vault-ffi が確保した書けるバッファ。request と重ならない。
    unsafe { ptr::copy_nonoverlapping(request.as_ptr(), buffer, request.len()) };
    let mut out_len = 0usize;
    // SAFETY: buffer の先頭 request.len() バイトは書いたばかりで読める。out_len は書ける usize。
    let out = unsafe { vf_open(buffer, request.len(), &mut out_len) };
    assert_eq!(live_buffers(), base + 2, "要求のバッファと返り値のバッファ");
    // SAFETY: out と out_len は、いま vf_open が返したもの（take が1回だけ解放する）。
    let reply = unsafe { take(out, out_len) };
    let handle = reply["ok"]["handle"].as_u64().expect("開けた");
    // SAFETY: buffer は vf_alloc(request.len() + 10) が返したもので、まだ解放していない。
    unsafe { vf_free(buffer, request.len() + 10) };
    assert_eq!(live_buffers(), base);

    // 二重の解放・長さの違い・ほかで確保したもの・null は、何もしない（表にないものは解放しない）
    let buffer = vf_alloc(16);
    // SAFETY: buffer は vf_alloc(16) が返したもの。長さを違えて渡すのは、決まりの外の呼び方だが、vf_free は表にない
    // 組み合わせを触らない（解放も読み書きもしない）ことを確かめるため。
    unsafe { vf_free(buffer, 15) };
    assert_eq!(live_buffers(), base + 1, "長さが違えば解放しない");
    // SAFETY: buffer は vf_alloc(16) が返したもので、まだ解放していない（上は何もしなかった）。
    unsafe { vf_free(buffer, 16) };
    assert_eq!(live_buffers(), base);
    // SAFETY: 二重の解放は決まりの外の呼び方だが、vf_free は表にないものを触らないことを確かめるため（表から外れている）。
    unsafe { vf_free(buffer, 16) };
    assert_eq!(live_buffers(), base);
    let mut foreign = vec![0u8; 16];
    // SAFETY: ほかで確保したものは決まりの外の呼び方だが、vf_free は表にないアドレスを触らないことを確かめるため。
    unsafe { vf_free(foreign.as_mut_ptr(), 16) };
    assert_eq!(foreign, vec![0u8; 16], "ほかのバッファは、そのまま");
    // SAFETY: null は何もしない決まり。
    unsafe { vf_free(ptr::null_mut(), 16) };

    // 返り値のバッファも、解放すれば表から外れる（失敗の返り値も同じ）
    let reply = open_raw(b"{");
    assert_eq!(reply["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(live_buffers(), base, "返り値のバッファの解放の漏れがない");

    vf_close(handle);
}
