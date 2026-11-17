//! 削除・ゴミ箱の一覧・復元と、open のときの orphan_tmp の片づけ。
//!
//! - 削除は、ゴミ箱へ移すだけ（共通の関数 `trash::move_to_trash`）。完全に消す処理は作らない。
//! - 書く処理は、(b) と同じく `Txn`（履歴を先に開く・1回の sync・索引は1回）で進める。
//! - 復元の検証：trash_id の形と、trash/ の直下の一覧との完全一致、original_path と to の検証、
//!   data がリンクなら `INVALID_TRASH_ENTRY`、戻す先があれば `CONFLICT`（replace なら先にゴミ箱へ）。
//! - 移す・戻すのは、確かめたハンドルで行う：1つめの門（パス）のあと、
//!   根から1段ずつ親を開き、2つめの門（TARGET_*）で値を確かめ、置き換えない rename で移す。ゴミ箱の `<id>` は
//!   PassDir で持つ。
//! - 列挙（ゴミ箱の直下・`.vault/tmp` の中・移すフォルダの中）は、持っているフォルダのハンドルで行う。

use std::ffi::OsStr;
use std::fs;
use std::time::Duration;

use serde::Serialize;

use crate::actor::{Actor, ActorKind};
use crate::error::{ErrorCode, Result, VaultError};
use crate::fsutil::{EntryKind, FileInfo};
use crate::guard::{
    self, DataState, Held, ItemState, ManagedName, Moved, ParentDir, Pass, Target, TargetOpen,
    TargetUse, TrashId, Trashed,
};
use crate::index::{IndexChange, IndexEntry};
use crate::journal::{Op, Record};
use crate::ops::{
    Txn, invalid_argument, last_name, not_found, outside_link, reject_read_only_file, second_gate,
    subtree_extent,
};
use crate::relpath::{RESERVED_NAME, RelPath, is_valid_component};
use crate::root::is_not_found;
use crate::trash::{
    self, ToTrash, TrashEntry, TrashReason, is_valid_trash_id, parse_entry, trash_id_order,
};
use crate::vault::Vault;

/// vault_trash_list の limit の既定値。
pub const DEFAULT_TRASH_LIST_LIMIT: u64 = 100;
/// vault_trash_list の limit の上限。
pub const MAX_TRASH_LIST_LIMIT: u64 = 10_000;
/// open のときの orphan_tmp の片づけで、ロックを待つ時間（ほかの処理が書いていれば、片づけは次に回す）。
const ORPHAN_LOCK_TIMEOUT: Duration = Duration::from_secs(1);

/// vault_delete の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeleteResult {
    pub path: String,
    pub trash_id: String,
}

/// vault_trash_list の1項目（trash_id と entry.json の中身）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrashItem {
    pub trash_id: String,
    #[serde(flatten)]
    pub entry: TrashEntry,
}

/// vault_trash_list の返り値。新しい順。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrashListResult {
    pub items: Vec<TrashItem>,
    pub truncated: bool,
}

/// vault_restore の返り値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoreResult {
    pub path: String,
    pub replaced_trash_id: Option<String>,
}

impl Vault {
    /// vault_delete：ファイルかフォルダをゴミ箱へ移す（vault_restore で戻せる）。
    ///
    /// 中が空でないフォルダは `recursive:true` が要る（`DIR_NOT_EMPTY`）。フォルダは1回の rename で
    /// 中身ごと移す（中のリンクは、リンクそのものが移り、先はたどらない。中に読み取り専用の印のファイルがあってもよい。
    /// 戻せるため）。読み取り専用の印が付いたファイルそのものは消さない（`ACCESS_DENIED`）。根は `INVALID_PATH`。
    pub fn delete(&self, actor: &Actor, path: &str, recursive: bool) -> Result<DeleteResult> {
        self.ensure_writable()?;
        let rel = RelPath::parse(path)?;
        if rel.is_root() {
            return Err(VaultError::new(
                ErrorCode::InvalidPath,
                "保管庫の根は消せない",
            ));
        }
        let lock = self.lock()?;
        let mut txn = Txn::begin(self, &lock)?;
        let result = self.delete_in(&mut txn, actor, &rel, recursive);
        txn.finish(result, |d| {
            format!(
                "ゴミ箱へ移すのは済んでいる：{}（trash_id: {}）",
                d.path, d.trash_id
            )
        })
    }

