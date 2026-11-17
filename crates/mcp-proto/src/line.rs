//! stdin から1行（改行区切りの1つのメッセージ）を、大きさの上限を守りながら読む。

use std::io::{self, BufRead};

/// 1行の大きさの上限（改行を除いて 32 MiB）。
pub const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

/// 1行を読んだ結果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Line {
    /// 1行を読んだ（`buf` に入っている。改行は含まない）。最後の行が改行で終わっていなくても、これになる。
    Complete,
    /// 上限を超えた。改行（か EOF）まで読み捨てた。`buf` は空。
    TooLong,
    /// もう何もない（EOF）。
    Eof,
}

/// 1行を `buf` に読む。上限を超えたら、それ以上はためずに、改行まで読み捨てる（メモリを使い切らない）。
pub(crate) fn read_line(
    input: &mut impl BufRead,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<Line> {
    buf.clear();
    let mut too_long = false;
    let mut read_any = false;
    loop {
        let available = match input.fill_buf() {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok(match (read_any, too_long) {
                (false, _) => Line::Eof,
                (true, true) => Line::TooLong,
                (true, false) => Line::Complete,
            });
        }
        read_any = true;
        let newline = available.iter().position(|&b| b == b'\n');
        let chunk = &available[..newline.unwrap_or(available.len())];
        if !too_long {
            if buf.len() + chunk.len() > max {
                too_long = true;
                buf.clear();
                buf.shrink_to_fit();
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        let used = newline.map_or(available.len(), |i| i + 1);
        input.consume(used);
        if newline.is_some() {
            return Ok(if too_long {
                Line::TooLong
            } else {
                Line::Complete
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    fn lines(input: &[u8], max: usize, capacity: usize) -> Vec<(Line, Vec<u8>)> {
        let mut reader = BufReader::with_capacity(capacity, Cursor::new(input.to_vec()));
        let mut buf = Vec::new();
        let mut out = Vec::new();
        loop {
            let line = read_line(&mut reader, &mut buf, max).unwrap();
            if line == Line::Eof {
                return out;
            }
            out.push((line, buf.clone()));
        }
    }

    #[test]
    fn reads_lines_and_the_last_line_without_a_newline() {
        let got = lines(b"ab\ncd\n\nef", 10, 3);
        assert_eq!(
            got,
            [
                (Line::Complete, b"ab".to_vec()),
                (Line::Complete, b"cd".to_vec()),
                (Line::Complete, b"".to_vec()),
                (Line::Complete, b"ef".to_vec()),
            ]
        );
        assert!(lines(b"", 10, 3).is_empty());
    }

    /// 上限を超えた行は、改行まで読み捨てて TooLong。次の行はふつうに読める。ちょうど上限なら読める。
    #[test]
    fn long_lines_are_cut_while_reading() {
        let mut input = vec![b'x'; 25];
        input.push(b'\n');
        input.extend_from_slice(b"0123456789\nok");
        let got = lines(&input, 10, 4);
        assert_eq!(
            got,
            [
                (Line::TooLong, Vec::new()),
                (Line::Complete, b"0123456789".to_vec()),
                (Line::Complete, b"ok".to_vec()),
            ]
        );
        // 改行のないまま EOF でも TooLong
        assert_eq!(lines(&[b'y'; 11], 10, 4), [(Line::TooLong, Vec::new())]);
        assert_eq!(MAX_LINE_BYTES, 32 * 1024 * 1024);
    }
}
