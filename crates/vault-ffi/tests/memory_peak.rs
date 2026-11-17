//! 要求を読むときのメモリの測定。ふつうのテストでは動かさない。
//!
//! 外から、このテストのプロセスの最大の作業セット（PeakWorkingSetSize）を見る。処理のあと少し待つので、そのあいだに読む。
//! `cargo test --release -p vault-ffi --test memory_peak -- --ignored --nocapture`

mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{TestDir, call_raw, new_vault, open_raw};

/// 32 MiB ちょうどの要求：`<head>[0,0,…,0]<tail>`。
fn big_request(head: &str, tail: &str) -> Vec<u8> {
    let budget = 32 * 1024 * 1024 - head.len() - tail.len();
    let mut request = Vec::with_capacity(32 * 1024 * 1024);
    request.extend_from_slice(head.as_bytes());
    request.extend_from_slice(&b"0,".repeat((budget - 1) / 2));
    request.push(b'0');
    request.extend_from_slice(tail.as_bytes());
    assert!(request.len() <= 32 * 1024 * 1024);
    request
}

fn report(label: &str, reply: &serde_json::Value, start: Instant) {
    let text: String = reply.to_string().chars().take(60).collect();
    println!("{label}：返事 {text}…／{:?}", start.elapsed());
    thread::sleep(Duration::from_millis(1500));
}

#[test]
#[ignore = "メモリを測る（外から最大の作業セットを見る）"]
fn memory_of_a_32_mib_call_request() {
    let dir = TestDir::new("ffi-peak-call");
    let (_path, handle) = new_vault(&dir, "v");
    let request = big_request(r#"{"tool":"vault_info","args":{"a":["#, "]}}");
    let start = Instant::now();
    let reply = call_raw(handle, &request);
    report("vf_call に 32 MiB の [0,0,…]", &reply, start);
}

#[test]
#[ignore = "メモリを測る（外から最大の作業セットを見る）"]
fn memory_of_a_32_mib_open_request() {
    let request = big_request(r#"{"root":["#, r#"],"init":false}"#);
    let start = Instant::now();
    let reply = open_raw(&request);
    report("vf_open に 32 MiB の [0,0,…]", &reply, start);
}
