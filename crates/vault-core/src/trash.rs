//! ゴミ箱へ移す共通の関数と、entry.json の形・trash_id。
//!
//! `trash/<trash_id>/` を作り、`entry.json` を書き、対象を置き換えない rename で `data` に移す（ハンドルの形は
//! guard の `trash_target`）。
//! 削除（delete）・上書き（overwrite）・置き換えての復元（restore_replace）・残った一時ファイル（orphan_tmp）が、
//! すべてこの関数を使う。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::error::{ErrorCode, Result, VaultError};
use crate::fsutil::EntryKind;
use crate::guard::{self, Pass, RenameParent, Root, Target, Trashed};
use crate::time;
use crate::vault::VaultLock;

/// ゴミ箱のフォルダ（`.vault/trash`）。
pub(crate) const TRASH_DIR: &str = "trash";
/// entry.json の名前。
pub(crate) const ENTRY_FILE: &str = "entry.json";
/// 移した中身の名前。
pub(crate) const DATA_NAME: &str = "data";
/// entry.json の大きさの上限。
pub const MAX_ENTRY_BYTES: usize = 64 * 1024;

/// trash_id の連番（`<unix_ms>-<pid>-<そのプロセスの連番>`）。
static TRASH_SEQ: AtomicU64 = AtomicU64::new(0);

/// ゴミ箱へ移した理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrashReason {
    Delete,
    Overwrite,
    RestoreReplace,
    OrphanTmp,
}

/// `trash/<trash_id>/entry.json` の中身。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashEntry {
    pub original_path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub deleted_at: String,
    pub actor: Actor,
    pub reason: TrashReason,
}

/// 新しい trash_id（`<unix_ms>-<pid>-<連番>`）。
pub(crate) fn new_trash_id() -> String {
    format!(
        "{}-{}-{}",
        time::now_unix_millis(),
        std::process::id(),
        TRASH_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// ゴミ箱へ移すもの。
pub(crate) struct ToTrash<'a> {
    /// 移す対象（呼ぶ側が2つめの門で開いたハンドル）。
    pub target: Target,
    /// 元の親と名前（rename のあとの確かめで、違う場所と分かったとき（起きないはず）、ここへ戻す）。
    pub origin: &'a dyn RenameParent,
    pub name: &'a str,
    /// 保管庫の中の相対パス（entry.json の original_path）。
    pub original_path: &'a str,
    pub kind: EntryKind,
    pub size: u64,
    pub reason: TrashReason,
}

/// ゴミ箱へ移す（共通の関数）。ロックを持っていること（`_lock`）が条件。
///
/// 順番：`trash/<id>/` を作る（確かめて、閉じて開き直す）→ `entry.json` を書く → 対象を置き換えない rename で
/// `data` に移す。移せなかったら、作った `entry.json` と `<id>` を片づけ、対象には触れずにエラーを返す。
/// 返り値は、移したもの（`guard::take_back` で取り戻すか、`into_id` で閉じて trash_id にする）。
pub(crate) fn move_to_trash(
    root: &Root,
    vault: &Pass,
    _lock: &VaultLock,
    actor: &Actor,
    item: ToTrash<'_>,
) -> Result<Trashed> {
    let ToTrash {
        target,
        origin,
        name,
        original_path,
        kind,
        size,
        reason,
    } = item;
    let entry = TrashEntry {
        original_path: original_path.to_owned(),
        kind,
        size,
        deleted_at: time::format_unix_millis(time::now_unix_millis())
            .ok_or_else(|| VaultError::new(ErrorCode::Internal, "今の時刻を書けない"))?,
        actor: actor.clone(),
        reason,
    };
    let bytes = serde_json::to_vec(&entry)
        .map_err(|_| VaultError::new(ErrorCode::Internal, "entry.json を作れない"))?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(VaultError::new(
            ErrorCode::Internal,
            "entry.json が大きすぎる",
        ));
    }
    guard::trash_target(root, vault, target, origin, name, &bytes)
}

/// trash_id の形（`^[0-9]{1,20}-[0-9]{1,10}-[0-9]{1,20}$`）か。
pub(crate) fn is_valid_trash_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 3
        && parts.iter().zip([20, 10, 20]).all(|(part, max)| {
            !part.is_empty() && part.len() <= max && part.bytes().all(|b| b.is_ascii_digit())
        })
}

/// trash_id を並べるための値（unix_ms・連番・pid）。形が正しくなければ None。
pub(crate) fn trash_id_order(id: &str) -> Option<(u128, u128, u128)> {
    if !is_valid_trash_id(id) {
        return None;
    }
    let mut parts = id.split('-').map(|p| p.parse::<u128>().ok());
    let (ms, pid, seq) = (parts.next()??, parts.next()??, parts.next()??);
    Some((ms, seq, pid))
}

/// entry.json の中身を読む（形が正しくなければ None）。ふつうのファイルか・大きさの上限は、読む側（guard）で確かめる。
pub(crate) fn parse_entry(bytes: &[u8]) -> Option<TrashEntry> {
    serde_json::from_slice(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_ids_match_the_restore_pattern() {
        // ^[0-9]{1,20}-[0-9]{1,10}-[0-9]{1,20}$
        let a = new_trash_id();
        let b = new_trash_id();
        assert_ne!(a, b);
        for id in [a, b] {
            let parts: Vec<&str> = id.split('-').collect();
            assert_eq!(parts.len(), 3, "{id}");
            for (part, max) in parts.iter().zip([20, 10, 20]) {
                assert!(
                    !part.is_empty()
                        && part.len() <= max
                        && part.bytes().all(|c| c.is_ascii_digit()),
                    "{id}"
                );
            }
        }
    }

    #[test]
    fn trash_id_shapes() {
        for ok in [
            "1-2-3",
            "12345678901234567890-1234567890-12345678901234567890",
        ] {
            assert!(is_valid_trash_id(ok), "{ok}");
        }
        for bad in [
            "",
            "../x",
            "1-2-3/../../x",
            "1-2",
            "1-2-3-4",
            "a-2-3",
            "1--3",
            "-1-2-3",
            "1-2-3 ",
            "123456789012345678901-1-1",
            "1-12345678901-1",
            "１-2-3",
        ] {
            assert!(!is_valid_trash_id(bad), "{bad:?}");
        }
        assert!(trash_id_order("5-1-9") > trash_id_order("5-2-8"));
        assert!(trash_id_order("6-1-0") > trash_id_order("5-1-9"));
        assert_eq!(trash_id_order("x"), None);
    }

    #[test]
    fn entry_serializes_like_the_adr() {
        let entry = TrashEntry {
            original_path: "a/b.md".to_owned(),
            kind: EntryKind::File,
            size: 3,
            deleted_at: "2026-11-16T00:00:00.000Z".to_owned(),
            actor: Actor::user("app"),
            reason: TrashReason::Overwrite,
        };
        let json = serde_json::to_value(&entry).unwrap();
        for key in [
            "original_path",
            "kind",
            "size",
            "deleted_at",
            "actor",
            "reason",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["reason"], "overwrite");
        assert_eq!(
            serde_json::to_value(TrashReason::RestoreReplace).unwrap(),
            "restore_replace"
        );
        assert_eq!(
            serde_json::to_value(TrashReason::OrphanTmp).unwrap(),
            "orphan_tmp"
        );
    }
}