    /// vault_trash_list：ゴミ箱の一覧（新しい順）。data のない entry・読めない entry.json・使用中の `<id>` は出さない。
    pub fn trash_list(&self, limit: u64) -> Result<TrashListResult> {
        if !(1..=MAX_TRASH_LIST_LIMIT).contains(&limit) {
            return Err(invalid_argument(format!(
                "limit は 1 から {MAX_TRASH_LIST_LIMIT} まで"
            )));
        }
        let root = self.op_root()?;
        let vault = root.open_vault_dir()?;
        // .vault/trash がリンクでないことを確かめ、ハンドルで持つ（ないなら空）
        let Some(mut trash) = guard::open_trash(&root, &vault)? else {
            return Ok(TrashListResult {
                items: Vec::new(),
                truncated: false,
            });
        };
        // 直下の名前は、持っている .vault/trash のハンドルで列挙する（E）
        let listed = trash.entries("ゴミ箱を読めない")?;
        let mut items = Vec::new();
        for entry in listed {
            let Some(name) = entry.name.to_str().map(str::to_owned) else {
                continue;
            };
            let (Some(order), Some(id)) = (trash_id_order(&name), TrashId::parse(&name)) else {
                continue;
            };
            // <id> は PassDir で開く（試し直さない）
            let item = match guard::open_trash_item(&root, &trash, id, false) {
                Ok(ItemState::Item(item)) => item,
                // 使用中（こちらが作った直後・消す直前の一瞬、またはほかのアプリ）：試し直さずに飛ばす
                // （BUSY にすると、1つの項目のせいで一覧の全体が失敗する）
                Ok(ItemState::Busy) => continue,
                // 消えた・フォルダでない（リンクを含む）：一覧に出さない（今までと同じ）
                Ok(ItemState::Missing | ItemState::NotDir) => continue,
                // 開けない（権限など）・場所が `.vault\trash\<id>` でない（起きないはず）：その項目だけを出さない
                // （1つの項目のせいで一覧の全体を失敗させない。復元では、同じ項目がエラーとして返る）
                Err(_) => continue,
            };
            if !item.has_data() {
                continue;
            }
            let Some(trash_entry) = item.read_entry().as_deref().and_then(parse_entry) else {
                continue;
            };
            items.push((
                order,
                TrashItem {
                    trash_id: name,
                    entry: trash_entry,
                },
            ));
        }
        // 新しい順
        items.sort_by_key(|(order, _)| std::cmp::Reverse(*order));
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let truncated = items.len() > limit;
        items.truncate(limit);
        Ok(TrashListResult {
            items: items.into_iter().map(|(_, item)| item).collect(),
            truncated,
        })
    }

    /// vault_restore：ゴミ箱から戻す。
    ///
    /// `to` を省けば、もとの場所（entry.json の original_path）へ戻す。`to` を指定したときは、
    /// vault_move と同じく、中のいちばん深いもの・長いものが上限に収まるかを確かめる。
    /// 戻す先にすでに何かあれば `CONFLICT`。`replace:true` なら、今ある中身を先にゴミ箱へ移す（restore_replace）。
    /// 戻す先の親のフォルダがなければ作り、履歴に mkdir を書く。
    pub fn restore(
        &self,
        actor: &Actor,
        trash_id: &str,
        to: Option<&str>,
        replace: bool,
    ) -> Result<RestoreResult> {
        self.ensure_writable()?;
        if !is_valid_trash_id(trash_id) {
            return Err(invalid_argument(
                "trash_id の形が正しくない（vault_trash_list が返す値を、そのまま渡す）",
            ));
        }
        let to_rel = to.map(RelPath::parse).transpose()?;
        let lock = self.lock()?;
        let mut txn = Txn::begin(self, &lock)?;
        let result = self.restore_in(&mut txn, actor, trash_id, to_rel.as_ref(), replace);
        txn.finish(result, |r| {
            format!("ゴミ箱から戻すのは済んでいる：{}", r.path)
        })
    }

