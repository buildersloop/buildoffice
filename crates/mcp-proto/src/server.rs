//! MCP のサーバー。1行を読んで、決まった順番で処理し、返事を1行で書く。
//!
//! 処理の順番（1行ずつ）：
//! 1. 1行を読む（32 MiB まで。超えたら改行まで読み捨てて -32600、id は null）
//! 2. JSON として読む（serde_json の既定の深さの上限 128 のまま）。読めなければ -32700
//! 3. 配列（バッチ）なら -32600
//! 4. 形を確かめる（`jsonrpc`・`method`・`id`）。不正なら -32600
//! 5. 通知（id のないもの）には返事をしない。受付口にも渡さない
//! 6. 回数の制限を数える（id のある要求だけ）。超えたら -31000
//! 7. 方式を決める（Modern・Legacy・`initialize` の前）。版の確認の -32022・-32602 はここ
//! 8. メソッドごとに処理する。tools/call は、どの方式でも同じ受付口（`ToolHost::call`）を通る

use std::io::{self, BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use safe_log::{Name, name};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::limit::RateLimiter;
use crate::line::{Line, MAX_LINE_BYTES, read_line};

/// Modern の版（`_meta` で使える、ただ1つの版）。
pub const MODERN_VERSION: &str = "2026-07-28";
/// Legacy の版（`initialize` で取り決める。新しい順）。
pub const LEGACY_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// 相手の版がどれでもないときに返す Legacy の版。
pub const DEFAULT_LEGACY_VERSION: &str = "2025-11-25";

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
/// キャッシュのヒントの ttlMs（0 ＝ すぐ古いものとして扱ってよい）。
const CACHE_TTL_MS: u64 = 0;
/// キャッシュのヒントの cacheScope（private ＝ ほかのクライアントと共有しない）。
const CACHE_SCOPE: &str = "private";

/// actor の名前の長さの上限（文字）。
pub const MAX_ACTOR_NAME_CHARS: usize = 64;
/// 名前がないとき・整えて空になったときの名前。
pub const UNKNOWN_ACTOR_NAME: &str = "unknown";
/// 「知らないメソッド」の返事の文に入れるメソッド名の長さの上限。
const MAX_REPLY_NAME_CHARS: usize = 64;
/// -32022 の data.requested に、そのまま返す版の長さの上限（印字できる ASCII のときだけ返す）。
const MAX_REQUESTED_VERSION_CHARS: usize = 64;
/// 1行の中で、文字列の外にある `[`・`{`・`,` の数の上限（Value のメモリを抑える）。
/// 数え方と値は json-guard にまとめてある（vault-ffi と共有）。
pub const MAX_JSON_ELEMENTS: usize = json_guard::MAX_ELEMENTS;

/// ログに書くメソッド名の一覧（mcp-proto が扱うメソッドだけ）。ほかのメソッド名は `?` にする。
const KNOWN_METHODS: &[Name] = &[
    name!("initialize"),
    name!("ping"),
    name!("server/discover"),
    name!("tools/list"),
    name!("tools/call"),
    name!("notifications/initialized"),
    name!("notifications/cancelled"),
];
/// ログの書き手の名前と、キー。
const LOG_SOURCE: Name = name!("mcp");
const LOG_METHOD: Name = name!("method");
const LOG_TOOL: Name = name!("tool");
const LOG_RESULT: Name = name!("result");
const LOG_MS: Name = name!("ms");
/// ログの result の名前（エラーの番号とツールのエラーのコードのほか）。
const RESULT_OK: Name = name!("ok");
const RESULT_NOTIFICATION: Name = name!("notification");
const RESULT_NOTIFICATION_IGNORED: Name = name!("notification-ignored");
const RESULT_RESPONSE_IGNORED: Name = name!("response-ignored");

/// JSON-RPC のエラーの番号。
pub mod codes {
    /// JSON として読めない（入れ子が深すぎるものも）。
    pub const PARSE_ERROR: i64 = -32700;
    /// 要求の形が不正・バッチ・1行が大きすぎる。
    pub const INVALID_REQUEST: i64 = -32600;
    /// 知らないメソッド。
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// 引数が不正（知らないツール、Modern の `_meta` の必須の項目がない、`initialize` の前で `_meta` がない）。
    pub const INVALID_PARAMS: i64 = -32602;
    /// 内部のエラー。
    pub const INTERNAL_ERROR: i64 = -32603;
    /// 対応していない版（MCP の仕様が決める番号）。
    pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
    /// 回数の制限を超えた（値は提案。-32768〜-32000 と -32020〜-32099 の外）。
    pub const RATE_LIMITED: i64 = -31000;
}

/// サーバーの名前と版と、使い方の説明（`server/discover` の instructions）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub instructions: String,
}

