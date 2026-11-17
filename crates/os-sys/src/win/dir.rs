//! ハンドルでフォルダの中を列挙する（W1。GetFileInformationByHandleEx）。
//!
//! - FileIdExtdDirectoryInfo（19・20）を先に使い、最初の呼び出しで 87・50・1 が返ったときだけ
//!   FileFullDirectoryInfo（14・15）に切り替える。切り替えたことは覚えず、フォルダごとに最初の呼び出しで決める。
//! - 最初の呼び出しは Restart の種類（頭から）、そのあとは続きの種類。ERROR_NO_MORE_FILES（18）で終わる。
//! - バッファは 64 KiB を 8 バイトにそろえた `Vec<u64>` で持ち、OS が書いたあと、バイト列に写して
//!   安全なコードで読む。GetFileInformationByHandleEx は書いたバイト数を返さないので、境はバッファの長さで取る。

use std::ffi::OsString;
use std::io;
use std::mem::size_of_val;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::AsRawHandle;

use super::ffi;
use super::{Dir, is_unsupported};

/// 1回の呼び出しで使うバッファの大きさ（u64 の数。64 KiB）。
const BUFFER_WORDS: usize = 64 * 1024 / 8;

/// FILETIME（1601-01-01 からの 100 ナノ秒）と UNIX 時刻（1970-01-01）の差（100 ナノ秒の単位）。
const UNIX_EPOCH_AS_FILETIME: i128 = 116_444_736_000_000_000;

/// 列挙で得た1つの項目（"." と ".." は含めない）。値は列挙の情報のもの（ハンドルで開いて読み直していない）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    name: OsString,
    attributes: u32,
    reparse_tag: Option<u32>,
    end_of_file: u64,
    last_write_time: i64,
    file_id: Option<[u8; 16]>,
}

/// 項目の種類（std と同じ決め方）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// ふつうのファイル（名前の代理でないリパースポイントのファイルも入る）。
    File,
    /// フォルダ（名前の代理でないリパースポイントのフォルダも入る）。
    Dir,
    /// 名前の代理のリパースポイント（ジャンクション・シンボリックリンク）。タグが分からないリパースポイントも入る。
    Link,
}

impl DirEntry {
    /// 名前。
    pub fn name(&self) -> &OsString {
        &self.name
    }

    /// 属性（FILE_ATTRIBUTE_*）。
    pub fn attributes(&self) -> u32 {
        self.attributes
    }

    /// リパースのタグ（リパースポイントのときだけ）。
    pub fn reparse_tag(&self) -> Option<u32> {
        self.reparse_tag
    }

    /// 大きさ（EndOfFile。バイト）。
    pub fn end_of_file(&self) -> u64 {
        self.end_of_file
    }

    /// 更新時刻（FILETIME の値。1601-01-01 からの 100 ナノ秒）。
    pub fn last_write_time(&self) -> i64 {
        self.last_write_time
    }

    /// 128 ビットのファイルの ID（FileIdExtdDirectoryInfo で読んだときだけ）。
    pub fn file_id(&self) -> Option<[u8; 16]> {
        self.file_id
    }

    /// 種類。リパースポイントで、タグが名前の代理か、タグが分からなければリンク。
    pub fn kind(&self) -> EntryKind {
        if self.attributes & ffi::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            match self.reparse_tag {
                Some(tag) if tag & ffi::REPARSE_TAG_NAME_SURROGATE == 0 => {}
                _ => return EntryKind::Link,
            }
        }
        if self.attributes & ffi::FILE_ATTRIBUTE_DIRECTORY != 0 {
            EntryKind::Dir
        } else {
            EntryKind::File
        }
    }

    /// 更新時刻を UNIX 時刻のナノ秒で（vault-core の mtime_ns と同じ値。あふれたら i64 の端に寄せる）。
    pub fn modified_unix_ns(&self) -> i64 {
        filetime_to_unix_ns(self.last_write_time)
    }
}