    fn delete_in(
        &self,
        txn: &mut Txn<'_>,
        actor: &Actor,
        rel: &RelPath,
        recursive: bool,
    ) -> Result<DeleteResult> {
        // 1つめの門（今のエラーのコードと文のため。決める値は2つめの門で取り直す）
        let abs = self.resolve_rel(rel)?;
        let meta = match fs::symlink_metadata(&abs) {
            Ok(meta) => meta,
            Err(e) if is_not_found(&e) => return Err(not_found(rel)),
            Err(e) => return Err(VaultError::io("消すものを調べられない", &e)),
        };
        let info = FileInfo::from_metadata(&meta);
        match info.kind {
            EntryKind::Link => return Err(outside_link()),
            EntryKind::File => reject_read_only_file(&meta, rel)?,
            EntryKind::Dir => {}
        }
        test_hook!(Step::AfterGate);

        // A：親を根から1段ずつ開いて持つ。2つめの門：消すものを TARGET_* で開き、門の値と比べる
        let chain = self.open_parents(txn, actor, rel, usize::MAX)?;
        let mut target = second_gate(&txn.root, &chain, rel, &meta, "消すものを開けない")?;
        // recursive:false：空かどうかは、移すハンドル（TARGET_DIR）で列挙して決める（確かめたものと移すものが
        // 同じになる）
        if info.kind == EntryKind::Dir && !recursive && target.has_entries()? {
            return Err(VaultError::new(
                ErrorCode::DirNotEmpty,
                format!(
                    "中が空でないフォルダ：{rel}（recursive:true なら中身ごとゴミ箱へ移り、vault_restore で戻せる）"
                ),
            ));
        }
        // entry.json・履歴・索引には、実際の名前（大文字小文字。2つめの門のハンドルの最終のパスから）で書く
        let rel_s = target.actual(&txn.root, rel)?;
        let size = match info.kind {
            EntryKind::Dir => tree_size(&mut target)?,
            _ => target.info().size,
        };
        let trashed = trash::move_to_trash(
            &txn.root,
            &txn.vault,
            txn.lock,
            actor,
            ToTrash {
                target,
                origin: guard::rename_parent(&txn.root, &chain),
                name: last_name(rel)?,
                original_path: &rel_s,
                kind: info.kind,
                size,
                reason: TrashReason::Delete,
            },
        )?;
        let trash_id = trashed.into_id().into_string();
        txn.record(
            actor,
            Op::Delete,
            Record {
                path: Some(rel_s.clone()),
                trash_id: Some(trash_id.clone()),
                size: Some(size),
                ..Record::default()
            },
        )?;
        txn.index(IndexChange::RemoveTreeAnyCase(rel_s.clone()));
        Ok(DeleteResult {
            path: rel_s,
            trash_id,
        })
    }

