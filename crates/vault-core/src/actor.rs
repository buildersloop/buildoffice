//! 誰が変えたか（actor）。
//!
//! - FFI からの操作：`{kind:"user", id:"app"}`
//! - `vault-mcp init`：`{kind:"user", id:"cli"}`
//! - MCP からの操作：`{kind:"ai", id:"mcp:<名前>"}`（kind は通り道で決め、クライアントの値からは決めない）
//! - 走査で見つけた変更：`{kind:"external", id:"fs"}`
//!
//! どれにも、プロセスごとの `session` を付ける。id は、コアでも整える（印字できる ASCII だけ、64字まで、
//! 空なら `unknown`）。MCP の層でも整えるが、コアに来る値を信用しない。

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::time;

/// actor の id の長さの上限（字）。
pub const MAX_ACTOR_ID_CHARS: usize = 64;
/// 整えたあとで空になった id の代わり。
pub const UNKNOWN_ACTOR_ID: &str = "unknown";

/// 変えた者の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    /// 人（アプリ・コマンドライン）。
    User,
    /// AI（MCP）。
    Ai,
    /// 保管庫の外の処理（走査で見つけた変更）。
    External,
}

/// 誰が変えたか。履歴とゴミ箱の entry に残す。作るのは `Actor::new` などからだけ（id を整えるため）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    kind: ActorKind,
    id: String,
    session: String,
}

impl Actor {
    /// このプロセスの session を付けて作る。id は整える（印字できる ASCII だけ、64字まで、空なら `unknown`）。
    pub fn new(kind: ActorKind, id: &str) -> Self {
        Self {
            kind,
            id: sanitize_id(id),
            session: process_session().to_owned(),
        }
    }

    /// 人（`kind:"user"`）。
    pub fn user(id: &str) -> Self {
        Self::new(ActorKind::User, id)
    }

    /// AI（`kind:"ai"`）。
    pub fn ai(id: &str) -> Self {
        Self::new(ActorKind::Ai, id)
    }

    pub fn kind(&self) -> ActorKind {
        self.kind
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn session(&self) -> &str {
        &self.session
    }
}

/// 印字できる ASCII（空白から `~` まで）だけを残し、前後の空白を除いて、64字で切る。空なら `unknown`。
fn sanitize_id(id: &str) -> String {
    let printable: String = id.chars().filter(|c| (' '..='~').contains(c)).collect();
    let trimmed: String = printable.trim().chars().take(MAX_ACTOR_ID_CHARS).collect();
    let trimmed = trimmed.trim_end();
    if trimmed.is_empty() {
        UNKNOWN_ACTOR_ID.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// プロセスごとの session（`<起動したときの unix_ms>-<pid>`）。
pub fn process_session() -> &'static str {
    static SESSION: OnceLock<String> = OnceLock::new();
    SESSION.get_or_init(|| format!("{}-{}", time::now_unix_millis(), std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_form_matches_the_adr() {
        let a = Actor::ai("mcp:claude");
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["kind"], "ai");
        assert_eq!(json["id"], "mcp:claude");
        assert_eq!(json["session"], process_session());
        assert_eq!(
            serde_json::to_value(Actor::user("app")).unwrap()["kind"],
            "user"
        );
        assert_eq!(
            serde_json::to_value(Actor::new(ActorKind::External, "fs")).unwrap()["kind"],
            "external"
        );
    }

    #[test]
    fn the_session_is_the_same_within_a_process() {
        assert_eq!(Actor::user("a").session(), Actor::ai("b").session());
        let (ms, pid) = process_session().split_once('-').unwrap();
        assert!(ms.parse::<i64>().is_ok());
        assert_eq!(pid, std::process::id().to_string());
    }

    /// コアでも id を整える。
    #[test]
    fn ids_are_sanitized_in_the_core() {
        let cases = [
            ("mcp:Claude Code", "mcp:Claude Code"),
            ("日本語\u{202E}abc\n", "abc"),
            ("a\u{0}b\u{7F}c", "abc"),
            ("  spaced  ", "spaced"),
            ("", "unknown"),
            ("\u{0}\n\t", "unknown"),
            ("   ", "unknown"),
            ("ＡＢＣ", "unknown"),
        ];
        for (input, expected) in cases {
            assert_eq!(Actor::ai(input).id(), expected, "{input:?}");
        }
        let long = "x".repeat(100);
        assert_eq!(Actor::user(&long).id().len(), MAX_ACTOR_ID_CHARS);
        assert_eq!(Actor::user(&long).kind(), ActorKind::User);
    }
}
