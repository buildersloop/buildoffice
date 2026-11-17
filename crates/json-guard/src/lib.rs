//! JSON を読む前の見張り：読み込んだ値（serde_json の Value）は入力より何十倍も大きくなるので、
//! 読む前に要素の数を数え、多すぎれば読まずに断るための関数。
//!
//! mcp-proto（MCP の要求）と vault-ffi（アプリの要求）で共有する。Phase 1 の daemon-ipc も使う予定。
//! 依存はない。JSON として正しいかは見ない（数えるだけ。正しさは、あとで serde_json が確かめる）。
#![forbid(unsafe_code)]

/// 1つの要求の中で、文字列の外にある `[`・`{`・`,` の数の上限（mcp-proto と vault-ffi で同じ値）。
pub const MAX_ELEMENTS: usize = 1_000_000;

/// 文字列の外にある `[`・`{`・`,` の数が `max` を超えるか（1回だけ走査し、超えたところで止める）。
///
/// 文字列の中の記号は数えない。文字列の中の `\"`（エスケープした引用符）で文字列は終わらず、`\\` のあとの `"` で終わる。
/// 壊れた JSON（閉じていない文字列など）でも、落ちずに数えられたところまでで判定する。
pub fn too_many_elements(bytes: &[u8], max: usize) -> bool {
    let mut count = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &b in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'[' | b'{' | b',' => {
                    count += 1;
                    if count > max {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上限ちょうどは超えない。1つ多ければ超える。
    #[test]
    fn exactly_at_the_limit_and_over() {
        assert!(!too_many_elements(b"[1,2,3]", 3));
        assert!(too_many_elements(b"[1,2,3,4]", 3));
        assert!(!too_many_elements(b"{}", 1));
        assert!(too_many_elements(b"{[]}", 1));
        assert!(!too_many_elements(b"", 0));
        assert!(!too_many_elements(b"1", 0));
        assert!(too_many_elements(b"[", 0));
        assert_eq!(MAX_ELEMENTS, 1_000_000);
    }

    /// 文字列の中の記号は数えない。
    #[test]
    fn symbols_inside_strings_are_not_counted() {
        assert!(!too_many_elements(br#"{"a":"[,{,,,[["}"#, 1));
        assert!(!too_many_elements(br#"["[","{",","]"#, 3));
        assert!(too_many_elements(br#"["[","{",","]"#, 2));
    }

    /// エスケープした引用符で、文字列は終わらない。`\\` のあとの引用符で、文字列は終わる。
    #[test]
    fn escapes_inside_strings() {
        assert!(!too_many_elements(br#"{"a":"\",[,,"}"#, 1));
        assert!(too_many_elements(br#"{"a":"\\",[,,"}"#, 2));
        assert!(!too_many_elements(br#"{"a":"\\\",[,"}"#, 1));
    }

    /// 壊れた JSON でも落ちない（閉じていない文字列のあとは数えない。閉じていない括弧は数える）。
    #[test]
    fn broken_json_does_not_panic() {
        assert!(!too_many_elements(br#"{"a":"[,,,[[["#, 1));
        assert!(too_many_elements(b"[[[[[[", 5));
        assert!(!too_many_elements(b"\"\\", 0));
        assert!(!too_many_elements(&[0xff, 0xfe, b'"', b'['], 0));
        assert!(too_many_elements(&[0xff, b',', 0xfe, b','], 1));
    }

    /// 大きな入力でも、超えたところで止まる。
    #[test]
    fn stops_at_the_first_excess() {
        let mut big = vec![b'['];
        big.extend(std::iter::repeat_n(b',', 5_000_000));
        assert!(too_many_elements(&big, MAX_ELEMENTS));
        let mut fits = vec![b'['];
        fits.extend(std::iter::repeat_n(b',', MAX_ELEMENTS - 1));
        assert!(!too_many_elements(&fits, MAX_ELEMENTS));
    }
}
