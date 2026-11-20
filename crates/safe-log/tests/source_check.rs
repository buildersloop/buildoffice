//! ソースの検査：本番のコード（`crates/*/src/**/*.rs` と `crates/*/build.rs`）が、ログの決まりを
//! 通らずに書き出していないこと。
//!
//! - 止めるもの（識別子）：`println`・`eprintln`・`print`・`eprint`・`dbg`・`stderr`・`stdout`・`Stderr`・`Stdout`・
//!   `StderrLock`・`StdoutLock`・`OutputDebugStringW`・`OutputDebugStringA`・`GetStdHandle`・`WriteFile`・
//!   `WriteConsoleW`・`WriteConsoleA`・`leak`、ビルドする PC の値を取り込むマクロ（`option_env`・`include_str`・
//!   `include_bytes`・`include!`、`env!` は決まった名前（`ALLOWED_ENV`）のほか）、`env` の別名（`env as`・
//!   `env::{self as …}`。`std::env` を別名で読むと `env!` も別名で使えるため）。`!` と `io::` は外して、識別子で照らす。
//! - 止めるもの（文字列の中身）：`CON`・`…\CON`・`CONOUT$`・`/dev/stderr`・`/dev/stdout`・`/dev/tty`・`/dev/fd/`・
//!   `/dev/console`・`/dev/pts`・`/proc/self/fd/`。
//! - `Cargo.toml` の `build = …`（`build.rs` でないビルドスクリプト）は、調べられないので失敗にする。
//! - 飛ばすのは、`#[cfg(test)]` のすぐ次が `mod <名前> {` の形のものだけ（その波かっこの中）。ほかの
//!   `#[cfg(test)]`（関数・`mod x;`・ほかの属性が挟まるもの）は飛ばさない。
//! - 文字列・文字・raw 文字列・コメントを読み飛ばして波かっこを数え、数が合わなければ失敗にする（安全な側）。
//! - 許可リストは「ファイル・見つかる数・理由」。数が合わなければ失敗（増えても減っても）。
//!
//! 検査の限界（レビューで見る）：マクロの別名（`macro_rules!` で包む）、ほかのクレートの関数で
//! 書き出すこと、分けて書いた文字列（`concat!`）とエスケープで書いた文字列、子のプロセス（`Command` の既定で stdio を
//! 受け継ぐ）、os-sys の `from_raw_handle`、`count` に秘密の数を渡すこと（`Secret` とキーの名前の検査で減らす）、
//! 許可リストは数で照らすので、同じ数のまま中身を置き換えても通ること。

use std::fs;
use std::path::{Path, PathBuf};

/// 書き出しを許すファイル（リポジトリの根からの相対、`/` 区切り）・見つかる数・理由。
const ALLOWED: &[(&str, usize, &str)] = &[
    (
        "crates/safe-log/src/panic.rs",
        1,
        "panic の知らせの決まった1行を stderr に書く所（この crate の役目。io::stderr を1回）",
    ),
    (
        "crates/vault-mcp/src/lib.rs",
        7,
        "コマンドの案内（使い方・引数の誤り・init と open の結果。画面の文として扱う）を eprintln で5回と、MCP の stdout とログの stderr を serve につなぐ所（stdout・stderr を1回ずつ）",
    ),
    (
        "crates/os-sys/src/win/testdir.rs",
        4,
        "テストだけの道具（win/mod.rs が #[cfg(test)] mod testdir; で読む。ファイルごとテスト用）。飛ばした知らせの println、テストの場所を決める env!(\"CARGO_MANIFEST_DIR\")、mklink の結果（Output の stdout・stderr のフィールド）を読む所",
    ),
    (
        "crates/vault-core/src/relpath.rs",
        2,
        "Windows の予約された装置の名前の一覧（\"CON\"・\"CONOUT$\"）。パスとして拒む側で、書き出しはしない",
    ),
];

/// `env!` で読んでよい名前（使っているものだけ。ビルドする PC の値を含まないもの）。
const ALLOWED_ENV: [&str; 1] = ["CARGO_PKG_VERSION"];

