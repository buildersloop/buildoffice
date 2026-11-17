//! ツールの受付口（`call`）と、ツールの定義（`tool_specs()`）。
//!
//! MCP（vault-mcp）も FFI（vault-ffi）も、この `call` だけを通る。読み取り専用の判定もここで行う。
//! ツールの名前の一覧は `crates/vault-core/tool_names.json` にも置き、テストで `tool_specs()` と一致させる。

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::actor::Actor;
use crate::error::{ErrorCode, Result, VaultError};
use crate::ops::{DEFAULT_LIST_LIMIT, DEFAULT_READ_BYTES, Encoding, WriteRequest};
use crate::scan::DEFAULT_CHANGES_LIMIT;
use crate::trash_ops::DEFAULT_TRASH_LIST_LIMIT;
use crate::vault::Vault;

/// 書くツール（読み取り専用のときは `READ_ONLY`。tools/list にも出さない）。
pub const WRITE_TOOLS: [&str; 5] = [
    "vault_write",
    "vault_mkdir",
    "vault_move",
    "vault_delete",
    "vault_restore",
];

/// ツールの定義。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// 名前（英数字・`_`）。
    pub name: &'static str,
    /// 説明（AI が読む）。
    pub description: &'static str,
    /// 引数の JSON Schema。引数なしは `{"type":"object","additionalProperties":false}`。
    pub input_schema: Value,
    /// 書くツールか（読み取り専用のときは使えない）。
    pub writes: bool,
    /// annotations の readOnlyHint（クライアントへのヒントでしかなく、守りの手段にはしない）。
    pub read_only_hint: bool,
    /// annotations の destructiveHint。
    pub destructive_hint: bool,
}

const PATH_NOTE: &str = "保管庫の中の相対パス（区切りは /、根は \"\"）。.vault、~ のあとに数字が続く名前（例：file~1.txt）、Windows の予約名（CON など）、末尾が . か空白の名前は使えない";

