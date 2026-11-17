//! base64（RFC 4648 の 4 章の標準の形。`+` `/` とパディングの `=` を使う）。
//!
//! 外部のライブラリを使わずに自作する。読むときは厳密に読み、次のものを拒否する：
//! 長さが4の倍数でないもの、知らない文字（空白・改行・URL 用の `-` `_` を含む）、
//! 途中や多すぎるパディング、使われないビットが0でないもの（RFC 4648 の 3.5 章）。

use crate::error::{ErrorCode, Result, VaultError};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// バイト列を base64 の文字にする。
pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(symbol(n >> 18));
        out.push(symbol(n >> 12));
        out.push(if chunk.len() > 1 { symbol(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { symbol(n) } else { '=' });
    }
    out
}

/// base64 の文字をバイト列に戻す。形が正しくなければ `INVALID_ARGUMENT`。
pub fn decode(input: &str) -> Result<Vec<u8>> {
    let (quads, rest) = input.as_bytes().as_chunks::<4>();
    if !rest.is_empty() {
        return Err(invalid("base64 の長さが4の倍数でない"));
    }
    let count = quads.len();
    let mut out = Vec::with_capacity(count * 3);
    for (i, quad) in quads.iter().enumerate() {
        let pad = if i + 1 == count {
            quad.iter().rev().take_while(|&&c| c == b'=').count()
        } else {
            0
        };
        if pad > 2 {
            return Err(invalid("base64 のパディング（=）が多すぎる"));
        }
        let mut n: u32 = 0;
        for &c in &quad[..4 - pad] {
            let v = value(c).ok_or_else(|| invalid("base64 に使えない文字がある"))?;
            n = (n << 6) | v;
        }
        n <<= 6 * pad as u32;
        let [_, b0, b1, b2] = n.to_be_bytes();
        match pad {
            0 => out.extend_from_slice(&[b0, b1, b2]),
            1 if b2 == 0 => out.extend_from_slice(&[b0, b1]),
            2 if b1 == 0 && b2 == 0 => out.push(b0),
            _ => return Err(invalid("base64 の使われないビットが0でない")),
        }
    }
    Ok(out)
}

fn symbol(v: u32) -> char {
    char::from(ALPHABET[(v & 0x3f) as usize])
}

fn value(c: u8) -> Option<u32> {
    let v = match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(v))
}

fn invalid(message: &str) -> VaultError {
    VaultError::new(ErrorCode::InvalidArgument, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 の 10 章のテストベクタ。
    const RFC4648_VECTORS: [(&str, &str); 7] = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn rfc4648_test_vectors_encode() {
        for (plain, encoded) in RFC4648_VECTORS {
            assert_eq!(encode(plain.as_bytes()), encoded, "encode({plain:?})");
        }
    }

    #[test]
    fn rfc4648_test_vectors_decode() {
        for (plain, encoded) in RFC4648_VECTORS {
            assert_eq!(
                decode(encoded).unwrap(),
                plain.as_bytes(),
                "decode({encoded:?})"
            );
        }
    }

    #[test]
    fn round_trip_all_byte_values_and_lengths() {
        let all: Vec<u8> = (0..=255).collect();
        for len in 0..=all.len() {
            let data = &all[..len];
            let text = encode(data);
            assert_eq!(text.len(), len.div_ceil(3) * 4);
            assert_eq!(decode(&text).unwrap(), data);
        }
        assert_eq!(encode(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn rejects_malformed_input() {
        let bad = [
            "Zg=",       // 長さが4の倍数でない
            "Zg",        // パディングがない
            "Zm9vY",     // 長さが4の倍数でない
            "Z===",      // パディングが多すぎる
            "====",      // パディングだけ
            "=Zg=",      // 先頭のパディング
            "Zg==Zg==",  // 途中のパディング
            "Zm=v",      // 途中のパディング
            "Zg=!",      // 知らない文字
            "Z m9",      // 空白
            "Zm9v\n",    // 改行
            "Zm9v\r\n",  // 改行
            "-_-_",      // URL 用の文字
            "Zh==",      // 使われないビットが0でない（1バイト）
            "Zm9=",      // 使われないビットが0でない（2バイト）
            "Zm9vYmFy=", // 長さが4の倍数でない
            "ＡＡＡＡ",  // 全角の文字
        ];
        for text in bad {
            let err = decode(text).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidArgument, "decode({text:?})");
        }
    }
}