/// 止める識別子。
const IDENTS: [&str; 21] = [
    "println",
    "eprintln",
    "print",
    "eprint",
    "dbg",
    "stderr",
    "stdout",
    "Stderr",
    "Stdout",
    "StderrLock",
    "StdoutLock",
    "OutputDebugStringW",
    "OutputDebugStringA",
    "GetStdHandle",
    "WriteFile",
    "WriteConsoleW",
    "WriteConsoleA",
    "leak",
    "option_env",
    "include_str",
    "include_bytes",
];

/// 字句。
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    /// 文字列（ふつう・バイト・C・raw）の中身（エスケープは解かない）。
    Str(String),
    Punct(char),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: usize,
}

/// 字句に分ける。コメント・文字・ライフタイム・数は捨てる（波かっこを数えるのに要らない）。
/// 閉じていない文字列・コメントは誤り。
fn lex(src: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let at = |i: usize| chars.get(i).copied();
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            let start = line;
            let mut depth = 0;
            loop {
                match (at(i), at(i + 1)) {
                    (None, _) => return Err(format!("{start} 行のコメントが閉じていない")),
                    (Some('/'), Some('*')) => {
                        depth += 1;
                        i += 2;
                    }
                    (Some('*'), Some('/')) => {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    }
                    (Some(ch), _) => {
                        if ch == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                }
            }
        } else if c == '"' {
            let start = line;
            let (text, next, lines) = read_string(&chars, i + 1)
                .ok_or_else(|| format!("{start} 行の文字列が閉じていない"))?;
            tokens.push(Token {
                tok: Tok::Str(text),
                line: start,
            });
            line += lines;
            i = next;
        } else if c == '\'' {
            if at(i + 1) == Some('\\') {
                // エスケープのある文字：`\` と次の1字を飛ばし、閉じる `'` まで
                i += 3;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if at(i + 2) == Some('\'') {
                // 1字の文字（`'{'`・`'"'` など）
                i += 3;
            } else {
                // ライフタイム・ラベル
                i += 1;
                while i < chars.len() && is_ident_char(chars[i]) {
                    i += 1;
                }
            }
        } else if c.is_ascii_digit() {
            while i < chars.len()
                && (is_ident_char(chars[i])
                    || (chars[i] == '.' && at(i + 1).is_some_and(|d| d.is_ascii_digit())))
            {
                i += 1;
            }
        } else if is_ident_char(c) {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            let raw_prefix = matches!(ident.as_str(), "r" | "br" | "cr");
            let plain_prefix = matches!(ident.as_str(), "b" | "c");
            if raw_prefix && matches!(at(i), Some('"' | '#')) {
                let mut hashes = 0;
                while at(i + hashes) == Some('#') {
                    hashes += 1;
                }
                if at(i + hashes) == Some('"') {
                    let begin = line;
                    let (text, next, lines) = read_raw_string(&chars, i + hashes + 1, hashes)
                        .ok_or_else(|| format!("{begin} 行の raw 文字列が閉じていない"))?;
                    tokens.push(Token {
                        tok: Tok::Str(text),
                        line: begin,
                    });
                    line += lines;
                    i = next;
                } else if ident == "r" && hashes == 1 {
                    // raw の識別子（`r#type`）
                    let begin = i + 1;
                    i = begin;
                    while i < chars.len() && is_ident_char(chars[i]) {
                        i += 1;
                    }
                    tokens.push(Token {
                        tok: Tok::Ident(chars[begin..i].iter().collect()),
                        line,
                    });
                } else {
                    return Err(format!("{line} 行の raw 文字列の形が読めない"));
                }
            } else if plain_prefix && at(i) == Some('"') {
                let begin = line;
                let (text, next, lines) = read_string(&chars, i + 1)
                    .ok_or_else(|| format!("{begin} 行の文字列が閉じていない"))?;
                tokens.push(Token {
                    tok: Tok::Str(text),
                    line: begin,
                });
                line += lines;
                i = next;
            } else {
                tokens.push(Token {
                    tok: Tok::Ident(ident),
                    line,
                });
            }
        } else {
            tokens.push(Token {
                tok: Tok::Punct(c),
                line,
            });
            i += 1;
        }
    }
    Ok(tokens)
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// `"` の次から、閉じる `"` まで読む。（中身、閉じる `"` の次の位置、中の改行の数）。
fn read_string(chars: &[char], mut i: usize) -> Option<(String, usize, usize)> {
    let start = i;
    let mut lines = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                if chars.get(i + 1) == Some(&'\n') {
                    lines += 1;
                }
                i += 2;
            }
            '"' => return Some((chars[start..i].iter().collect(), i + 1, lines)),
            ch => {
                if ch == '\n' {
                    lines += 1;
                }
                i += 1;
            }
        }
    }
    None
}

