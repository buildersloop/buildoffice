//! 索引（`.vault/index.json`。`{path:{kind,size,mtime_ns}}`）。
//!
//! 走査（`scan.rs`）が、前の状態と比べるのに使う。コアの操作のたびに直す。
//! コアの操作では、索引が読めない（ない・壊れている）ときは直さずにそのままにする。作り直すのは走査
//! （`index_rebuilt` を記録して、歩いた結果で置き換える）。

use std::collections::BTreeMap;
use std::io::Read;
use std::ops::Bound;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, Result, VaultError};
use crate::fsutil::{EntryKind, FileInfo};
use crate::guard::{
    self, ManagedFile, ManagedName, ManagedUse, ParentDir, Pass, Replaceable, Root,
};
use crate::relpath::RelPath;
use crate::vault::VaultLock;

/// 索引のファイルの名前。
pub(crate) const INDEX_FILE: &str = "index.json";
/// 読む索引の大きさの上限（これより大きければ、読めないものとして扱う）。
const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;

/// 索引の1項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ns: i64,
}

impl From<FileInfo> for IndexEntry {
    fn from(info: FileInfo) -> Self {
        Self {
            kind: info.kind,
            size: info.size,
            mtime_ns: info.mtime_ns,
        }
    }
}

/// 索引（保管庫の中の相対パス → 項目）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Index {
    entries: BTreeMap<String, IndexEntry>,
}

impl Index {
    /// 項目の集まりから作る。
    pub(crate) fn from_entries(entries: BTreeMap<String, IndexEntry>) -> Self {
        Self { entries }
    }

    /// 項目（相対パスの順）。
    pub(crate) fn entries(&self) -> &BTreeMap<String, IndexEntry> {
        &self.entries
    }

    /// 項目を入れる（あれば置き換える）。
    pub fn set(&mut self, path: &str, entry: IndexEntry) {
        self.entries.insert(path.to_owned(), entry);
    }

    /// `path` と、その下（`path/...`）のキー（順序付きの範囲で引く。木の大きさに比例し、索引の全体は見ない）。
    fn tree_keys(&self, path: &str) -> Vec<String> {
        let prefix = format!("{path}/");
        let mut keys: Vec<String> = self
            .entries
            .get_key_value(path)
            .map(|(key, _)| key.clone())
            .into_iter()
            .collect();
        keys.extend(
            self.entries
                .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
                .take_while(|(key, _)| key.starts_with(&prefix))
                .map(|(key, _)| key.clone()),
        );
        keys
    }

    /// `path` と、その下（`path/...`）の項目を除く。
    pub fn remove_tree(&mut self, path: &str) {
        for key in self.tree_keys(path) {
            self.entries.remove(&key);
        }
    }

    /// `from` と、その下の項目を `to` の下へ移す（走査の名前の変更で、変更の数だけ呼ぶので、索引の全体は見ない）。
    pub fn rename_tree(&mut self, from: &str, to: &str) {
        let moved: Vec<(String, IndexEntry)> = self
            .tree_keys(from)
            .into_iter()
            .filter_map(|key| {
                let entry = self.entries.remove(&key)?;
                Some((format!("{to}{}", &key[from.len()..]), entry))
            })
            .collect();
        self.entries.extend(moved);
    }

    /// `path` とその下の項目を、大文字小文字を問わずに除く。
    pub fn remove_tree_any_case(&mut self, path: &str) {
        let lower = path.to_lowercase();
        let prefix = format!("{lower}/");
        self.entries.retain(|key, _| {
            let key = key.to_lowercase();
            key != lower && !key.starts_with(&prefix)
        });
    }

    /// `path` と大文字小文字だけが違うキーを除く（`path` そのものは残す）。
    /// Windows では、上書きで同じファイルが別の大文字小文字の名前に変わることがあるため。
    pub fn remove_case_variants(&mut self, path: &str) {
        let lower = path.to_lowercase();
        self.entries
            .retain(|key, _| key == path || key.to_lowercase() != lower);
    }

    /// 変更を1つ当てる。
    fn apply(&mut self, change: IndexChange) {
        match change {
            IndexChange::Set {
                path,
                entry,
                drop_case_variants,
            } => {
                if drop_case_variants && cfg!(windows) {
                    self.remove_case_variants(&path);
                }
                self.entries.insert(path, entry);
            }
            IndexChange::RemoveTreeAnyCase(path) => {
                if cfg!(windows) {
                    self.remove_tree_any_case(&path);
                } else {
                    self.remove_tree(&path);
                }
            }
            IndexChange::RenameTree { from, to } => self.rename_tree(&from, &to),
        }
    }
}