/// FILETIME を UNIX 時刻のナノ秒にする（(FILETIME − 116444736000000000) × 100。i128 で計算して i64 に寄せる）。
fn filetime_to_unix_ns(filetime: i64) -> i64 {
    let ns = (i128::from(filetime) - UNIX_EPOCH_AS_FILETIME) * 100;
    i64::try_from(ns).unwrap_or(if ns < 0 { i64::MIN } else { i64::MAX })
}

/// 情報の種類（列挙で読む構造体）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// FILE_ID_EXTD_DIR_INFO（ファイルの ID とリパースのタグがある）。
    IdExtd,
    /// FILE_FULL_DIR_INFO（リパースポイントのとき、EaSize にリパースのタグが入る）。
    Full,
}

impl Mode {
    fn class(self, restart: bool) -> i32 {
        match (self, restart) {
            (Self::IdExtd, true) => ffi::CLASS_ID_EXTD_DIRECTORY_RESTART_INFO,
            (Self::IdExtd, false) => ffi::CLASS_ID_EXTD_DIRECTORY_INFO,
            (Self::Full, true) => ffi::CLASS_FULL_DIRECTORY_RESTART_INFO,
            (Self::Full, false) => ffi::CLASS_FULL_DIRECTORY_INFO,
        }
    }

    /// FileName の位置（見出しの大きさ）。
    fn name_at(self) -> usize {
        match self {
            Self::IdExtd => ffi::ID_EXTD_NAME_AT,
            Self::Full => ffi::FULL_NAME_AT,
        }
    }
}

/// フォルダの中を、持っているハンドルで頭から列挙する（"." と ".." は除く）。
///
/// `&mut` を受け取るので、同じ [`Dir`] で2つの列挙を同時にできない。`dir` は LIST_DIRECTORY の権利で
/// 開いたもの。
pub fn read_dir_by_handle(dir: &mut Dir) -> io::Result<Vec<DirEntry>> {
    read_dir_mode(dir, Mode::IdExtd)
}

/// 最初の情報の種類を選べる中の関数（`Mode::Full` を渡すと、切り替えの道をテストできる）。
pub(crate) fn read_dir_mode(dir: &mut Dir, first: Mode) -> io::Result<Vec<DirEntry>> {
    let mut words = vec![0u64; BUFFER_WORDS];
    let mut mode = first;
    let mut restart = true;
    let mut entries = Vec::new();
    loop {
        match query(dir, mode.class(restart), &mut words) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(ffi::ERROR_NO_MORE_FILES) => return Ok(entries),
            Err(e) if restart && mode == Mode::IdExtd && is_unsupported(&e) => {
                mode = Mode::Full;
                continue;
            }
            Err(e) => return Err(e),
        }
        restart = false;
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_ne_bytes()).collect();
        parse_entries(&bytes, mode, &mut entries)?;
    }
}