    fn restore_in(
        &self,
        txn: &mut Txn<'_>,
        actor: &Actor,
        trash_id: &str,
        to: Option<&RelPath>,
        replace: bool,
    ) -> Result<RestoreResult> {
        // .vault/trash がリンクでないことを確かめてハンドルで持ち、trash/ の直下を一覧した名前と、完全に一致したものだけを使う
        let Some(mut trash) = guard::open_trash(&txn.root, &txn.vault)? else {
            return Err(not_in_trash(trash_id));
        };
        let name = find_listed(&mut trash, trash_id)?;
        let id = TrashId::parse(&name).ok_or_else(|| not_in_trash(trash_id))?;
        // <id> を PassDir で開いて持つ（ID も読む。戻したあとの片づけで照らす）
        let item = match guard::open_trash_item(&txn.root, &trash, id, true)? {
            ItemState::Item(item) => item,
            ItemState::Missing => return Err(not_in_trash(trash_id)),
            ItemState::NotDir | ItemState::Busy => {
                return Err(invalid_entry("ゴミ箱の entry がフォルダでない"));
            }
        };
        let entry = item
            .read_entry()
            .as_deref()
            .and_then(parse_entry)
            .ok_or_else(|| {
                invalid_entry("entry.json を読めない（ない・大きすぎる・形が正しくない）")
            })?;
        // original_path も、決定3 の検証（文字列の規則）をもう一度通す（偽の entry.json に備える）
        let original = RelPath::parse(&entry.original_path).map_err(|_| {
            invalid_entry("entry.json の original_path が、保管庫の中の相対パスとして正しくない")
        })?;
        // data を、戻すために開いて持つ（リンクなら INVALID_TRASH_ENTRY）
        let mut data = match item.open_data()? {
            DataState::Data(data) => data,
            DataState::Missing => return Err(invalid_entry("ゴミ箱の中身（data）がない")),
            DataState::Link => {
                return Err(invalid_entry(
                    "ゴミ箱の中身（data）がリンク（ジャンクションを含む）になっている",
                ));
            }
            DataState::Changed => {
                return Err(VaultError::new(
                    ErrorCode::Conflict,
                    "ゴミ箱の中身（data）が、確かめたあとで、ほかのアプリに変えられた。もう一度試す",
                ));
            }
        };
        let data_info = data.info();

        let dest_rel = to.cloned().unwrap_or(original);
        if dest_rel.is_root() {
            return Err(VaultError::new(
                ErrorCode::InvalidPath,
                "戻す先がない（もとの場所がない：書き込み途中の一時ファイルなど）。to を指定して戻す",
            ));
        }
        // 戻す先も、決定3 の検証（文字列とファイルシステムの両方）を通す
        let dest = self.resolve_rel(&dest_rel)?;
        if to.is_some() && data_info.kind == EntryKind::Dir {
            // data の中は、data のハンドルで歩く
            self.check_extent(
                &dest_rel,
                subtree_extent(&mut data)?,
                "戻す先では、中のいちばん深いもの・長いもの",
            )?;
        }
        let dest_s = dest_rel.to_string();

        // 1つめの門：戻す先がふさがっていれば CONFLICT（replace なら、今あるものの値を2つめの門と比べる）
        let current = match fs::symlink_metadata(&dest) {
            Ok(meta) => {
                if !replace {
                    return Err(conflict(&dest_s));
                }
                match FileInfo::from_metadata(&meta).kind {
                    EntryKind::Link => return Err(outside_link()),
                    EntryKind::File => reject_read_only_file(&meta, &dest_rel)?,
                    EntryKind::Dir => {}
                }
                Some(meta)
            }
            Err(e) if is_not_found(&e) => None,
            Err(e) => return Err(VaultError::io("戻す先を調べられない", &e)),
        };
        let existing_parents = self.gate_parents(&dest_rel, true)?;
        test_hook!(Step::AfterGate);

        // A・B：戻す先の親を1段ずつ開く・作る（作った親は履歴に mkdir。閉じて開き直したものを持つ）
        let chain = self.open_parents(txn, actor, &dest_rel, existing_parents)?;
        let dest_name = last_name(&dest_rel)?;

        // replace：今あるものを2つめの門で開き、門の値と比べてから、先にゴミ箱へ移す（restore_replace）
        let replaced = match current {
            Some(meta) => {
                let mut target = second_gate(
                    &txn.root,
                    &chain,
                    &dest_rel,
                    &meta,
                    "戻す先の今あるものを開けない",
                )?;
                // 今ある中身は、実際の名前（大文字小文字）で entry に書く
                let path = target.actual(&txn.root, &dest_rel)?;
                let size = match target.info().kind {
                    EntryKind::Dir => tree_size(&mut target)?,
                    _ => target.info().size,
                };
                let kind = target.info().kind;
                let trashed = trash::move_to_trash(
                    &txn.root,
                    &txn.vault,
                    txn.lock,
                    actor,
                    ToTrash {
                        target,
                        origin: guard::rename_parent(&txn.root, &chain),
                        name: dest_name,
                        original_path: &path,
                        kind,
                        size,
                        reason: TrashReason::RestoreReplace,
                    },
                )?;
                Some(Replaced {
                    trashed,
                    size,
                    path,
                })
            }
            None => None,
        };

        // C：data を、置き換えない rename で戻す（RootDirectory は戻す先の親）。ふさがっていれば CONFLICT。
        // ここから先の失敗は、どれも、replace で移した中身をもとに戻す
        test_hook!(Step::BeforeRename(Rename::Restore));
        let failure = match data.rename_into(
            guard::rename_parent(&txn.root, &chain),
            dest_name,
            "ゴミ箱から戻せない",
        ) {
            Ok(Moved::Done) => None,
            Ok(Moved::Collision) => Some(conflict(&dest_s)),
            Err(e) => Some(e),
        };
        if let Some(e) = failure {
            return Err(self.undo_replace(txn, actor, replaced, &chain, &dest_rel, e));
        }
        // rename のあと、同じハンドルで最終のパスを読み直す（起きないはず：外なら、ゴミ箱の data へ戻す）。
        // 記録（返り値・履歴・索引のキー）は、このハンドルの実際の名前（途中のフォルダも含めた大文字小文字）にそろえる
        let dest_s = match data.check_renamed(&txn.root, &dest_rel) {
            Ok(actual) => actual,
            Err(e) => {
                let e = if item.put_data_back(&data) {
                    e
                } else {
                    e.with_note(&format!("ゴミ箱へ戻せなかった（戻した先：{dest_rel}）"))
                };
                return Err(self.undo_replace(txn, actor, replaced, &chain, &dest_rel, e));
            }
        };
        // 戻したものの大きさと索引の値は、戻した data のハンドルで読む・歩く
        let size = match data_info.kind {
            EntryKind::Dir => tree_size(&mut data).unwrap_or(0),
            _ => data_info.size,
        };
        let changes = self.index_tree(&mut data, &dest_s);
        drop(data);

        // 戻したので、ゴミ箱の entry（entry.json と空の <id>）を片づける（<id> は ID を照らしてから）
        item.finish(&txn.root);
        drop(trash);

        if let Some(r) = &replaced {
            let id = r.trashed.id().as_str().to_owned();
            txn.record(actor, Op::Delete, replaced_record(&r.path, &id, r.size))?;
        }
        txn.record(
            actor,
            Op::Restore,
            Record {
                path: Some(dest_s.clone()),
                trash_id: Some(trash_id.to_owned()),
                size: Some(size),
                ..Record::default()
            },
        )?;
        // 戻した先と大文字小文字だけ違う古いキー（と、replace で移したもののキー）を除いてから、入れる
        txn.index(IndexChange::RemoveTreeAnyCase(dest_s.clone()));
        for change in changes {
            txn.index(change);
        }
        Ok(RestoreResult {
            path: dest_s,
            replaced_trash_id: replaced.map(|r| r.trashed.into_id().into_string()),
        })
    }