/// 10個のツールの定義（いつも同じ順番）。
pub fn tool_specs() -> Vec<ToolSpec> {
    let path = json!({"type": "string", "description": PATH_NOTE});
    vec![
        ToolSpec {
            name: "vault_info",
            description: "保管庫の情報（フォルダの名前・vault_id・format・読み取り専用か）を返す。",
            input_schema: json!({"type": "object", "additionalProperties": false}),
            writes: false,
            read_only_hint: true,
            destructive_hint: false,
        },
        ToolSpec {
            name: "vault_list",
            description: "フォルダの中の一覧を返す（フォルダを先に、名前の順）。recursive で下のフォルダも並べる。リンクは kind:\"link\" で示し、たどらない。ほかのアプリで作られた、深すぎる・長すぎるパス（64段・260字を超えるもの）は出さず、skipped_over_limit に数だけを返す（祖先のフォルダを vault_delete すれば、まとめてゴミ箱へ移せる）。ツールで扱えない名前（使えない文字・予約名・~ と数字など）も出さず、skipped_invalid_names に数だけを返す。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": PATH_NOTE, "default": ""},
                    "recursive": {"type": "boolean", "default": false},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 10000, "default": DEFAULT_LIST_LIMIT}
                },
                "additionalProperties": false
            }),
            writes: false,
            read_only_hint: true,
            destructive_hint: false,
        },
        ToolSpec {
            name: "vault_read",
            description: "ファイルを読む。offset から最大 max_bytes バイト（既定 256 KiB、上限 16 MiB）。文字でないファイルは encoding:\"base64\" で読む。truncated が true なら続きがある。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "encoding": {"type": "string", "enum": ["utf8", "base64"], "default": "utf8"},
                    "offset": {"type": "integer", "minimum": 0, "default": 0},
                    "max_bytes": {"type": "integer", "minimum": 0, "maximum": 16_777_216, "default": DEFAULT_READ_BYTES}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            writes: false,
            read_only_hint: true,
            destructive_hint: false,
        },
        ToolSpec {
            name: "vault_write",
            description: "ファイルを書く（中身は 16 MiB まで）。上書きした古い中身はゴミ箱に入り、vault_restore で戻せる（previous_trash_id）。expected_version を渡すと、版が合わないとき CONFLICT になる。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "content": {"type": "string"},
                    "encoding": {"type": "string", "enum": ["utf8", "base64"], "default": "utf8"},
                    "overwrite": {"type": "boolean", "default": true},
                    "create_dirs": {"type": "boolean", "default": true},
                    "expected_version": {"type": "string", "description": "vault_list・vault_read が返す version（\"<size>-<mtime_ns>\"）"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            writes: true,
            read_only_hint: false,
            destructive_hint: true,
        },
        ToolSpec {
            name: "vault_mkdir",
            description: "フォルダを作る（途中のフォルダもなければ作る）。すでにフォルダがあれば created:false。作ったフォルダは vault_delete で消せる。",
            input_schema: json!({
                "type": "object",
                "properties": {"path": path},
                "required": ["path"],
                "additionalProperties": false
            }),
            writes: true,
            read_only_hint: false,
            destructive_hint: false,
        },
        ToolSpec {
            name: "vault_move",
            description: "ファイルかフォルダを移す（名前を変える）。移す先にすでに何かあれば ALREADY_EXISTS（上書きしない）。逆に移せば戻せる。",
            input_schema: json!({
                "type": "object",
                "properties": {"from": path, "to": path},
                "required": ["from", "to"],
                "additionalProperties": false
            }),
            writes: true,
            read_only_hint: false,
            destructive_hint: true,
        },
        ToolSpec {
            name: "vault_delete",
            description: "ファイルかフォルダを消す（ゴミ箱へ移す）。vault_restore で戻せる。中身のあるフォルダは recursive:true が要る。読み取り専用の印が付いたファイルは ACCESS_DENIED（フォルダごと recursive で消すなら、中にあっても移せる）。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "recursive": {"type": "boolean", "default": false}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            writes: true,
            read_only_hint: false,
            destructive_hint: true,
        },
        ToolSpec {
            name: "vault_trash_list",
            description: "ゴミ箱の一覧を返す（新しい順）。vault_restore に渡す trash_id がわかる。truncated が true なら、limit で切った（まだある）。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 10000, "default": 100}
                },
                "additionalProperties": false
            }),
            writes: false,
            read_only_hint: true,
            destructive_hint: false,
        },
        ToolSpec {
            name: "vault_restore",
            description: "ゴミ箱から戻す。戻す先にすでに何かあれば CONFLICT（replace:true なら、いまある中身をゴミ箱へ移してから戻す。移した中身も vault_restore で戻せる）。original_path が空の entry（reason が orphan_tmp：書き込み途中の一時ファイル）は、もとの場所がないので to が要る。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "trash_id": {"type": "string", "pattern": "^[0-9]{1,20}-[0-9]{1,10}-[0-9]{1,20}$"},
                    "to": {"type": "string", "description": "戻す先（省けば、もとの場所）。".to_owned() + PATH_NOTE},
                    "replace": {"type": "boolean", "default": false}
                },
                "required": ["trash_id"],
                "additionalProperties": false
            }),
            writes: true,
            read_only_hint: false,
            destructive_hint: true,
        },
        ToolSpec {
            name: "vault_changes",
            description: "変更の履歴を返す（seq が since_seq より大きいものを、古い順に limit 件まで）。rescan:true（既定）なら、先に保管庫を調べて、ほかのアプリでの作成・更新・削除・名前の変更も履歴に入れる（actor は external/fs）。前の走査からの間隔（2秒と、前の走査にかかった時間の10倍（10分まで）の長いほう）が過ぎていなければ、調べずに履歴だけを返す（scanned:false。そのときの変更は次の走査で出る）。scan_incomplete が true なら、履歴の行が欠けていた、または索引がなくて合わせ直せなかったので、調べた結果を記録せずに終えた（incomplete_scans は、続けてそうなった回数。増え続けるなら、管理用のフォルダの履歴や索引をほかのアプリが書き換えている・消している）。完全な変更の検知ではない：大きさも更新時刻も変えずに中身だけを変えたもの、ハードリンクの別の名前から書き換えたものは見つからないことがある。続きは next_seq を since_seq に渡して読む。truncated が true なら、まだある。skipped_over_limit・skipped_invalid_names は、上限（64段・260字）を超える・ツールで扱えない名前なので調べなかった項目の数。読み取り専用でも、rescan は管理用のフォルダ（.vault）の索引と履歴を書き換える。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "since_seq": {"type": "integer", "minimum": 0, "default": 0},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 10000, "default": 200},
                    "rescan": {"type": "boolean", "default": true}
                },
                "additionalProperties": false
            }),
            writes: false,
            read_only_hint: true,
            destructive_hint: false,
        },
    ]
}