/// tools/list に出すツールの定義。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    /// 名前（英数字・`_`）。
    pub name: String,
    /// 説明（AI が読む）。
    pub description: String,
    /// 引数の JSON Schema。
    pub input_schema: Value,
    /// annotations の `readOnlyHint`。
    pub read_only_hint: bool,
    /// annotations の `destructiveHint`。
    pub destructive_hint: bool,
}

/// ツールの呼び出しの失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// 知らないツール（JSON-RPC のエラー -32602 にする）。
    UnknownTool,
    /// ツールの実行のエラー（`isError:true`。text は「CODE: 説明」、`structuredContent:{error:{code,message}}`）。
    Tool { code: String, message: String },
}

/// ツールの一覧と、ツールを呼ぶ処理（受付口）。mcp-proto は保管庫のことを知らないので、外から渡す。
pub trait ToolHost {
    /// tools/list に出すツール（この順番のまま返す。毎回同じ順にする）。
    fn tools(&self) -> Vec<ToolDef>;

    /// ツールを呼ぶ。`arguments` は、無い・null なら `{}`（それ以外の形もそのまま渡す。確かめるのは受け取る側）。
    /// 要求から取り出したものを、写さずに渡す（受け取る側も、写さずに使う）。
    /// `actor_id` は `mcp:<名前>`。MCP の通り道から来た操作なので、受け取る側は actor の kind を常に `ai` にする
    /// （kind はここでは渡さない。クライアントが送ってきた値からは決めない）。
    /// 結果は JSON のオブジェクトにする（`structuredContent` はオブジェクトのため）。オブジェクトでなければ -32603 にする。
    fn call(&mut self, name: &str, arguments: &Value, actor_id: &str) -> Result<Value, CallError>;

    /// ログに書いてよい名前の一覧（ツールの名前と、`CallError::Tool` のエラーのコード）。`safe_log::name!` で作る。
    /// ログには、送られたツール名・返したコードがこの一覧にあるときだけ一覧の名前を書き、ほかは `?` にする。
    fn log_names(&self) -> &[Name];
}

/// 要求を処理する方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Era {
    Modern,
    Legacy(LegacyVersion),
}

/// Legacy の版（古い順に並べ、比べられるようにする）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LegacyVersion {
    V2024_11_05,
    V2025_03_26,
    V2025_06_18,
    V2025_11_25,
}

impl LegacyVersion {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "2024-11-05" => Some(Self::V2024_11_05),
            "2025-03-26" => Some(Self::V2025_03_26),
            "2025-06-18" => Some(Self::V2025_06_18),
            "2025-11-25" => Some(Self::V2025_11_25),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::V2024_11_05 => "2024-11-05",
            Self::V2025_03_26 => "2025-03-26",
            Self::V2025_06_18 => "2025-06-18",
            Self::V2025_11_25 => "2025-11-25",
        }
    }
}

impl Era {
    /// `structuredContent` を付けるか（2025-06-18 以降と Modern）。
    fn structured(self) -> bool {
        match self {
            Self::Modern => true,
            Self::Legacy(v) => v >= LegacyVersion::V2025_06_18,
        }
    }

    /// ツールの `annotations` を付けるか（2025-03-26 以降と Modern）。
    fn annotations(self) -> bool {
        match self {
            Self::Modern => true,
            Self::Legacy(v) => v >= LegacyVersion::V2025_03_26,
        }
    }
}

/// `initialize` で取り決めたこと（そのプロセスのあいだ使う）。
#[derive(Debug, Clone)]
struct Negotiated {
    version: LegacyVersion,
    /// actor の id（`mcp:<名前>`）。initialize の時点で、上書きか clientInfo.name から作って整えたもの。
    actor_id: String,
}

/// ログの1つの欄の値。名前は一覧の側の名前だけで、送られた文字そのものは持たない。
#[derive(Debug, Clone, Copy, Default)]
enum Logged {
    /// ない（`-`）。
    #[default]
    Missing,
    /// 一覧の名前・こちらが決めた名前。
    Name(Name),
    /// 送られたが、一覧にない（`?`）。
    Unknown,
    /// JSON-RPC のエラーの番号。
    Code(i64),
}