    /// 戻すのに失敗したとき：replace で移した今あった中身を、もとの場所へ取り戻す（take_back。置き換えない）。
    /// 取り戻せなければ、ゴミ箱へ移ったことを履歴に残し、居場所をエラーの文に入れる。
    fn undo_replace(
        &self,
        txn: &mut Txn<'_>,
        actor: &Actor,
        replaced: Option<Replaced>,
        chain: &[Pass],
        dest_rel: &RelPath,
        e: VaultError,
    ) -> VaultError {
        let Some(r) = replaced else {
            return e;
        };
        let id = r.trashed.id().as_str().to_owned();
        let name = dest_rel.components().last().unwrap_or_default();
        let parent = guard::rename_parent(&txn.root, chain);
        if guard::take_back(&txn.root, r.trashed, parent, name, dest_rel) {
            return e;
        }
        let _ = txn.record(actor, Op::Delete, replaced_record(&r.path, &id, r.size));
        txn.index(IndexChange::RemoveTreeAnyCase(r.path));
        e.with_note(&format!(
            "いまあった中身はゴミ箱にある（replaced_trash_id: {id}。vault_restore で戻せる）"
        ))
    }

    /// 移した・戻したもの（`rel` とその中）の索引の項目。値は、移したもののハンドルと、その中の項目を属性だけで
    /// 開いて読む。中は、そのハンドルで歩く（リンクはたどらない）。`.vault`・ツールで扱えない名前・上限を超えるものは
    /// 入れない（中へも進まない）。
    pub(crate) fn index_tree(&self, target: &mut Target, rel: &str) -> Vec<IndexChange> {
        let mut changes = Vec::new();
        let Ok(info) = target.current_info() else {
            return changes;
        };
        changes.push(set(rel, info));
        if info.kind != EntryKind::Dir {
            return changes;
        }
        let _ = target.walk(false, &mut |found| {
            if found.name.eq_ignore_ascii_case(RESERVED_NAME) {
                return Ok(false);
            }
            // ツールで扱えない名前は、索引に入れない（一覧と走査と同じ）
            let Some(name) = found.name.to_str().filter(|name| is_valid_component(name)) else {
                return Ok(false);
            };
            let child_rel = if found.parent_rel.is_empty() {
                format!("{rel}/{name}")
            } else {
                format!("{rel}/{}/{name}", found.parent_rel)
            };
            if !self.fits(&child_rel) {
                return Ok(false);
            }
            let Some(info) = found.current() else {
                return Ok(false); // 読むあいだに消えた
            };
            changes.push(set(&child_rel, info));
            Ok(info.kind == EntryKind::Dir)
        });
        changes
    }
}