/// 索引への変更（ツールの呼び出しのあいだにためて、最後に1回で当てる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IndexChange {
    /// 項目を入れる。`drop_case_variants` なら、大文字小文字だけが違うキーも除く（Windows）。
    Set {
        path: String,
        entry: IndexEntry,
        drop_case_variants: bool,
    },
    /// その項目と下の項目を、大文字小文字だけが違うキーも含めて除く（Windows。ほかの OS では RemoveTree と同じ）。
    RemoveTreeAnyCase(String),
    /// その項目と下の項目を移す。
    RenameTree { from: String, to: String },
}

/// 索引のファイルが、ないか、ふつうのファイルか（フォルダやリンクなら `NOT_A_VAULT`。open と走査で確かめる）。
/// `.vault` のハンドルを基準に、リンクをたどらずに開いて確かめる。
pub(crate) fn check_file(vault: &Pass) -> Result<()> {
    match guard::open_managed_file(
        vault.parent(),
        &ManagedName::IndexJson,
        ManagedUse::Attrs,
        "索引のファイルを調べられない",
    )? {
        ManagedFile::File(_) | ManagedFile::Missing => Ok(()),
        ManagedFile::NotFile => Err(VaultError::new(
            ErrorCode::NotAVault,
            "索引のファイル（.vault/index.json）が、ふつうのファイルでない",
        )),
    }
}

/// 索引を読む。ない・リンク・大きすぎる・壊れている・ツールで扱えないキー（文字列の規則に通らない・根）が
/// 1つでもあるときは None（直さない。走査が作り直す）。`.vault` のハンドルを基準に、リンクをたどらずに開く。
pub(crate) fn load(vault: &Pass) -> Option<Index> {
    test_hook!(Step::Open(Place::Index));
    let ManagedFile::File(file) = guard::open_managed_file(
        vault.parent(),
        &ManagedName::IndexJson,
        ManagedUse::Read,
        "索引を読めない",
    )
    .ok()?
    else {
        return None;
    };
    if file.metadata().ok()?.len() > MAX_INDEX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let index: Index = serde_json::from_slice(&bytes).ok()?;
    index
        .entries
        .keys()
        .all(|key| RelPath::parse(key).is_ok_and(|rel| !rel.is_root()))
        .then_some(index)
}

/// 索引を、書く形（JSON）にする。256 MiB を超えるなら `TOO_LARGE`（書いても、次に読めないため）。
pub(crate) fn encode(index: &Index) -> Result<Vec<u8>> {
    encode_within(index, MAX_INDEX_BYTES)
}

fn encode_within(index: &Index, max: u64) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(index)
        .map_err(|_| VaultError::new(ErrorCode::Internal, "索引を作れない"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err(VaultError::new(
            ErrorCode::TooLarge,
            format!(
                "索引が大きすぎる（{} MiB を超える）ので書かない。保管庫の中のファイルとフォルダの数を減らす",
                max / (1024 * 1024)
            ),
        ));
    }
    Ok(bytes)
}

/// 索引を書く（一時ファイルから置き換える rename）。ロックを持っていること（`_lock`）が条件。
pub(crate) fn save(
    root: &Root,
    vault: &Pass,
    _lock: &VaultLock,
    index: &Index,
) -> Result<guard::Written> {
    let bytes = encode(index)?;
    guard::atomic_write(root, vault, Replaceable::IndexJson, &bytes)
}

/// 書く形にした索引で置き換える（走査）。書けなかったときは、索引を消して、次の走査に作り直させる
/// （古い索引のままだと、履歴に書いた変更を、次の走査でもう一度出してしまうため）。
pub(crate) fn replace_encoded(root: &Root, vault: &Pass, _lock: &VaultLock, bytes: &[u8]) {
    if guard::atomic_write(root, vault, Replaceable::IndexJson, bytes).is_err() {
        guard::remove_index(root, vault);
    }
}