impl Logged {
    /// 送られた文字を一覧と照らす（あれば一覧の名前、なければ `?`）。
    fn pick(input: &str, allowed: &[Name]) -> Self {
        safe_log::pick(input, allowed).map_or(Self::Unknown, Self::Name)
    }

    fn put(self, line: safe_log::Line, key: Name) -> safe_log::Line {
        match self {
            Self::Missing => line.missing(key),
            Self::Name(name) => line.name(key, name),
            Self::Unknown => line.unknown(key),
            Self::Code(code) => line.int(key, code),
        }
    }
}

/// 1つの要求のログ（メソッド名・ツール名・結果のコード・かかった時間だけ。引数と中身は入れない）。
/// 書くときは safe-log の1行（`mcp: method=… tool=… result=… ms=…`）に組み立てる。
#[derive(Debug, Clone, Default)]
struct LogLine {
    method: Logged,
    tool: Logged,
    result: Logged,
    elapsed: Duration,
}

impl LogLine {
    fn to_line(&self) -> safe_log::Line {
        let line = safe_log::Line::new(LOG_SOURCE);
        let line = self.method.put(line, LOG_METHOD);
        let line = self.tool.put(line, LOG_TOOL);
        let line = self.result.put(line, LOG_RESULT);
        line.ms(LOG_MS, self.elapsed)
    }
}

/// MCP のサーバー（stdio・改行区切りの JSON-RPC 2.0。Modern と Legacy の両対応）。
pub struct Server<H> {
    host: H,
    info: ServerInfo,
    actor_override: Option<String>,
    negotiated: Option<Negotiated>,
    limiter: RateLimiter,
    clock: Box<dyn Fn() -> Instant>,
}

impl<H: ToolHost> Server<H> {
    /// サーバーを作る。
    pub fn new(host: H, info: ServerInfo) -> Self {
        Self {
            host,
            info,
            actor_override: None,
            negotiated: None,
            limiter: RateLimiter::new(),
            clock: Box::new(Instant::now),
        }
    }

    /// actor の名前を、clientInfo の name の代わりに使う（vault-mcp の `--actor-id`）。整え方は同じ。
    #[must_use]
    pub fn with_actor_override(mut self, name: Option<String>) -> Self {
        self.actor_override = name;
        self
    }