/// raw 文字列の `"` の次から、`"` と `#` が `hashes` 個続く所まで読む。
fn read_raw_string(chars: &[char], mut i: usize, hashes: usize) -> Option<(String, usize, usize)> {
    let start = i;
    let mut lines = 0;
    while i < chars.len() {
        if chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#')) {
            return Some((chars[start..i].iter().collect(), i + 1 + hashes, lines));
        }
        if chars[i] == '\n' {
            lines += 1;
        }
        i += 1;
    }
    None
}

/// 波かっこの数が合うか。合えば、各 `{` の対になる `}` の位置（字句の番号）。
fn match_braces(tokens: &[Token]) -> Result<Vec<Option<usize>>, String> {
    let mut pairs = vec![None; tokens.len()];
    let mut stack = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match token.tok {
            Tok::Punct('{') => stack.push(index),
            Tok::Punct('}') => {
                let open = stack
                    .pop()
                    .ok_or_else(|| format!("{} 行の }} に対になる {{ がない", token.line))?;
                pairs[open] = Some(index);
            }
            _ => {}
        }
    }
    if let Some(open) = stack.pop() {
        return Err(format!("{} 行の {{ が閉じていない", tokens[open].line));
    }
    Ok(pairs)
}

/// `index` から `#[cfg(test)] mod <名前> {` が始まるなら、その `{` の位置。
fn cfg_test_mod(tokens: &[Token], index: usize) -> Option<usize> {
    let want = [
        Tok::Punct('#'),
        Tok::Punct('['),
        Tok::Ident("cfg".to_owned()),
        Tok::Punct('('),
        Tok::Ident("test".to_owned()),
        Tok::Punct(')'),
        Tok::Punct(']'),
        Tok::Ident("mod".to_owned()),
    ];
    let rest = tokens.get(index..)?;
    if rest.len() < want.len() + 2 || rest.iter().zip(&want).any(|(token, tok)| token.tok != *tok) {
        return None;
    }
    match (&rest[want.len()].tok, &rest[want.len() + 1].tok) {
        (Tok::Ident(_), Tok::Punct('{')) => Some(index + want.len() + 1),
        _ => None,
    }
}

/// 字句が止めるものか。止めるなら、その説明。
fn flagged(tokens: &[Token], index: usize) -> Option<String> {
    match &tokens[index].tok {
        Tok::Ident(ident) if IDENTS.contains(&ident.as_str()) => Some(ident.clone()),
        Tok::Ident(ident) if ident == "env" => env_use(tokens, index),
        Tok::Ident(ident) if ident == "include" => {
            matches!(tok_at(tokens, index + 1), Some(Tok::Punct('!')))
                .then(|| "include!".to_owned())
        }
        Tok::Str(text) => {
            let upper = text.to_ascii_uppercase();
            let device = upper == "CON"
                || upper.ends_with("\\CON")
                || upper.contains("CONOUT$")
                || [
                    "/DEV/STDERR",
                    "/DEV/STDOUT",
                    "/DEV/TTY",
                    "/DEV/FD/",
                    "/DEV/CONSOLE",
                    "/DEV/PTS",
                    "/PROC/SELF/FD/",
                ]
                .iter()
                .any(|path| upper.contains(path));
            device.then(|| format!("\"{text}\""))
        }
        _ => None,
    }
}

fn tok_at(tokens: &[Token], index: usize) -> Option<&Tok> {
    tokens.get(index).map(|token| &token.tok)
}

