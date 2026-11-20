//! ログの決まりを型で守る小さな部品。依存はない。
//!
//! - [`Line`]：ログの1行（`source: k=v k=v`）。受けるのは、名前（[`Name`]）・数・かかった時間・はい／いいえ・
//!   入出力のエラーの種類だけ。文字列・バイト列・`Display` の値を受ける口はない。
//! - [`Name`]：ログに書いてよい名前。[`name!`] でソースに書いた文字からだけ作れる（実行のときの文字列からは
//!   作れない）。相手が送った文字は [`Line::one_of`]・[`pick`] で一覧と照らし、一覧の側の名前だけを書く。
//! - [`set_quiet_panic_hook`]：panic の知らせを、決まった1行（と、望めば場所）だけにする。
//! - [`Secret`]：秘密の値の包み。表示・写し・比べることをさせない。
//!
//! 書き出す先（stderr・ファイル）は、使う側が決める（`writeln!(log, "{line}")`）。ファイルへの出し先は (n) で作る。
//! 秘密を出さない保証は、名前を定数からしか作れないこと・相手の文字を一覧と照らすこと・ソースの検査
//! （`tests/source_check.rs`：`println!` などの直の書き出しと、ビルドする PC の値を取り込むマクロを止める）で持つ。
//! 名前の形の検査（[`is_name`]）は書式の守り（改行・区切り・パス・IP・長い鍵の形を止める）。
#![forbid(unsafe_code)]

mod line;
mod name;
mod panic;
mod secret;

pub use line::Line;
pub use name::{Literal, MAX_ALNUM_RUN, MAX_NAME_LEN, Name, is_name, pick};
pub use panic::set_quiet_panic_hook;
pub use secret::Secret;
