//! OS の関数を、安全な関数として包むクレート。
//!
//! - 依存を持たない。OS の関数の宣言は自分で書き、SDK のヘッダと突き合わせた（`win/ffi.rs`）。
//! - 公開する関数は、すべて安全な関数（unsafe の公開の関数を作らない）。生のポインタ・生のハンドルを外に出さず、
//!   std の `File` と、このクレートの型（[`Dir`]・[`PassDir`]・[`Opened`]・[`Created`]・[`Parent`]）で受け取り、返す。
//! - unsafe のブロックは、外の関数を呼ぶ所と、OS が返したハンドルを包む所だけ。ブロックごとに `// SAFETY:` を書く。
//! - Windows（64 ビット）だけ。Windows 以外では中身がなく、unsafe を書かない。
//!
//! 何に使うか：
//! - 親のフォルダのハンドルを基準に、リンクをたどらずに開く・作る（[`open_child`]・[`open_pass_dir`]・
//!   [`create_dir_child`]・[`create_file_child`]。NtCreateFile）。
//! - 開いたハンドルの最終のパス（[`final_path`]）・ファイルの ID（[`file_id`]）・リンクの数（[`link_count`]）と、
//!   ハンドルでの列挙（[`read_dir_by_handle`]）。
//! - ハンドルでの rename（[`rename_no_replace`]・[`rename_replace`]。NtSetInformationFile）と、ハンドルで
//!   消す処理（[`delete_by_handle`]）。
//! - 空き容量（[`free_space`]）と、OS の乱数（[`os_random`]）。
//! - 既知のフォルダの場所（[`known_folder`]）。守る場所を、環境変数だけでなく OS の値でも決めるため

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

// 構造体の大きさと項目の位置は、x64 で確かめた（win/ffi.rs のテスト）。32 ビットの Windows には対応しない。
#[cfg(all(windows, not(target_pointer_width = "64")))]
compile_error!(
    "os-sys は 64 ビットの Windows だけを対象にする（構造体の位置を x64 で確かめたため）"
);

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{
    Access, Created, Dir, DirEntry, EntryKind, FileId, InvalidName, KnownFolder, Links, Name,
    Opened, OpenedPass, Parent, PassDir, Share, Want, create_dir_child, create_file_child,
    delete_by_handle, file_id, final_path, free_space, is_reparse_point_encountered, known_folder,
    link_count, open_child, open_pass_dir, open_path, os_random, read_dir_by_handle,
    rename_no_replace, rename_replace,
};