/// ためた変更を、1回の読みと1回の書きで当てる。`loaded` は、呼び出しの途中で読んだ索引（読んでいなければ None）。
///
/// 読めないときは何もしない（作り直しは走査）。書けなかったときは、古いままの索引を残すと
/// 走査がコアの操作を外での変更と取り違えるので、索引を消して、走査に作り直させる。
/// どちらのときも、ツールの呼び出しは成功のままにする（索引は走査で作り直せるため）。
pub(crate) fn apply(
    root: &Root,
    vault: &Pass,
    lock: &VaultLock,
    loaded: Option<Option<Index>>,
    changes: Vec<IndexChange>,
) {
    if changes.is_empty() {
        return;
    }
    let Some(mut index) = loaded.unwrap_or_else(|| load(vault)) else {
        return;
    };
    for change in changes {
        index.apply(change);
    }
    if save(root, vault, lock, &index).is_err() {
        guard::remove_index(root, vault);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(size: u64) -> IndexEntry {
        IndexEntry {
            kind: EntryKind::File,
            size,
            mtime_ns: 1,
        }
    }

    #[test]
    fn serialized_form_matches_the_adr() {
        let mut index = Index::default();
        index.set("a/b.md", entry(3));
        let json = serde_json::to_value(&index).unwrap();
        assert_eq!(json["a/b.md"]["kind"], "file");
        assert_eq!(json["a/b.md"]["size"], 3);
        assert_eq!(json["a/b.md"]["mtime_ns"], 1);
        let back: Index = serde_json::from_value(json).unwrap();
        assert_eq!(back, index);
    }

    #[test]
    fn remove_and_rename_trees() {
        let mut index = Index::default();
        for p in ["a", "a/x", "a/y/z", "ab", "b"] {
            index.set(p, entry(1));
        }
        let mut renamed = index.clone();
        renamed.rename_tree("a", "c/d");
        let keys: Vec<&str> = renamed.entries.keys().map(String::as_str).collect();
        assert_eq!(keys, ["ab", "b", "c/d", "c/d/x", "c/d/y/z"]);

        index.remove_tree("a");
        let keys: Vec<&str> = index.entries.keys().map(String::as_str).collect();
        assert_eq!(keys, ["ab", "b"]);
    }

    /// 削除と復元では、大文字小文字を問わずに、その項目と下の項目を除く。
    #[test]
    fn trees_are_removed_in_any_case() {
        let mut index = Index::default();
        for p in ["A.txt", "a.TXT", "Dir/x", "dir/Y", "dirs", "b"] {
            index.set(p, entry(1));
        }
        index.remove_tree_any_case("a.txt");
        index.remove_tree_any_case("DIR");
        let keys: Vec<&str> = index.entries.keys().map(String::as_str).collect();
        assert_eq!(keys, ["b", "dirs"]);
    }

    /// 256 MiB（ここでは小さくした上限）を超える索引は、書く形にしない（TOO_LARGE）。
    #[test]
    fn oversized_indexes_are_not_encoded() {
        let mut index = Index::default();
        for i in 0..100 {
            index.set(&format!("file-{i}.txt"), entry(i));
        }
        let size = u64::try_from(encode(&index).unwrap().len()).unwrap();
        assert!(encode_within(&index, size).is_ok());
        let err = encode_within(&index, size - 1).unwrap_err();
        assert_eq!(err.code(), ErrorCode::TooLarge);
        assert_eq!(MAX_INDEX_BYTES, 256 * 1024 * 1024);
    }

    /// 上書きで今あるファイルが見つかったら、大文字小文字だけ違うキーも除く。
    #[test]
    fn case_variants_are_removed() {
        let mut index = Index::default();
        for p in ["Dir/A.txt", "dir/a.TXT", "dir/a.txt", "dir/b.txt", "Other"] {
            index.set(p, entry(1));
        }
        index.remove_case_variants("dir/a.txt");
        let keys: Vec<&str> = index.entries.keys().map(String::as_str).collect();
        assert_eq!(keys, ["Other", "dir/a.txt", "dir/b.txt"]);

        let mut index = Index::default();
        index.set("A.txt", entry(1));
        index.apply(IndexChange::Set {
            path: "a.txt".to_owned(),
            entry: entry(2),
            drop_case_variants: true,
        });
        let keys: Vec<&str> = index.entries.keys().map(String::as_str).collect();
        if cfg!(windows) {
            assert_eq!(keys, ["a.txt"]);
        } else {
            assert_eq!(keys, ["A.txt", "a.txt"], "Windows のほかでは別のファイル");
        }
    }
}