/// ツールを呼ぶ（受付口）。`args` が null のときは `{}` として扱う。
///
/// 順番：知らない名前なら `UNKNOWN_TOOL` → 読み取り専用で書くツールなら `READ_ONLY` →
/// 引数を読む（知らない引数・型の誤りは `INVALID_ARGUMENT`）→ ツールを動かす。
pub fn call(vault: &Vault, tool: &str, args: &Value, actor: &Actor) -> Result<Value> {
    let Some(writes) = writes(tool) else {
        return Err(VaultError::new(
            ErrorCode::UnknownTool,
            "知らないツールの名前",
        ));
    };
    if writes && vault.is_read_only() {
        return Err(VaultError::new(
            ErrorCode::ReadOnly,
            "読み取り専用で開いた保管庫には書けない",
        ));
    }
    match tool {
        "vault_info" => {
            parse::<NoArgs>(args)?;
            to_value(&vault.info())
        }
        "vault_list" => {
            let a: ListArgs = parse(args)?;
            to_value(&vault.list(&a.path, a.recursive, a.limit)?)
        }
        "vault_read" => {
            let a: ReadArgs = parse(args)?;
            to_value(&vault.read(&a.path, a.encoding, a.offset, a.max_bytes)?)
        }
        "vault_write" => {
            let a: WriteArgs = parse(args)?;
            let req = WriteRequest {
                path: &a.path,
                content: &a.content,
                encoding: a.encoding,
                overwrite: a.overwrite,
                create_dirs: a.create_dirs,
                expected_version: a.expected_version.as_deref(),
            };
            to_value(&vault.write(actor, &req)?)
        }
        "vault_mkdir" => {
            let a: PathArgs = parse(args)?;
            to_value(&vault.mkdir(actor, &a.path)?)
        }
        "vault_move" => {
            let a: MoveArgs = parse(args)?;
            to_value(&vault.move_path(actor, &a.from, &a.to)?)
        }
        "vault_delete" => {
            let a: DeleteArgs = parse(args)?;
            to_value(&vault.delete(actor, &a.path, a.recursive)?)
        }
        "vault_trash_list" => {
            let a: TrashListArgs = parse(args)?;
            to_value(&vault.trash_list(a.limit)?)
        }
        "vault_restore" => {
            let a: RestoreArgs = parse(args)?;
            to_value(&vault.restore(actor, &a.trash_id, a.to.as_deref(), a.replace)?)
        }
        "vault_changes" => {
            let a: ChangesArgs = parse(args)?;
            to_value(&vault.changes(a.since_seq, a.limit, a.rescan)?)
        }
        // 知らない名前は、最初の writes で UNKNOWN_TOOL にしている
        _ => Err(VaultError::new(
            ErrorCode::UnknownTool,
            "知らないツールの名前",
        )),
    }
}

/// 名前が知っているツールなら、書くツールか（知らなければ None）。
fn writes(tool: &str) -> Option<bool> {
    match tool {
        "vault_info" | "vault_list" | "vault_read" | "vault_trash_list" | "vault_changes" => {
            Some(false)
        }
        "vault_write" | "vault_mkdir" | "vault_move" | "vault_delete" | "vault_restore" => {
            Some(true)
        }
        _ => None,
    }
}

const MAX_ARG_ERROR_CHARS: usize = 300;