    /// 回数の制限に使う時計を替える（テストで、時間を進めるため）。
    #[must_use]
    pub fn with_clock(mut self, clock: impl Fn() -> Instant + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// 受付口。
    pub fn host(&self) -> &H {
        &self.host
    }

    /// stdin（`input`）から1行ずつ読み、返事を stdout（`output`）に1行ずつ書く。`log` には1要求1行のログを書く。
    /// stdin が閉じられたら（EOF）終わる。`output` に書けなくなったら、そのエラーで終わる。
    /// ログの時間（ms）は、1行を読み終えたあとから測る（次の行を待つ時間は入れない）。
    pub fn serve(
        &mut self,
        mut input: impl BufRead,
        mut output: impl Write,
        mut log: impl Write,
    ) -> io::Result<()> {
        let mut buf = Vec::new();
        loop {
            let line = read_line(&mut input, &mut buf, MAX_LINE_BYTES)?;
            let start = Instant::now();
            let mut entry = LogLine::default();
            let reply = match line {
                Line::Eof => return Ok(()),
                Line::TooLong => {
                    entry.result = Logged::Code(codes::INVALID_REQUEST);
                    Some(error_reply(
                        Value::Null,
                        codes::INVALID_REQUEST,
                        &format!(
                            "1行が大きすぎる（{} MiB まで）",
                            MAX_LINE_BYTES / (1024 * 1024)
                        ),
                        None,
                    ))
                }
                Line::Complete => {
                    // JSON の空白（空白・タブ・CR）だけの行は、メッセージでないので何もしない（数えない）
                    if buf.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r')) {
                        continue;
                    }
                    self.handle(&buf, &mut entry)
                }
            };
            if let Some(reply) = reply {
                serde_json::to_writer(&mut output, &reply).map_err(io::Error::other)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
            entry.elapsed = start.elapsed();
            let _ = writeln!(log, "{}", entry.to_line());
        }
    }

    /// 1つのメッセージを処理する。返事がなければ None（通知など）。
    fn handle(&mut self, line: &[u8], log: &mut LogLine) -> Option<Value> {
        // 2. JSON として読む。読む前に、要素の数を数えて、多すぎれば読まない（Value は入力より何十倍も大きくなるため）
        if json_guard::too_many_elements(line, MAX_JSON_ELEMENTS) {
            log.result = Logged::Code(codes::INVALID_REQUEST);
            return Some(error_reply(
                Value::Null,
                codes::INVALID_REQUEST,
                "要素が多すぎる（文字列の外の [ と { と , は、1行に 100 万個まで）",
                None,
            ));
        }
        // 深さの上限は serde_json の既定 128 のまま
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            log.result = Logged::Code(codes::PARSE_ERROR);
            return Some(error_reply(
                Value::Null,
                codes::PARSE_ERROR,
                "JSON として読めない（入れ子が深すぎるものも読まない）",
                None,
            ));
        };
        // 3. バッチ
        let Value::Object(mut object) = message else {
            log.result = Logged::Code(codes::INVALID_REQUEST);
            let text = if message.is_array() {
                "バッチ（JSON-RPC の配列）は受け付けない。1行に1つの要求を送る"
            } else {
                "JSON-RPC の要求は、オブジェクトで送る"
            };
            return Some(error_reply(Value::Null, codes::INVALID_REQUEST, text, None));
        };
        // クライアントからの返事（method がなく result か error がある）：こちらは要求を送らないので、無視する
        if !object.contains_key("method")
            && (object.contains_key("result") || object.contains_key("error"))
        {
            log.result = Logged::Name(RESULT_RESPONSE_IGNORED);
            return None;
        }
        // 4. 形を確かめる。id は、文字列か、i64・u64 で表せる数だけ（小数・大きすぎる数・-0 は使えない）
        let (id, id_ok) = match object.remove("id") {
            None => (None, true),
            Some(id @ Value::String(_)) => (Some(id), true),
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => (Some(Value::Number(n)), true),
            Some(_) => (None, false),
        };
        let method = match object.remove("method") {
            Some(Value::String(method)) => Some(method),
            _ => None,
        };
        log.method = method.as_deref().map_or(Logged::Missing, |method| {
            Logged::pick(method, KNOWN_METHODS)
        });
        let shape_error = if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            Some("jsonrpc は \"2.0\" にする")
        } else if method.is_none() {
            Some("method（文字列）がない")
        } else if !id_ok {
            Some(
                "id は文字列か整数（i64・u64 で表せるもの）にする。null・小数・大きすぎる数は使えない",
            )
        } else {
            None
        };
        if let Some(text) = shape_error {
            log.result = Logged::Code(codes::INVALID_REQUEST);
            return Some(error_reply(
                id.unwrap_or(Value::Null),
                codes::INVALID_REQUEST,
                text,
                None,
            ));
        }
        let method = method.unwrap_or_default();
        // 5. 通知には返事をしない。initialized・cancelled 以外は無視し、受付口にも渡さない
        let Some(id) = id else {
            log.result = Logged::Name(match method.as_str() {
                "notifications/initialized" | "notifications/cancelled" => RESULT_NOTIFICATION,
                _ => RESULT_NOTIFICATION_IGNORED,
            });
            return None;
        };
        // 6. 回数の制限（方式と版の確認より前）
        if !self.limiter.allow((self.clock)()) {
            log.result = Logged::Code(codes::RATE_LIMITED);
            return Some(error_reply(id, codes::RATE_LIMITED, "Rate limited", None));
        }
        // 7・8. 方式を決めて、メソッドごとに処理する（params は取り出して渡し、写さない）
        let params = object.remove("params");
        drop(object);
        let reply = match self.request(&method, params, log) {
            Ok(result) => {
                if matches!(log.result, Logged::Missing) {
                    log.result = Logged::Name(RESULT_OK);
                }
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }
            Err(e) => {
                log.result = Logged::Code(e.code);
                error_reply(id, e.code, &e.message, e.data)
            }
        };
        Some(reply)
    }

    /// 7・8. 方式を決めて、メソッドごとに処理する。
    fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        log: &mut LogLine,
    ) -> Result<Value, RpcError> {
        let params = match params {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(params)) => params,
            Some(_) => return Err(invalid_params("params は、オブジェクトで渡す")),
        };
        // _meta：なし・null は「ない」。ほかの形は -32602
        let meta = match params.get("_meta") {
            None | Some(Value::Null) => None,
            Some(Value::Object(meta)) => Some(meta),
            Some(_) => return Err(invalid_params("params._meta は、オブジェクトで渡す")),
        };

        // Modern：_meta に protocolVersion がある要求（initialize の前でもあとでも、要求ごとに _meta を優先）
        if let Some(meta) = meta
            && let Some(version) = meta.get(META_PROTOCOL_VERSION)
        {
            let Some(version) = version.as_str() else {
                return Err(invalid_params(
                    "_meta の io.modelcontextprotocol/protocolVersion は、文字列で渡す",
                ));
            };
            if version != MODERN_VERSION {
                let mut data = json!({ "supported": [MODERN_VERSION] });
                // 返すのは、印字できる ASCII で 64 字までのときだけ（大きな文字や変な文字を、そのまま返さない）
                if version.chars().count() <= MAX_REQUESTED_VERSION_CHARS
                    && version.chars().all(|c| matches!(c, ' '..='~'))
                {
                    data["requested"] = json!(version);
                }
                return Err(RpcError {
                    code: codes::UNSUPPORTED_PROTOCOL_VERSION,
                    message: "Unsupported protocol version".to_owned(),
                    data: Some(data),
                });
            }
            if !meta
                .get(META_CLIENT_CAPABILITIES)
                .is_some_and(Value::is_object)
            {
                return Err(invalid_params(
                    "_meta の io.modelcontextprotocol/clientCapabilities（オブジェクト）がない",
                ));
            }
            let client_name = meta
                .get(META_CLIENT_INFO)
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str);
            let actor_id = actor_id(self.actor_override.as_deref(), client_name);
            let result = match method {
                "server/discover" => Ok(with_cache_hints(self.discover())),
                "tools/list" => Ok(with_cache_hints(self.list(Era::Modern))),
                "tools/call" => call_tool(&mut self.host, Era::Modern, params, &actor_id, log),
                "ping" => Ok(json!({})),
                _ => Err(method_not_found(method)),
            }?;
            return Ok(self.modern_result(result));
        }

        // Legacy：initialize で取り決めたあと
        if let Some(negotiated) = self.negotiated.as_ref() {
            let era = Era::Legacy(negotiated.version);
            return match method {
                "tools/list" => Ok(self.list(era)),
                "tools/call" => call_tool(&mut self.host, era, params, &negotiated.actor_id, log),
                "ping" => Ok(json!({})),
                "initialize" => Err(RpcError {
                    code: codes::INVALID_REQUEST,
                    message: format!(
                        "initialize は1回だけ（取り決めた版 {} のまま）",
                        negotiated.version.as_str()
                    ),
                    data: None,
                }),
                _ => Err(method_not_found(method)),
            };
        }

        // initialize の前で、_meta に protocolVersion がない
        match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            _ => Err(invalid_params(
                "_meta の io.modelcontextprotocol/protocolVersion がない。Modern なら _meta を付け、Legacy なら先に initialize を送る",
            )),
        }
    }

    /// Legacy の initialize：版を取り決め、そのプロセスのあいだ使う。actor の id は、ここで作って整えたものだけを持つ。
    fn initialize(&mut self, params: &Map<String, Value>) -> Value {
        let version = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .and_then(LegacyVersion::parse)
            .unwrap_or(LegacyVersion::V2025_11_25);
        let client_name = params
            .get("clientInfo")
            .and_then(|info| info.get("name"))
            .and_then(Value::as_str);
        self.negotiated = Some(Negotiated {
            version,
            actor_id: actor_id(self.actor_override.as_deref(), client_name),
        });
        json!({
            "protocolVersion": version.as_str(),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": self.info.name, "version": self.info.version},
        })
    }

    /// Modern の server/discover。
    fn discover(&self) -> Value {
        json!({
            "supportedVersions": [MODERN_VERSION],
            "capabilities": {"tools": {}},
            "instructions": self.info.instructions,
        })
    }

    /// tools/list（全部を1回で返す。nextCursor は付けない）。
    fn list(&self, era: Era) -> Value {
        let tools: Vec<Value> = self
            .host
            .tools()
            .into_iter()
            .map(|tool| {
                let mut entry = json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                });
                if era.annotations() {
                    entry["annotations"] = json!({
                        "readOnlyHint": tool.read_only_hint,
                        "destructiveHint": tool.destructive_hint,
                    });
                }
                entry
            })
            .collect();
        json!({ "tools": tools })
    }

    /// Modern の結果に、resultType と serverInfo の _meta を付ける（isError:true のときも complete）。
    fn modern_result(&self, mut result: Value) -> Value {
        if let Value::Object(map) = &mut result {
            map.insert("resultType".to_owned(), json!("complete"));
            map.insert(
                "_meta".to_owned(),
                json!({ META_SERVER_INFO: {"name": self.info.name, "version": self.info.version} }),
            );
        }
        result
    }
}