/// 識別子 `env` の使い方を調べる。止めるなら、その説明。
/// - `env!(…)`：中が決まった名前（`ALLOWED_ENV`）の文字列1つだけなら許す。
/// - `env as …`・`env::{self as …}`：別名にすると `env!` を別名で使えるので止める。
/// - ほか（`std::env::var` などのパス）は対象外。
fn env_use(tokens: &[Token], index: usize) -> Option<String> {
    match tok_at(tokens, index + 1) {
        Some(Tok::Punct('!')) => {
            let close = match tok_at(tokens, index + 2) {
                Some(Tok::Punct('(')) => ')',
                Some(Tok::Punct('[')) => ']',
                Some(Tok::Punct('{')) => '}',
                _ => return Some("env!".to_owned()),
            };
            let allowed = matches!(tok_at(tokens, index + 3), Some(Tok::Str(text)) if ALLOWED_ENV.contains(&text.as_str()))
                && tok_at(tokens, index + 4) == Some(&Tok::Punct(close));
            (!allowed).then(|| "env!".to_owned())
        }
        Some(Tok::Ident(next)) if next == "as" => Some("env as".to_owned()),
        Some(Tok::Punct(':'))
            if tok_at(tokens, index + 2) == Some(&Tok::Punct(':'))
                && tok_at(tokens, index + 3) == Some(&Tok::Punct('{')) =>
        {
            // `env::{…}` の中に `self as` があれば止める（入れ子の波かっこも数える）
            let mut depth = 0;
            for i in index + 3..tokens.len() {
                match &tokens[i].tok {
                    Tok::Punct('{') => depth += 1,
                    Tok::Punct('}') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Tok::Ident(word)
                        if word == "self"
                            && matches!(tok_at(tokens, i + 1), Some(Tok::Ident(a)) if a == "as") =>
                    {
                        return Some("env::{self as}".to_owned());
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

/// 1つのソースを調べ、止めるものの一覧（行・説明）を返す。
fn scan(src: &str) -> Result<Vec<(usize, String)>, String> {
    let tokens = lex(src)?;
    let pairs = match_braces(&tokens)?;
    let mut hits = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if let Some(open) = cfg_test_mod(&tokens, index) {
            index = pairs[open].expect("波かっこは数え済み") + 1;
            continue;
        }
        if let Some(what) = flagged(&tokens, index) {
            hits.push((tokens[index].line, what));
        }
        index += 1;
    }
    Ok(hits)
}

/// リポジトリの根。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// `dir` の下の `.rs` を全部（並びは名前の順）。
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} を読めない：{e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// 調べるファイル：`crates_dir` の下の各クレートの `src/**/*.rs` と `build.rs`（並びは名前の順）。
/// `Cargo.toml` に `build = …`（ほかの名前のビルドスクリプト）があれば、`problems` に足す。
fn source_files(crates_dir: &Path, problems: &mut Vec<String>) -> Vec<PathBuf> {
    let mut crates: Vec<PathBuf> = fs::read_dir(crates_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    crates.sort();
    let mut files = Vec::new();
    for dir in &crates {
        if dir.join("src").is_dir() {
            rust_files(&dir.join("src"), &mut files);
        }
        if dir.join("build.rs").is_file() {
            files.push(dir.join("build.rs"));
        }
        if let Ok(manifest) = fs::read_to_string(dir.join("Cargo.toml"))
            && manifest.lines().any(|line| {
                let line = line.trim_start();
                line.strip_prefix("build")
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
        {
            problems.push(format!(
                "{}：Cargo.toml の build = …（build.rs でないビルドスクリプト）は検査できない",
                dir.display()
            ));
        }
    }
    files
}

/// 本番のコードに、許可リストの外の書き出しがない。許可リストの数はちょうど合い、どの行にも理由がある。
#[test]
fn production_sources_do_not_write_around_the_logger() {
    let root = repo_root();
    let mut problems = Vec::new();
    let files = source_files(&root.join("crates"), &mut problems);
    assert!(files.len() >= 40, "調べたファイルが少ない：{}", files.len());

    let mut seen = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let src = fs::read_to_string(file).unwrap();
        let hits = match scan(&src) {
            Ok(hits) => hits,
            Err(e) => {
                problems.push(format!("{rel}：読めない（{e}）"));
                continue;
            }
        };
        match ALLOWED.iter().find(|(path, _, _)| *path == rel) {
            Some((_, count, _)) => {
                seen.push(rel.clone());
                if hits.len() != *count {
                    problems.push(format!(
                        "{rel}：許可リストは {count} 件、見つかったのは {} 件：{hits:?}",
                        hits.len()
                    ));
                }
            }
            None => {
                for (line, what) in hits {
                    problems.push(format!("{rel}:{line}：{what}"));
                }
            }
        }
    }
    for (path, _, reason) in ALLOWED {
        if !seen.iter().any(|s| s == path) {
            problems.push(format!("許可リストの {path} が見つからない（消すか直す）"));
        }
        if reason.trim().is_empty() {
            problems.push(format!("許可リストの {path} に理由がない"));
        }
    }
    assert!(
        problems.is_empty(),
        "ログの決まりを通らない書き出し：直すか、理由を付けて許可リストに足す\n{}",
        problems.join("\n")
    );
}

fn hits(src: &str) -> Vec<String> {
    scan(src)
        .unwrap()
        .into_iter()
        .map(|(_, what)| what)
        .collect()
}

#[test]
fn scanner_finds_direct_writes() {
    assert_eq!(hits("fn f() { println!(\"x\"); }"), ["println"]);
    assert_eq!(
        hits("fn f() { eprint!(\"x\"); print!(\"y\"); }"),
        ["eprint", "print"]
    );
    assert_eq!(hits("fn f() { dbg!(1); }"), ["dbg"]);
    assert_eq!(
        hits("use std::io::{self, Write};\nfn f() { let _ = writeln!(io::stderr(), \"x\"); }"),
        ["stderr"]
    );
    assert_eq!(hits("use std::io::stdout as out;"), ["stdout"]);
    assert_eq!(
        hits("fn f(s: String) -> &'static str { s.leak() }"),
        ["leak"]
    );
    assert_eq!(
        hits("fn f(s: String) { Box::leak(Box::new(s)); }"),
        ["leak"]
    );
    assert_eq!(hits("const A: &str = env!(\"USERNAME\");"), ["env!"]);
    assert_eq!(
        hits("const A: &str = env!(\"CARGO_MANIFEST_DIR\");"),
        ["env!"]
    );
    assert!(hits("const A: &str = env!(\"CARGO_PKG_VERSION\");").is_empty());
    assert!(hits("fn f() { std::env::var(\"X\"); }").is_empty());
    assert!(hits("use std::env;\nfn f() { env::args_os(); }").is_empty());
    // 決まった名前のほかの CARGO_PKG_・分けて書いた名前・2つめの引数は止める
    for src in [
        "const A: &str = env!(\"CARGO_PKG_AUTHORS\");",
        "const A: &str = env!(\"CARGO_PKG_VERSION_X\");",
        "const A: &str = env!(concat!(\"CARGO_PKG_\", \"VERSION\"));",
        "const A: &str = env!(\"CARGO_PKG_VERSION\", \"x\");",
        "const A: &str = std::env![\"USER\"];",
        "const A: &str = core::env!{\"USER\"};",
    ] {
        assert_eq!(hits(src), ["env!"], "{src}");
    }
    // env の別名（`env!` も別名で使えてしまう）
    assert_eq!(
        hits("use std::env as e;\nconst A: &str = e!(\"USER\");"),
        ["env as"]
    );
    assert_eq!(hits("use core::env as e2;"), ["env as"]);
    assert_eq!(hits("use std::{env as e};"), ["env as"]);
    assert_eq!(hits("use std::env::{self as e};"), ["env::{self as}"]);
    assert!(hits("use std::env::{self, var};").is_empty());
    assert_eq!(
        hits("include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));"),
        ["include!", "env!"]
    );
    assert!(hits("fn include(x: u8) {}").is_empty());
    assert_eq!(
        hits("const A: Option<&str> = option_env!(\"USER\");"),
        ["option_env"]
    );
    assert_eq!(
        hits("const A: &str = include_str!(\"a.txt\");"),
        ["include_str"]
    );
    assert_eq!(hits("fn f() { open(\"CON\"); }"), ["\"CON\""]);
    assert_eq!(
        hits("fn f() { open(r\"\\\\.\\CONOUT$\"); }"),
        ["\"\\\\.\\CONOUT$\""]
    );
    assert_eq!(hits("fn f() { open(\"/dev/tty\"); }"), ["\"/dev/tty\""]);
    for path in [
        "/dev/console",
        "/dev/pts/0",
        "/proc/self/fd/2",
        "/PROC/SELF/FD/1",
    ] {
        let src = format!("fn f() {{ open(\"{path}\"); }}");
        assert_eq!(hits(&src), [format!("\"{path}\"")], "{path}");
    }
    assert_eq!(
        hits("extern \"system\" { fn GetStdHandle(n: u32) -> isize; }"),
        ["GetStdHandle"]
    );
    assert_eq!(hits("fn f() { r#println!(\"x\"); }"), ["println"]);
}

#[test]
fn scanner_skips_comments_strings_and_chars() {
    let src = r##"
        // println!("comment")
        /* eprintln!("block /* nested */ comment") */
        /// doc: dbg!(x)
        fn f() {
            let a = "println!(\"in a string\") { {";
            let b = r#"eprintln!("raw") }"#;
            let c = b"stderr";
            let d = '{';
            let e = '"';
            let f = '\'';
            let g = '\u{7b}';
            let h: &'static str = "x";
            'outer: loop { break 'outer; }
            let i = 1.0e3_f64;
        }
    "##;
    assert_eq!(hits(src), Vec::<String>::new());
}

#[test]
fn scanner_skips_only_exact_cfg_test_modules() {
    let src = "
        fn a() {}
        #[cfg(test)]
        mod tests {
            fn t() { println!(\"ok in tests\"); let s = \"}\"; }
        }
        fn b() {}
    ";
    assert!(hits(src).is_empty());
    // 関数・`mod x;`・ほかの属性が挟まるものは飛ばさない
    let src = "#[cfg(test)]\nfn t() { println!(\"x\"); }";
    assert_eq!(hits(src), ["println"]);
    let src = "#[cfg(test)]\n#[allow(dead_code)]\nmod tests { fn t() { println!(\"x\"); } }";
    assert_eq!(hits(src), ["println"]);
    let src = "#[cfg(any(test, feature = \"x\"))]\nmod tests { fn t() { println!(\"x\"); } }";
    assert_eq!(hits(src), ["println"]);
    // 飛ばしたあとのコードは調べる
    let src = "#[cfg(test)]\nmod tests { }\nfn f() { println!(\"x\"); }";
    assert_eq!(hits(src), ["println"]);
}

/// `build.rs` も調べる。`build = …` は失敗にする（写しのクレートで確かめる）。
#[test]
fn build_scripts_are_scanned_too() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("safe-log-source-check")
        .join(format!("{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let plain = base.join("crates").join("plain");
    let built = base.join("crates").join("built");
    let other = base.join("crates").join("other");
    for dir in [&plain, &built, &other] {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src").join("lib.rs"), "pub fn f() {}\n").unwrap();
    }
    fs::write(
        plain.join("Cargo.toml"),
        "[package]\nname = \"plain\"\n[build-dependencies]\n",
    )
    .unwrap();
    fs::write(built.join("Cargo.toml"), "[package]\nname = \"built\"\n").unwrap();
    fs::write(
        built.join("build.rs"),
        "fn main() { println!(\"cargo:rustc-env=WHO={}\", std::env::var(\"USERNAME\").unwrap()); }\n",
    )
    .unwrap();
    fs::write(
        other.join("Cargo.toml"),
        "[package]\nname = \"other\"\nbuild = \"gen.rs\"\n",
    )
    .unwrap();

    let mut problems = Vec::new();
    let files = source_files(&base.join("crates"), &mut problems);
    let build_rs = built.join("build.rs");
    assert!(files.contains(&build_rs), "{files:?}");
    assert_eq!(files.len(), 4, "{files:?}");
    let src = fs::read_to_string(&build_rs).unwrap();
    assert_eq!(hits(&src), ["println"]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("other"), "{problems:?}");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn scanner_fails_closed_on_unbalanced_sources() {
    assert!(scan("fn f() {").is_err());
    assert!(scan("fn f() }").is_err());
    assert!(scan("fn f() { \"open string }").is_err());
    assert!(scan("fn f() { /* open comment }").is_err());
    assert!(scan("fn f() { r#\"open raw }").is_err());
}