fn parse<T: DeserializeOwned>(args: &Value) -> Result<T> {
    let empty = Value::Object(Map::new());
    let args = if args.is_null() { &empty } else { args };
    if !args.is_object() {
        return Err(VaultError::new(
            ErrorCode::InvalidArgument,
            "引数は JSON のオブジェクトで渡す",
        ));
    }
    T::deserialize(args).map_err(|e| {
        let detail: String = e.to_string().chars().take(MAX_ARG_ERROR_CHARS).collect();
        VaultError::new(
            ErrorCode::InvalidArgument,
            format!("引数が正しくない：{detail}"),
        )
    })
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|_| VaultError::new(ErrorCode::Internal, "返り値を作れない"))
}

fn yes() -> bool {
    true
}

fn default_list_limit() -> u64 {
    DEFAULT_LIST_LIMIT
}

fn default_read_bytes() -> u64 {
    DEFAULT_READ_BYTES
}

fn default_changes_limit() -> u64 {
    DEFAULT_CHANGES_LIMIT
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    path: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default = "default_list_limit")]
    limit: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    encoding: Encoding,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_read_bytes")]
    max_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    encoding: Encoding,
    #[serde(default = "yes")]
    overwrite: bool,
    #[serde(default = "yes")]
    create_dirs: bool,
    #[serde(default)]
    expected_version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
}

fn default_trash_list_limit() -> u64 {
    DEFAULT_TRASH_LIST_LIMIT
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrashListArgs {
    #[serde(default = "default_trash_list_limit")]
    limit: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreArgs {
    trash_id: String,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    replace: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesArgs {
    #[serde(default)]
    since_seq: u64,
    #[serde(default = "default_changes_limit")]
    limit: u64,
    #[serde(default = "yes")]
    rescan: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 網羅率：`tool_specs()` の名前 ＝ `crates/vault-core/tool_names.json`（順番も同じ）。
    #[test]
    fn tool_specs_match_tool_names_json() {
        let file: Vec<String> = serde_json::from_str(include_str!("../tool_names.json")).unwrap();
        let names: Vec<String> = tool_specs().iter().map(|s| s.name.to_owned()).collect();
        assert_eq!(names, file);
        assert_eq!(names.len(), 10);
    }

    #[test]
    fn every_tool_name_is_known_to_call() {
        for spec in tool_specs() {
            assert_eq!(writes(spec.name), Some(spec.writes), "{}", spec.name);
        }
        assert_eq!(writes("vault_unknown"), None);
    }

    #[test]
    fn write_tools_and_annotations_match_the_adr() {
        let specs = tool_specs();
        let writing: Vec<&str> = specs.iter().filter(|s| s.writes).map(|s| s.name).collect();
        assert_eq!(writing, WRITE_TOOLS);
        for spec in &specs {
            let read_only = matches!(
                spec.name,
                "vault_info" | "vault_list" | "vault_read" | "vault_trash_list" | "vault_changes"
            );
            let destructive = matches!(
                spec.name,
                "vault_delete" | "vault_move" | "vault_write" | "vault_restore"
            );
            assert_eq!(spec.read_only_hint, read_only, "{}", spec.name);
            assert_eq!(spec.destructive_hint, destructive, "{}", spec.name);
            if destructive {
                assert!(spec.description.contains("戻せる"), "{}", spec.name);
            }
        }
    }

    #[test]
    fn input_schemas_are_closed_objects() {
        for spec in tool_specs() {
            let schema = &spec.input_schema;
            assert_eq!(schema["type"], "object", "{}", spec.name);
            assert_eq!(schema["additionalProperties"], false, "{}", spec.name);
            let props = schema.get("properties").and_then(Value::as_object);
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                let props = props.unwrap();
                for r in required {
                    assert!(props.contains_key(r.as_str().unwrap()), "{}", spec.name);
                }
            }
            // 名前は 1〜128 字の英数字・_・-・.（MCP の Tool の定義）
            assert!(
                (1..=128).contains(&spec.name.len())
                    && spec
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
            );
        }
        // 引数なしのツール
        assert_eq!(
            tool_specs()[0].input_schema,
            json!({"type": "object", "additionalProperties": false})
        );
    }
}
