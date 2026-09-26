//! LLM の呼び出し口。今の実装は Claude Code の headless モード（`claude -p`）だけで、
//! サブスクリプションの枠内で動かす。将来 API などを足せるよう `Llm` トレイトで抽象化する。

pub mod claude_cli;

use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct LlmRequest<'a> {
    pub system: &'a str,
    pub prompt: &'a str,
    /// 出力の JSON Schema。応答はこれに従った `output` になる。
    pub schema: &'a serde_json::Value,
    pub model: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmResponse {
    pub output: serde_json::Value,
    pub rate_limit: Option<RateLimit>,
}

/// サブスクリプションの使用率（0〜1、超えることもある）とリセット時刻（UNIX 秒）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, serde::Deserialize)]
pub struct Window {
    pub utilization: f64,
    pub resets_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, serde::Deserialize)]
pub struct RateLimit {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("failed to run {command}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("failed to talk to the llm process")]
    Io(#[source] std::io::Error),
    #[error("llm call timed out after {secs}s")]
    Timeout { secs: u64 },
    #[error("llm process exited with {status}: {stderr}")]
    Exit { status: String, stderr: String },
    #[error("unexpected llm output: {0}")]
    Protocol(String),
    #[error("usage limit reached (resets at {resets_at:?})")]
    RateLimited { resets_at: Option<i64> },
    #[error("llm reported an error ({subtype}): {message}")]
    Reported { subtype: String, message: String },
    #[error("llm returned no structured output")]
    NoStructuredOutput,
}

/// LLM のバックエンド。
pub trait Llm {
    /// `llm_calls` などに記録する名前（例 "claude-cli"）
    fn backend(&self) -> &'static str;

    fn call(
        &self,
        req: LlmRequest<'_>,
    ) -> impl std::future::Future<Output = Result<LlmResponse, LlmError>>;
}