/// open（書ける状態）のときに、`.vault/tmp` に残った一時ファイルを、reason `orphan_tmp` でゴミ箱へ移す（消さない）。
///
/// 一時ファイルは、書く処理がロックを持っているあいだだけあるので、ロックを取ってから移す。
/// ほかの処理が書いていてロックを取れなければ、片づけは次に回す（open は失敗させない）。
/// もとの場所はないので、original_path は `""`（戻すときは to を指定する）。保管庫の中の見える場所は変わらないので、
/// 履歴には書かない（ゴミ箱の entry に、reason・時刻・actor が残る）。
/// `.vault/tmp` はハンドル（Held）で持ち、そのハンドルで列挙して、項目ごとにリンクそのものを開き（TARGET_*）、
/// 置き換えない rename で移す。
pub(crate) fn move_orphan_tmp(vault: &Vault) {
    let Ok(root) = vault.op_root() else {
        return;
    };
    let Ok(vault_handle) = root.open_vault_dir() else {
        return;
    };
    // .vault/tmp をハンドルで開いて持つ（リンク・フォルダでないなら、片づけない）。残っていなければ、ロックも取らない
    let Ok(Some(mut tmp_dir)) = guard::open_tmp_dir(&root, &vault_handle) else {
        return;
    };
    if !tmp_dir
        .entries("一時ファイルのフォルダを読めない")
        .is_ok_and(|entries| !entries.is_empty())
    {
        return;
    }
    let Ok(lock) = vault.lock_with_timeout(ORPHAN_LOCK_TIMEOUT) else {
        return;
    };
    // ロックを取ったあとに、列挙し直す（そのときに残っているものが、書く処理の残り）
    let Ok(entries) = tmp_dir.entries("一時ファイルのフォルダを読めない") else {
        return;
    };
    let actor = Actor::new(ActorKind::External, "vault");
    for entry in entries {
        let Some(name) = entry.name.to_str().map(str::to_owned) else {
            continue;
        };
        // リンクそのものを開く（フォルダは TARGET_DIR、ほかは TARGET_FILE。管理用の用途なので、書き込みの権利は
        // 求めない）。場所が .vault\tmp\<名前> であること
        let Ok(TargetOpen::Target(mut target)) = guard::open_target(
            tmp_dir.parent(),
            &name,
            entry.info.kind,
            TargetUse::Managed,
            "残った一時ファイルを開けない",
        ) else {
            continue;
        };
        if target
            .check_managed(&root, &ManagedName::TmpFile(name.clone()))
            .is_err()
        {
            continue;
        }
        let kind = target.info().kind;
        // フォルダの大きさは、vault_delete と同じく中のファイルの合計（そのハンドルで歩く）
        let size = match kind {
            EntryKind::Dir => tree_size(&mut target).unwrap_or(0),
            _ => target.info().size,
        };
        let _ = trash::move_to_trash(
            &root,
            &vault_handle,
            &lock,
            &actor,
            ToTrash {
                target,
                origin: &tmp_dir,
                name: &name,
                original_path: "",
                kind,
                size,
                reason: TrashReason::OrphanTmp,
            },
        )
        .map(Trashed::into_id);
    }
}

