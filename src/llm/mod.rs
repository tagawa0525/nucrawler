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
    pub usage: Option<Usage>,
}

/// 呼び出しで分かった使用量。バックエンドによって分かるものが違う。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Usage {
    /// サブスクリプションの枠の使用率（claude-cli）
    Subscription(RateLimit),
}

impl Usage {
    /// サブスクリプションの枠の使用率（それ以外の使用量なら `None`）
    pub fn rate_limit(&self) -> Option<RateLimit> {
        match self {
            Usage::Subscription(r) => Some(*r),
        }
    }
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

impl RateLimit {
    /// 2 つの観測を合わせる。並行した呼び出しの結果は順が前後するので、枠ごとに、リセット時刻が
    /// 新しい方を使い、同じ枠なら使用率の高い方を使う（同じ枠の中で使用率は下がらない）。
    /// どちらかに無い枠は、ある方を使う。順によらず同じ結果になる。
    pub fn merge(self, other: RateLimit) -> RateLimit {
        RateLimit {
            five_hour: Window::merge(self.five_hour, other.five_hour),
            seven_day: Window::merge(self.seven_day, other.seven_day),
        }
    }
}

impl Window {
    fn merge(a: Option<Window>, b: Option<Window>) -> Option<Window> {
        match (a, b) {
            (Some(a), Some(b)) if a.resets_at != b.resets_at => {
                Some(if a.resets_at > b.resets_at { a } else { b })
            }
            (Some(a), Some(b)) => Some(if a.utilization >= b.utilization { a } else { b }),
            (a, b) => a.or(b),
        }
    }
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
    #[error("failed to take a call slot")]
    Slot(#[source] crate::pipeline::lock::LockError),
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
    /// 呼び出しの枠。持っている間だけ呼び出してよい（drop すると空く）
    type Slot;

    /// `llm_calls` などに記録する名前（例 "claude-cli"）
    fn backend(&self) -> &'static str;

    /// 呼び出しの枠を取る。埋まっていれば空くまで待つ。枠はクォータの判定と作業の予約の前に取り、
    /// 呼び出しを終えるまで持つ（枠を待つ間に使用率が上がっても、判定し直してから呼ぶように）。
    fn reserve(&self) -> impl std::future::Future<Output = Result<Self::Slot, LlmError>>;

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

    /// 呼ばれたとき（応答を返す前）に、何回目の呼び出しか（0 から）を渡して実行する処理。
    /// 呼び出しの最中にほかの実行が DB を書き換える状況を作るのに使う。
    type Hook = Box<dyn FnMut(usize) + Send>;

    /// 依頼から応答を作る処理（応答を順に用意せず、依頼の内容に応じて返すとき）。
    type Responder = Box<dyn Fn(&LlmRequest<'_>) -> Result<LlmResponse, LlmError> + Send + Sync>;

    #[derive(Default)]
    pub struct FakeLlm {
        responses: Mutex<VecDeque<Result<LlmResponse, LlmError>>>,
        requests: Mutex<Vec<Recorded>>,
        hook: Mutex<Option<Hook>>,
        reserve_hook: Mutex<Option<Hook>>,
        reserved: Mutex<usize>,
        responder: Option<Responder>,
        delay: std::time::Duration,
        /// いま応答を待っている呼び出しの数と、その最大
        in_flight: Mutex<(usize, usize)>,
    }

    impl FakeLlm {
        pub fn new(responses: impl IntoIterator<Item = Result<LlmResponse, LlmError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                requests: Mutex::default(),
                hook: Mutex::default(),
                reserve_hook: Mutex::default(),
                reserved: Mutex::default(),
                responder: None,
                delay: std::time::Duration::ZERO,
                in_flight: Mutex::default(),
            }
        }

        pub fn with_hook(
            responses: impl IntoIterator<Item = Result<LlmResponse, LlmError>>,
            hook: impl FnMut(usize) + Send + 'static,
        ) -> Self {
            Self {
                hook: Mutex::new(Some(Box::new(hook))),
                ..Self::new(responses)
            }
        }

        /// 呼び出しの枠を取るたびに、何回目か（0 から）を渡して `hook` を実行する。枠を待つ間に
        /// ほかの実行が DB を書き換える状況を作るのに使う。
        pub fn with_reserve_hook(
            responses: impl IntoIterator<Item = Result<LlmResponse, LlmError>>,
            hook: impl FnMut(usize) + Send + 'static,
        ) -> Self {
            Self {
                reserve_hook: Mutex::new(Some(Box::new(hook))),
                ..Self::new(responses)
            }
        }

        /// 依頼ごとに `respond` で応答を作り、`delay` だけ待ってから返す。同時に何本呼ばれたかを
        /// 確かめるのに使う（`max_in_flight`）。
        pub fn responding(
            delay: std::time::Duration,
            respond: impl Fn(&LlmRequest<'_>) -> Result<LlmResponse, LlmError> + Send + Sync + 'static,
        ) -> Self {
            Self {
                responder: Some(Box::new(respond)),
                delay,
                ..Self::new([])
            }
        }

        /// 同時に応答を待った呼び出しの数の最大
        pub fn max_in_flight(&self) -> usize {
            self.in_flight.lock().unwrap().1
        }

        pub fn requests(&self) -> Vec<Recorded> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Llm for FakeLlm {
        type Slot = ();

        fn backend(&self) -> &'static str {
            "fake"
        }

        async fn reserve(&self) -> Result<(), LlmError> {
            let n = {
                let mut reserved = self.reserved.lock().unwrap();
                *reserved += 1;
                *reserved - 1
            };
            if let Some(hook) = self.reserve_hook.lock().unwrap().as_mut() {
                hook(n);
            }
            Ok(())
        }

        async fn call(&self, req: LlmRequest<'_>) -> Result<LlmResponse, LlmError> {
            let n = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(Recorded {
                    system: req.system.into(),
                    prompt: req.prompt.into(),
                    schema: req.schema.clone(),
                    model: req.model.into(),
                });
                requests.len() - 1
            };
            if let Some(hook) = self.hook.lock().unwrap().as_mut() {
                hook(n);
            }
            if !self.delay.is_zero() {
                /// 応答を待つ間だけ数える。呼び出しを途中で捨てられても（止める指示）、drop で戻す
                struct InFlight<'a>(&'a Mutex<(usize, usize)>);
                impl Drop for InFlight<'_> {
                    fn drop(&mut self) {
                        self.0.lock().unwrap().0 -= 1;
                    }
                }
                {
                    let mut in_flight = self.in_flight.lock().unwrap();
                    in_flight.0 += 1;
                    in_flight.1 = in_flight.1.max(in_flight.0);
                }
                let _in_flight = InFlight(&self.in_flight);
                tokio::time::sleep(self.delay).await;
            }
            if let Some(respond) = &self.responder {
                return respond(&req);
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("FakeLlm ran out of prepared responses")
        }
    }
}
