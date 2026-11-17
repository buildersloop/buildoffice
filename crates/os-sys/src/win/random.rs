//! OS の乱数（W4。BCryptGenRandom）。

use std::io;
use std::ptr;

use super::{ffi, status_error};

/// OS の乱数で `buf` を埋める（BCryptGenRandom を、アルゴリズムのハンドルなしで
/// BCRYPT_USE_SYSTEM_PREFERRED_RNG を付けて呼ぶ）。
///
/// - 長さが 0 なら呼ばずに Ok。u32 の最大より長ければ、分けて呼ぶ。
/// - 成功は状態の値が 0（STATUS_SUCCESS）のときだけ。0 でなければ失敗で、RtlNtStatusToDosError で直した
///   エラーを返す。
/// - **Err のときは、バッファを 0 で上書きしてから返す。Err のとき、バッファの中身を使わないこと。**
///   ほかの出どころ（時刻など）には落とさない。
pub fn os_random(buf: &mut [u8]) -> io::Result<()> {
    fill_in_chunks(buf, u32::MAX as usize, |chunk| {
        // 分けて呼ぶので、長さは u32 に収まる（収まらなければ失敗の値にする）
        let Ok(len) = u32::try_from(chunk.len()) else {
            return ffi::STATUS_INVALID_PARAMETER;
        };
        // SAFETY: `chunk` は `len` バイトを書ける領域で、OS は `len` バイトを超えて書かない
        // （_Out_writes_bytes_(cbBuffer)）。アルゴリズムのハンドルは NULL（BCRYPT_USE_SYSTEM_PREFERRED_RNG
        // を付けるときの決まり）。
        unsafe {
            ffi::BCryptGenRandom(
                ptr::null_mut(),
                chunk.as_mut_ptr(),
                len,
                ffi::BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        }
    })
}

/// `max` バイトずつに分けて `fill` を呼ぶ。どれかが 0 以外を返したら、残りを呼ばず、全体を 0 にして Err。
fn fill_in_chunks(
    buf: &mut [u8],
    max: usize,
    mut fill: impl FnMut(&mut [u8]) -> i32,
) -> io::Result<()> {
    let max = max.max(1);
    let mut start = 0;
    while start < buf.len() {
        let end = start + (buf.len() - start).min(max);
        let status = fill(&mut buf[start..end]);
        if status != 0 {
            buf.fill(0);
            return Err(status_error(status));
        }
        start = end;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分けて呼ぶ：4 で分けて長さ 10 なら 4・4・2。
    #[test]
    fn splits_into_chunks() {
        let mut lens = Vec::new();
        let mut buf = [0u8; 10];
        fill_in_chunks(&mut buf, 4, |c| {
            lens.push(c.len());
            c.fill(7);
            0
        })
        .unwrap();
        assert_eq!(lens, vec![4, 4, 2]);
        assert_eq!(buf, [7; 10]);
    }

    /// 2回めで失敗させると Err になり、3回めを呼ばず、バッファが全部 0。
    #[test]
    fn failure_zeroes_buffer_and_stops() {
        let mut calls = 0;
        let mut buf = [0u8; 10];
        // STATUS_UNSUCCESSFUL
        let failure = 0xC000_0001_u32 as i32;
        let err = fill_in_chunks(&mut buf, 4, |c| {
            calls += 1;
            c.fill(9);
            if calls == 2 { failure } else { 0 }
        })
        .unwrap_err();
        assert_eq!(calls, 2);
        assert_eq!(buf, [0; 10]);
        // STATUS_UNSUCCESSFUL は ERROR_GEN_FAILURE（31）に直る
        assert_eq!(err.raw_os_error(), Some(31));
    }

    /// 正の状態の値（STATUS_PENDING など。NT_SUCCESS では成功の側）も失敗にする。バッファは全部 0。
    #[test]
    fn positive_status_is_failure() {
        let mut buf = [0u8; 10];
        let err = fill_in_chunks(&mut buf, 4, |c| {
            c.fill(9);
            ffi::STATUS_PENDING
        })
        .unwrap_err();
        assert_eq!(buf, [0; 10]);
        assert_ne!(err.raw_os_error(), Some(0));
    }

    /// 長さ 0 なら呼ばない。
    #[test]
    fn empty_buffer_does_not_call() {
        let mut called = false;
        fill_in_chunks(&mut [], 4, |_| {
            called = true;
            0
        })
        .unwrap();
        assert!(!called);
    }
}