/// tools/call：どの方式でも、同じ受付口（`ToolHost::call`）を通す。arguments は params から取り出して、写さずに渡す。
fn call_tool<H: ToolHost>(
    host: &mut H,
    era: Era,
    mut params: Map<String, Value>,
    actor_id: &str,
    log: &mut LogLine,
) -> Result<Value, RpcError> {
    let Some(Value::String(name)) = params.remove("name") else {
        return Err(invalid_params("name（ツールの名前）を文字列で渡す"));
    };
    log.tool = Logged::pick(&name, host.log_names());
    let arguments = match params.remove("arguments") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(arguments) => arguments,
    };
    drop(params);
    let outcome = catch_unwind(AssertUnwindSafe(|| host.call(&name, &arguments, actor_id)));
    drop(arguments);
    let (is_error, text, structured) = match outcome {
        Err(_) => {
            return Err(RpcError {
                code: codes::INTERNAL_ERROR,
                message: "内部のエラー（ツールの処理が途中で止まった）".to_owned(),
                data: None,
            });
        }
        Ok(Err(CallError::UnknownTool)) => {
            return Err(invalid_params(
                "知らないツールの名前（tools/list で確かめる）",
            ));
        }
        Ok(Ok(value)) if !value.is_object() => {
            return Err(RpcError {
                code: codes::INTERNAL_ERROR,
                message: "内部のエラー（ツールの結果がオブジェクトでない）".to_owned(),
                data: None,
            });
        }
        Ok(Ok(value)) => {
            log.result = Logged::Name(RESULT_OK);
            let text = serde_json::to_string(&value).map_err(|_| internal())?;
            (false, text, value)
        }
        Ok(Err(CallError::Tool { code, message })) => {
            log.result = Logged::pick(&code, host.log_names());
            let text = format!("{code}: {message}");
            (
                true,
                text,
                json!({"error": {"code": code, "message": message}}),
            )
        }
    };
    let mut result = json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    });
    if era.structured() {
        result["structuredContent"] = structured;
    }
    Ok(result)
}