/// replace でゴミ箱へ移した、今あった中身。
struct Replaced {
    trashed: Trashed,
    size: u64,
    /// 実際の名前（大文字小文字）での相対パス。
    path: String,
}

/// trash/ の直下を、持っている .vault/trash のハンドルで列挙し（E）、名前が `id` と完全に一致するものの名前を返す。
/// なければ `NOT_FOUND`。
fn find_listed(trash: &mut Held, id: &str) -> Result<String> {
    if trash
        .entries("ゴミ箱を読めない")?
        .iter()
        .any(|entry| entry.name == OsStr::new(id))
    {
        return Ok(id.to_owned());
    }
    Err(not_in_trash(id))
}

fn not_in_trash(id: &str) -> VaultError {
    VaultError::new(
        ErrorCode::NotFound,
        format!("ゴミ箱にない：{id}（vault_trash_list で確かめる）"),
    )
}

/// 移すフォルダの中のファイルの大きさの合計（そのハンドル（TARGET_DIR）で歩く。リンクはたどらない）。
/// ファイルの大きさは、項目を属性だけで開いて読む（開けなければ列挙の値）。
fn tree_size(target: &mut Target) -> Result<u64> {
    let mut total = 0u64;
    target.walk(true, &mut |found| {
        if found.listed.kind == EntryKind::File
            && let Some(info) = found.current()
            && info.kind == EntryKind::File
        {
            total = total.saturating_add(info.size);
        }
        Ok(true)
    })?;
    Ok(total)
}

fn set(rel: &str, info: FileInfo) -> IndexChange {
    IndexChange::Set {
        path: rel.to_owned(),
        entry: IndexEntry::from(info),
        drop_case_variants: false,
    }
}

fn replaced_record(path: &str, trash_id: &str, size: u64) -> Record {
    Record {
        path: Some(path.to_owned()),
        trash_id: Some(trash_id.to_owned()),
        size: Some(size),
        ..Record::default()
    }
}

fn conflict(dest: &str) -> VaultError {
    VaultError::new(
        ErrorCode::Conflict,
        format!(
            "戻す先にすでに何かある：{dest}（replace:true なら、今ある中身をゴミ箱へ移してから戻す。to で別の場所も選べる）"
        ),
    )
}

fn invalid_entry(message: &str) -> VaultError {
    VaultError::new(ErrorCode::InvalidTrashEntry, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_match_the_adr() {
        assert_eq!(DEFAULT_TRASH_LIST_LIMIT, 100);
        assert_eq!(MAX_TRASH_LIST_LIMIT, 10_000);
    }

    #[test]
    fn trash_items_flatten_the_entry() {
        let item = TrashItem {
            trash_id: "1-2-3".to_owned(),
            entry: TrashEntry {
                original_path: "a.txt".to_owned(),
                kind: EntryKind::File,
                size: 1,
                deleted_at: "2026-11-16T00:00:00.000Z".to_owned(),
                actor: Actor::user("app"),
                reason: TrashReason::Delete,
            },
        };
        let json = serde_json::to_value(&item).unwrap();
        for key in [
            "trash_id",
            "original_path",
            "kind",
            "size",
            "deleted_at",
            "actor",
            "reason",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["reason"], "delete");
    }
}
