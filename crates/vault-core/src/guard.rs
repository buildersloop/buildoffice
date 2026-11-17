//! 確かめたハンドルで操作する部品。
//!
//! 「パスで確かめる → パスで操作する」をやめ、根から1段ずつ、持っている親のハンドルを基準に、リンクを
//! たどらずに開く・作る。開いたハンドル（2つめの門）で値を確かめ、同じハンドルで操作する。
//!
//! - Windows：`windows.rs`（os-sys の上）。
//! - Windows 以外：`other.rs`（Phase 0 のパスの処理のまま。CI が動いたら直す）。
//!
//! どちらも同じ形の部品を出す：`Root`（操作の根）・`Held`（列挙もするフォルダ）・`Pass`（通り抜けるだけの
//! フォルダ。Windows では PassDir）・`TmpFile`（作った一時ファイル）・`NewDir`（作ったフォルダ）・`Target`（2つめの門で
//! 開いた、移す・ゴミ箱へ送るもの）・`Trashed`（ゴミ箱へ移したもの）・`TrashItem`（開いたゴミ箱の `<id>`）など。
//! rename の移す先の親に渡せるのは、DELETE の権利を持たない `Held` と `Pass` だけ（`RenameParent`）。

use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(not(windows))]
mod other;
#[cfg(not(windows))]
pub(crate) use other::*;

/// 管理用の決まった名前（`.vault` の下）。利用者の名前（RelPath の要素）からは作らない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedName {
    /// `.vault/tmp`
    Tmp,
    /// `.vault/journal`
    Journal,
    /// `.vault/lock`
    Lock,
    /// `.vault/vault.json`
    VaultJson,
    /// `.vault/index.json`
    IndexJson,
    /// `.vault/tmp/<pid>-<連番>.tmp`
    TmpFile(String),
    /// `.vault/journal/<20桁>.jsonl`
    JournalFile(String),
    /// `.vault/trash`
    Trash,
    /// `.vault/trash/<id>`
    #[cfg_attr(not(windows), allow(dead_code))]
    TrashItem(TrashId),
    /// `.vault/trash/<id>/entry.json`
    #[cfg_attr(not(windows), allow(dead_code))]
    EntryJson(TrashId),
    /// `.vault/trash/<id>/data`
    #[cfg_attr(not(windows), allow(dead_code))]
    TrashData(TrashId),
}

impl ManagedName {
    /// 名前（1つの要素）。
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Tmp => crate::fsutil::TMP_DIR,
            Self::Journal => crate::journal::JOURNAL_DIR,
            Self::Lock => crate::vault::LOCK_FILE,
            Self::VaultJson => crate::root::VAULT_JSON,
            Self::IndexJson => crate::index::INDEX_FILE,
            Self::TmpFile(name) | Self::JournalFile(name) => name,
            Self::Trash => crate::trash::TRASH_DIR,
            Self::TrashItem(id) => id.as_str(),
            Self::EntryJson(_) => crate::trash::ENTRY_FILE,
            Self::TrashData(_) => crate::trash::DATA_NAME,
        }
    }

    /// `.vault` からの並び（最終のパスの比べに使う）。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn below_vault(&self) -> Vec<&str> {
        match self {
            Self::TmpFile(name) => vec![crate::fsutil::TMP_DIR, name],
            Self::JournalFile(name) => vec![crate::journal::JOURNAL_DIR, name],
            Self::TrashItem(id) => vec![crate::trash::TRASH_DIR, id.as_str()],
            Self::EntryJson(id) => vec![
                crate::trash::TRASH_DIR,
                id.as_str(),
                crate::trash::ENTRY_FILE,
            ],
            Self::TrashData(id) => vec![
                crate::trash::TRASH_DIR,
                id.as_str(),
                crate::trash::DATA_NAME,
            ],
            other => vec![other.as_str()],
        }
    }

    /// 履歴のファイルの名前（`<20桁の seq>.jsonl`）。
    pub(crate) fn journal_file(first_seq: u64) -> Self {
        Self::JournalFile(format!("{first_seq:020}.jsonl"))
    }

    /// 新しい一時ファイルの名前（`<pid>-<連番>.tmp`）。
    pub(crate) fn new_tmp_file() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        Self::TmpFile(format!(
            "{}-{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

/// 置き換える rename（C'）で置いてよい名前（索引と vault.json だけ）。
#[derive(Debug, Clone, Copy)]
pub(crate) enum Replaceable {
    /// `.vault/index.json`
    IndexJson,
    /// `.vault/vault.json`
    VaultJson,
}

impl Replaceable {
    /// 管理用の名前。
    pub(crate) fn name(self) -> ManagedName {
        match self {
            Self::IndexJson => ManagedName::IndexJson,
            Self::VaultJson => ManagedName::VaultJson,
        }
    }
}

/// 形を確かめた trash_id（`<unix_ms>-<pid>-<連番>`）。
/// 利用者の名前（RelPath の要素）からは作らない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrashId(String);

impl TrashId {
    /// 新しい trash_id。
    pub(crate) fn new() -> Self {
        Self(crate::trash::new_trash_id())
    }

    /// 形が正しければ trash_id にする。
    pub(crate) fn parse(id: &str) -> Option<Self> {
        crate::trash::is_valid_trash_id(id).then(|| Self(id.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

/// 書く道（vault_write）で、書いたあとに残す空き容量（1 GiB）。
pub const MIN_FREE_AFTER_WRITE: u64 = 1 << 30;