/// Modern の server/discover と tools/list の結果に、キャッシュのヒント（`ttlMs:0`・`cacheScope:"private"`）を付ける。
/// 仕様（2026-07-28 の caching）の MUST：resultType が complete の、server/discover・tools/list などの結果に付ける。
/// 0 と private は、古い一覧を使われる心配がいちばん少ない値。Legacy の返事と tools/call の結果には付けない。
fn with_cache_hints(mut result: Value) -> Value {
    if let Value::Object(map) = &mut result {
        map.insert("ttlMs".to_owned(), json!(CACHE_TTL_MS));
        map.insert("cacheScope".to_owned(), json!(CACHE_SCOPE));
    }
    result
}

/// actor の id（`mcp:<名前>`）。名前は、上書きがあればそれ、なければクライアントの名前。整えて空なら `unknown`。
fn actor_id(actor_override: Option<&str>, client_name: Option<&str>) -> String {
    format!("mcp:{}", clean_actor_name(actor_override.or(client_name)))
}

/// actor の名前を整える：印字できる ASCII だけを残し、前後の空白を除き、64字で切る。空なら `unknown`。
pub fn clean_actor_name(name: Option<&str>) -> String {
    let kept: String = name
        .unwrap_or_default()
        .chars()
        .filter(|c| matches!(c, ' '..='~'))
        .collect();
    let trimmed: String = kept.trim().chars().take(MAX_ACTOR_NAME_CHARS).collect();
    let trimmed = trimmed.trim_end().to_owned();
    if trimmed.is_empty() {
        UNKNOWN_ACTOR_NAME.to_owned()
    } else {
        trimmed
    }
}