fn query(dir: &mut Dir, class: i32, words: &mut [u64]) -> io::Result<()> {
    let size = u32::try_from(size_of_val(words))
        .map_err(|_| io::Error::other("列挙のバッファが大きすぎる"))?;
    // SAFETY: ハンドルは `dir` から借りた有効なもの。`words` は 8 バイトにそろった `size` バイトの領域で、
    // OS は `size` バイトを超えて書かない（_Out_writes_bytes_(dwBufferSize)）。`class` は列挙の種類
    // （FILE_ID_EXTD_DIR_INFO か FILE_FULL_DIR_INFO を並べて書く）で、8 バイトのそろえで足りる。
    let ok = unsafe {
        ffi::GetFileInformationByHandleEx(
            dir.as_file().as_raw_handle(),
            class,
            words.as_mut_ptr().cast(),
            size,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn corrupt() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "フォルダの一覧の情報が壊れている（範囲の外を指している）",
    )
}

fn read_bytes<const N: usize>(bytes: &[u8], at: usize) -> io::Result<[u8; N]> {
    let end = at.checked_add(N).ok_or_else(corrupt)?;
    let slice = bytes.get(at..end).ok_or_else(corrupt)?;
    slice.try_into().map_err(|_| corrupt())
}

fn read_u32(bytes: &[u8], at: usize) -> io::Result<u32> {
    read_bytes::<4>(bytes, at).map(u32::from_ne_bytes)
}

fn read_i64(bytes: &[u8], at: usize) -> io::Result<i64> {
    read_bytes::<8>(bytes, at).map(i64::from_ne_bytes)
}

/// OS が書いたバッファ（バイト列）から項目を読む。1つでも範囲の外を指していたら、その回は Err。
///
/// 項目ごとに確かめること（checked_add で）：
/// - オフセット＋見出しの大きさ（FileName の位置まで）≤ 長さ。
/// - FileNameLength が偶数。
/// - オフセット＋見出し＋FileNameLength ≤ 長さ。
/// - NextEntryOffset が 0 でなければ、見出し＋FileNameLength 以上（次の項目が前へ進む）。
///
/// 読めた項目は、全部を確かめてから `out` に足す（Err のときは何も足さない）。
pub(crate) fn parse_entries(bytes: &[u8], mode: Mode, out: &mut Vec<DirEntry>) -> io::Result<()> {
    let header = mode.name_at();
    let mut at = 0usize;
    let mut found = Vec::new();
    loop {
        let name_at = at.checked_add(header).ok_or_else(corrupt)?;
        if name_at > bytes.len() {
            return Err(corrupt());
        }
        let next = read_u32(bytes, at + ffi::DIR_NEXT_AT)?;
        let name_len = usize::try_from(read_u32(bytes, at + ffi::DIR_NAME_LENGTH_AT)?)
            .map_err(|_| corrupt())?;
        if name_len % 2 != 0 || name_len == 0 {
            return Err(corrupt());
        }
        let name_end = name_at.checked_add(name_len).ok_or_else(corrupt)?;
        if name_end > bytes.len() {
            return Err(corrupt());
        }
        let attributes = read_u32(bytes, at + ffi::DIR_ATTRIBUTES_AT)?;
        let last_write_time = read_i64(bytes, at + ffi::DIR_LAST_WRITE_AT)?;
        let end_of_file =
            u64::try_from(read_i64(bytes, at + ffi::DIR_END_OF_FILE_AT)?).map_err(|_| corrupt())?;
        let (tag_value, file_id) = match mode {
            Mode::IdExtd => (
                read_u32(bytes, at + ffi::ID_EXTD_TAG_AT)?,
                Some(read_bytes::<16>(bytes, at + ffi::ID_EXTD_FILE_ID_AT)?),
            ),
            // FILE_FULL_DIR_INFO では、リパースポイントのとき EaSize にタグが入る
            Mode::Full => (read_u32(bytes, at + ffi::FULL_EA_SIZE_AT)?, None),
        };
        let reparse_tag =
            (attributes & ffi::FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(tag_value);
        // name_len は偶数と確かめたので、2 バイトずつに割り切れる
        let wide: Vec<u16> = bytes[name_at..name_end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_ne_bytes(pair))
            .collect();
        let dot = u16::from(b'.');
        if wide != [dot] && wide != [dot, dot] {
            found.push(DirEntry {
                name: OsString::from_wide(&wide),
                attributes,
                reparse_tag,
                end_of_file,
                last_write_time,
                file_id,
            });
        }
        if next == 0 {
            out.append(&mut found);
            return Ok(());
        }
        let next = usize::try_from(next).map_err(|_| corrupt())?;
        // header + name_len は name_end − at と同じで、あふれない
        if next < header + name_len {
            return Err(corrupt());
        }
        at = at.checked_add(next).ok_or_else(corrupt)?;
    }
}

#[cfg(test)]
mod tests {
    use super::super::testdir::{UnitDir, junction};
    use super::super::{Access, Links, Opened, Share, open_path};
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::UNIX_EPOCH;

    /// 項目を1つ、バイト列に書く（テストのため）。
    fn entry_bytes(mode: Mode, next: u32, name: &str, attributes: u32, tag: u32) -> Vec<u8> {
        let wide: Vec<u16> = name.encode_utf16().collect();
        let header = mode.name_at();
        let mut b = vec![0u8; (header + wide.len() * 2).div_ceil(8) * 8];
        b[0..4].copy_from_slice(&next.to_ne_bytes());
        b[ffi::DIR_LAST_WRITE_AT..ffi::DIR_LAST_WRITE_AT + 8]
            .copy_from_slice(&UNIX_EPOCH_AS_FILETIME_I64.to_ne_bytes());
        b[ffi::DIR_END_OF_FILE_AT..ffi::DIR_END_OF_FILE_AT + 8]
            .copy_from_slice(&5i64.to_ne_bytes());
        b[ffi::DIR_ATTRIBUTES_AT..ffi::DIR_ATTRIBUTES_AT + 4]
            .copy_from_slice(&attributes.to_ne_bytes());
        b[ffi::DIR_NAME_LENGTH_AT..ffi::DIR_NAME_LENGTH_AT + 4]
            .copy_from_slice(&((wide.len() * 2) as u32).to_ne_bytes());
        let tag_at = match mode {
            Mode::IdExtd => ffi::ID_EXTD_TAG_AT,
            Mode::Full => ffi::FULL_EA_SIZE_AT,
        };
        b[tag_at..tag_at + 4].copy_from_slice(&tag.to_ne_bytes());
        for (i, u) in wide.iter().enumerate() {
            b[header + i * 2..header + i * 2 + 2].copy_from_slice(&u.to_ne_bytes());
        }
        b
    }

    const UNIX_EPOCH_AS_FILETIME_I64: i64 = 116_444_736_000_000_000;

    #[test]
    fn parse_well_formed_buffer() {
        for mode in [Mode::IdExtd, Mode::Full] {
            let first = entry_bytes(mode, 0, ".", ffi::FILE_ATTRIBUTE_DIRECTORY, 0);
            let mut bytes = first.clone();
            let next = u32::try_from(first.len()).unwrap();
            bytes[0..4].copy_from_slice(&next.to_ne_bytes());
            let second = entry_bytes(
                mode,
                0,
                "ジャンクション",
                ffi::FILE_ATTRIBUTE_DIRECTORY | ffi::FILE_ATTRIBUTE_REPARSE_POINT,
                0xA000_0003,
            );
            bytes.extend_from_slice(&second);
            bytes.resize(bytes.len() + 64, 0xEE); // 最後の項目のあとのごみは読まない
            let mut out = Vec::new();
            parse_entries(&bytes, mode, &mut out).unwrap();
            assert_eq!(out.len(), 1, "{mode:?}：. は除く");
            assert_eq!(out[0].name(), &OsString::from("ジャンクション"));
            assert_eq!(out[0].reparse_tag(), Some(0xA000_0003));
            assert_eq!(out[0].kind(), EntryKind::Link);
            assert_eq!(out[0].end_of_file(), 5);
            assert_eq!(out[0].modified_unix_ns(), 0);
            assert_eq!(out[0].file_id().is_some(), mode == Mode::IdExtd);
        }
    }

    /// 境を外れるバイト列（NextEntryOffset が範囲の外・前へ進まない、FileNameLength が奇数・長さを超える、
    /// 見出しが途中で切れる）は Err。
    #[test]
    fn parse_rejects_out_of_range() {
        for mode in [Mode::IdExtd, Mode::Full] {
            let good = entry_bytes(mode, 0, "a.txt", 0, 0);
            let mut out = Vec::new();

            // NextEntryOffset が範囲の外
            let mut b = good.clone();
            b[0..4].copy_from_slice(&4096u32.to_ne_bytes());
            assert!(
                parse_entries(&b, mode, &mut out).is_err(),
                "{mode:?} next の外"
            );

            // NextEntryOffset が見出し＋名前より小さい（前へ進まない・重なる）
            let mut b = good.clone();
            b[0..4].copy_from_slice(&8u32.to_ne_bytes());
            b.extend_from_slice(&good);
            assert!(
                parse_entries(&b, mode, &mut out).is_err(),
                "{mode:?} next が小さい"
            );

            // FileNameLength が奇数
            let mut b = good.clone();
            b[ffi::DIR_NAME_LENGTH_AT..ffi::DIR_NAME_LENGTH_AT + 4]
                .copy_from_slice(&3u32.to_ne_bytes());
            assert!(parse_entries(&b, mode, &mut out).is_err(), "{mode:?} 奇数");

            // FileNameLength が長さを超える
            let mut b = good.clone();
            b[ffi::DIR_NAME_LENGTH_AT..ffi::DIR_NAME_LENGTH_AT + 4]
                .copy_from_slice(&0x7FFF_FFFEu32.to_ne_bytes());
            assert!(
                parse_entries(&b, mode, &mut out).is_err(),
                "{mode:?} 名前が長い"
            );

            // 見出しが途中で切れる
            assert!(parse_entries(&good[..mode.name_at() - 1], mode, &mut out).is_err());
            assert!(parse_entries(&[], mode, &mut out).is_err());
            assert!(out.is_empty());
        }
    }

    #[test]
    fn filetime_conversion_matches_unix_nanos() {
        assert_eq!(filetime_to_unix_ns(UNIX_EPOCH_AS_FILETIME_I64), 0);
        assert_eq!(filetime_to_unix_ns(UNIX_EPOCH_AS_FILETIME_I64 + 1), 100);
        assert_eq!(filetime_to_unix_ns(UNIX_EPOCH_AS_FILETIME_I64 - 1), -100);
        assert_eq!(filetime_to_unix_ns(i64::MAX), i64::MAX);
        assert_eq!(filetime_to_unix_ns(i64::MIN), i64::MIN);
    }

    /// std の read_dir と、os-sys の列挙を、名前ごとに比べる（R8。`first` で最初の種類を選ぶ）。
    pub(crate) fn compare_with_std(path: &std::path::Path, first: Mode) -> usize {
        let access = Access::LIST_DIRECTORY | Access::TRAVERSE | Access::READ_ATTRIBUTES;
        let share = Share::READ | Share::WRITE;
        let mut dir = match open_path(path, Links::DontFollow, access, share).unwrap() {
            Opened::Dir(d) => d,
            other => panic!("{other:?}"),
        };
        let ours: BTreeMap<OsString, DirEntry> = read_dir_mode(&mut dir, first)
            .unwrap()
            .into_iter()
            .map(|e| (e.name().clone(), e))
            .collect();
        let theirs: BTreeMap<OsString, fs::Metadata> = fs::read_dir(path)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (e.file_name(), e.metadata().unwrap())
            })
            .collect();
        assert_eq!(
            ours.keys().collect::<Vec<_>>(),
            theirs.keys().collect::<Vec<_>>()
        );
        for (name, meta) in &theirs {
            let e = &ours[name];
            let ft = meta.file_type();
            let kind = if ft.is_symlink() {
                EntryKind::Link
            } else if ft.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            assert_eq!(e.kind(), kind, "{name:?}");
            assert_eq!(e.end_of_file(), meta.len(), "{name:?}");
            let std_ns = match meta.modified().unwrap().duration_since(UNIX_EPOCH) {
                Ok(d) => i64::try_from(d.as_nanos()).unwrap(),
                Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap(),
            };
            assert_eq!(e.modified_unix_ns(), std_ns, "{name:?}");
            assert_eq!(
                e.attributes(),
                std::os::windows::fs::MetadataExt::file_attributes(meta),
                "{name:?}"
            );
        }
        ours.len()
    }

    /// R8（FileFullDirectoryInfo の道）：最初から FileFullDirectoryInfo で読んでも、std の read_dir と同じ。
    #[test]
    fn r8_full_directory_info_matches_std() {
        let base = UnitDir::new("r8-full");
        let outside = UnitDir::new("r8-full-outside");
        for i in 0..2000 {
            fs::write(base.join(format!("f{i:04}.txt")), vec![b'x'; i % 7]).unwrap();
        }
        fs::write(base.join("日本語の名前.txt"), b"abc").unwrap();
        fs::write(base.join("a".repeat(255)), b"long").unwrap();
        fs::write(base.join("あ".repeat(255)), b"long").unwrap();
        fs::create_dir(base.join("sub")).unwrap();
        let has_junction = junction(&base.join("link"), outside.path());
        let n = compare_with_std(base.path(), Mode::Full);
        assert_eq!(n, 2000 + 4 + usize::from(has_junction));
        println!("R8（FileFullDirectoryInfo）：{n} 項目が std と一致");
    }
}
