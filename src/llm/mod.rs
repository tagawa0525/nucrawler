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
    Exit {
        status: String,
        stderr: String,
        /// SIGINT・SIGTERM で終わった（シグナルによる終了か、終了コード 130・143）
        interrupted: bool,
    },
    #[error("unexpected llm output: {0}")]
    Protocol(String),
    #[error("usage limit reached (resets at {resets_at:?})")]
    RateLimited {
        resets_at: Option<i64>,
        /// 拒否されたときの使用率（次回の判定に使う）
        rate_limit: Option<RateLimit>,
    },
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

/// テスト用の偽のバックエンド。用意した応答を順に返し、受け取った依頼を記録する。
#[cfg(test)]
pub mod fake {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::{Llm, LlmError, LlmRequest, LlmResponse};

    #[derive(Debug, Clone, PartialEq)]
    pub struct Recorded {
        pub system: String,
        pub prompt: String,
        pub schema: serde_json::Value,
        pub model: String,
    }

    #[derive(Default)]
    pub struct FakeLlm {
        responses: Mutex<VecDeque<Result<LlmResponse, LlmError>>>,
        requests: Mutex<Vec<Recorded>>,
    }

    impl FakeLlm {
        pub fn new(responses: impl IntoIterator<Item = Result<LlmResponse, LlmError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                requests: Mutex::default(),
            }
        }

        pub fn requests(&self) -> Vec<Recorded> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Llm for FakeLlm {
        fn backend(&self) -> &'static str {
            "fake"
        }

        async fn call(&self, req: LlmRequest<'_>) -> Result<LlmResponse, LlmError> {
            self.requests.lock().unwrap().push(Recorded {
                system: req.system.into(),
                prompt: req.prompt.into(),
                schema: req.schema.clone(),
                model: req.model.into(),
            });
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("FakeLlm ran out of prepared responses")
        }
    }
}