/// 「知らないメソッド」の返事の文に入れるメソッド名：印字できる ASCII（空白を除く）で 64 字まで。ほかの文字があれば `?`。
/// 返事は送ってきた相手にだけ返す（stdout）。ログ（stderr）には使わない（ログは `KNOWN_METHODS` で照らす）。
fn reply_name(name: &str) -> String {
    if !name.is_empty()
        && name.chars().count() <= MAX_REPLY_NAME_CHARS
        && name.chars().all(|c| c.is_ascii_graphic())
    {
        name.to_owned()
    } else {
        "?".to_owned()
    }
}

/// JSON-RPC のエラー。
#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

fn invalid_params(message: &str) -> RpcError {
    RpcError {
        code: codes::INVALID_PARAMS,
        message: message.to_owned(),
        data: None,
    }
}

fn method_not_found(method: &str) -> RpcError {
    RpcError {
        code: codes::METHOD_NOT_FOUND,
        message: format!("知らないメソッド：{}", reply_name(method)),
        data: None,
    }
}

fn internal() -> RpcError {
    RpcError {
        code: codes::INTERNAL_ERROR,
        message: "内部のエラー".to_owned(),
        data: None,
    }
}

fn error_reply(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let error = RpcError {
        code,
        message: message.to_owned(),
        data,
    };
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_names_are_cleaned() {
        assert_eq!(clean_actor_name(Some("claude-code")), "claude-code");
        assert_eq!(clean_actor_name(Some("  a b  ")), "a b");
        assert_eq!(clean_actor_name(Some("\u{7}\n\t")), "unknown");
        assert_eq!(clean_actor_name(Some("日本語")), "unknown");
        assert_eq!(clean_actor_name(Some("x日y")), "xy");
        assert_eq!(clean_actor_name(None), "unknown");
        assert_eq!(clean_actor_name(Some(&"n".repeat(100))).len(), 64);
    }

    #[test]
    fn reply_names_never_carry_odd_characters() {
        assert_eq!(reply_name("tools/call"), "tools/call");
        assert_eq!(reply_name("a\nb"), "?");
        assert_eq!(reply_name(&"m".repeat(65)), "?");
        assert_eq!(reply_name(""), "?");
    }

    /// ログのメソッド名は、扱うメソッドの一覧の名前だけ。ほかは `?`、ないときは `-`。
    #[test]
    fn log_lines_carry_only_known_method_names() {
        for method in [
            "initialize",
            "ping",
            "server/discover",
            "tools/list",
            "tools/call",
            "notifications/initialized",
            "notifications/cancelled",
        ] {
            let entry = LogLine {
                method: Logged::pick(method, KNOWN_METHODS),
                ..LogLine::default()
            };
            assert_eq!(
                entry.to_line().to_string(),
                format!("mcp: method={method} tool=- result=- ms=0")
            );
        }
        for method in [
            r"C:\Profiles\SECRET-USER-q7\a.txt",
            "192.168.77.88",
            "home/alice",
            "resources/list",
            "TOOLS/CALL",
            "",
        ] {
            let entry = LogLine {
                method: Logged::pick(method, KNOWN_METHODS),
                tool: Logged::Unknown,
                result: Logged::Code(codes::METHOD_NOT_FOUND),
                elapsed: Duration::from_millis(3),
            };
            assert_eq!(
                entry.to_line().to_string(),
                "mcp: method=? tool=? result=-32601 ms=3"
            );
        }
    }

    /// 上限は json-guard と同じ値（数え方のテストは json-guard にある）。
    #[test]
    fn element_limit_is_shared_with_json_guard() {
        assert_eq!(MAX_JSON_ELEMENTS, 1_000_000);
        assert_eq!(MAX_JSON_ELEMENTS, json_guard::MAX_ELEMENTS);
    }

    #[test]
    fn legacy_versions_parse_in_order() {
        for text in ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"] {
            assert_eq!(LegacyVersion::parse(text).unwrap().as_str(), text);
        }
        assert!(LegacyVersion::V2024_11_05 < LegacyVersion::V2025_11_25);
        assert_eq!(LegacyVersion::parse(MODERN_VERSION), None);
        assert_eq!(DEFAULT_LEGACY_VERSION, LegacyVersion::V2025_11_25.as_str());
    }
}
