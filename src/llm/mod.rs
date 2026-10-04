//! LLM の呼び出し口。Claude Code の headless モード（`claude -p`、サブスクリプションの枠）と
//! GitHub Copilot CLI（`copilot`、AI Credits の月の予算）を `Llm` トレイトで抽象化し、設定（既定の
//! `llm.backend` と、工程ごとに上書きする `llm.*_backend`）で選ぶ（`Backends`）。

pub mod claude_cli;
pub mod copilot_cli;
mod process;
pub mod slot;

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
    /// 消費した AI Credits（copilot-cli）。10^-9 クレジット単位
    Credits { nano_aiu: i64 },
}

impl Usage {
    /// サブスクリプションの枠の使用率（それ以外の使用量なら `None`）
    pub fn rate_limit(&self) -> Option<RateLimit> {
        match self {
            Usage::Subscription(r) => Some(*r),
            Usage::Credits { .. } => None,
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
    Slot(#[source] crate::filelock::LockError),
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
    RateLimited { resets_at: Option<i64> },
    #[error("llm reported an error ({subtype}): {message}")]
    Reported { subtype: String, message: String },
    #[error("llm returned no structured output")]
    NoStructuredOutput,
}

/// 工程ごとの LLM。工程によってバックエンドを変えられる。
pub trait LlmSet {
    type Llm: Llm;

    fn for_task(&self, task: crate::config::LlmTask) -> &Self::Llm;
}

/// 1 つの LLM は、どの工程にも自分を使う。
impl<L: Llm> LlmSet for L {
    type Llm = L;

    fn for_task(&self, _: crate::config::LlmTask) -> &L {
        self
    }
}

/// 設定の工程ごとのバックエンド（`llm.backend`・`llm.*_backend`）。claude と copilot の両方を持ち、
/// 工程に応じて返す（使わない方は呼ばないので、作るだけなら費用は無い）。
pub struct Backends {
    claude: Backend,
    copilot: Backend,
    config: crate::config::LlmConfig,
}

impl Backends {
    /// 作業ディレクトリは `llm-cwd`、呼び出しの枠は `data` に置き、バックエンドによらず同じ場所で数える。
    pub fn from_config(c: &crate::config::LlmConfig, data: &std::path::Path) -> Self {
        let cwd = data.join("llm-cwd");
        let slots = data.to_path_buf();
        Self {
            claude: Backend::Claude(claude_cli::ClaudeCli::from_config(
                c,
                cwd.clone(),
                slots.clone(),
            )),
            copilot: Backend::Copilot(copilot_cli::CopilotCli::from_config(
                c,
                cwd,
                data.join("copilot-home"),
                slots,
            )),
            config: c.clone(),
        }
    }
}

impl LlmSet for Backends {
    type Llm = Backend;

    fn for_task(&self, task: crate::config::LlmTask) -> &Backend {
        match self.config.backend_for(task) {
            crate::config::LlmBackend::ClaudeCli => &self.claude,
            crate::config::LlmBackend::CopilotCli => &self.copilot,
        }
    }
}

/// claude か copilot のバックエンド。
pub enum Backend {
    Claude(claude_cli::ClaudeCli),
    Copilot(copilot_cli::CopilotCli),
}

impl Llm for Backend {
    type Slot = slot::Slot;

    fn backend(&self) -> &'static str {
        match self {
            Self::Claude(c) => c.backend(),
            Self::Copilot(c) => c.backend(),
        }
    }

    async fn reserve(&self) -> Result<Self::Slot, LlmError> {
        match self {
            Self::Claude(c) => c.reserve().await,
            Self::Copilot(c) => c.reserve().await,
        }
    }

    async fn call(&self, req: LlmRequest<'_>) -> Result<LlmResponse, LlmFailure> {
        match self {
            Self::Claude(c) => c.call(req).await,
            Self::Copilot(c) => c.call(req).await,
        }
    }
}

/// 失敗した呼び出し。失敗しても、それまでに分かった使用量を運ぶ（消費した分をクォータに数えるため）。
#[derive(Debug)]
pub struct LlmFailure {
    pub error: LlmError,
    pub usage: Option<Usage>,
}

impl From<LlmError> for LlmFailure {
    fn from(error: LlmError) -> Self {
        Self { error, usage: None }
    }
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
    ) -> impl std::future::Future<Output = Result<LlmResponse, LlmFailure>>;
}

/// テスト用の偽のバックエンド。用意した応答を順に返し、受け取った依頼を記録する。
#[cfg(test)]
pub mod fake {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::{Llm, LlmError, LlmFailure, LlmRequest, LlmResponse};

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
        responses: Mutex<VecDeque<Result<LlmResponse, LlmFailure>>>,
        requests: Mutex<Vec<Recorded>>,
        hook: Mutex<Option<Hook>>,
        reserve_hook: Mutex<Option<Hook>>,
        reserved: Mutex<usize>,
        responder: Option<Responder>,
        delay: std::time::Duration,
        /// いま応答を待っている呼び出しの数と、その最大
        in_flight: Mutex<(usize, usize)>,
        /// `backend()` の名前（省けば "fake"）。工程ごとにバックエンドを変える場合を作るのに使う
        name: Option<&'static str>,
    }

    impl FakeLlm {
        pub fn new(responses: impl IntoIterator<Item = Result<LlmResponse, LlmError>>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|r| r.map_err(LlmFailure::from))
                        .collect(),
                ),
                requests: Mutex::default(),
                hook: Mutex::default(),
                reserve_hook: Mutex::default(),
                reserved: Mutex::default(),
                responder: None,
                delay: std::time::Duration::ZERO,
                in_flight: Mutex::default(),
                name: None,
            }
        }

        /// 使用量の分かった失敗を順に返す（失敗しても消費した分を記録するかを確かめるのに使う）。
        pub fn failing(failures: impl IntoIterator<Item = LlmFailure>) -> Self {
            Self {
                responses: Mutex::new(failures.into_iter().map(Err).collect()),
                ..Self::new([])
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

        /// 呼ばれたとき（応答を返す前）に、何回目の呼び出しか（0 から）を渡して `hook` を実行する。
        /// `responding` と組み合わせて、応答を依頼から作りつつ途中で止める指示を出すのに使う。
        pub fn hooked(self, hook: impl FnMut(usize) + Send + 'static) -> Self {
            Self {
                hook: Mutex::new(Some(Box::new(hook))),
                ..self
            }
        }

        /// `backend()` の名前を変える。
        pub fn named(self, name: &'static str) -> Self {
            Self {
                name: Some(name),
                ..self
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
            self.name.unwrap_or("fake")
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

        async fn call(&self, req: LlmRequest<'_>) -> Result<LlmResponse, LlmFailure> {
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
                return respond(&req).map_err(Into::into);
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("FakeLlm ran out of prepared responses")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LlmBackend, LlmConfig, LlmTask};

    #[test]
    fn builds_backends_per_task() {
        let data = std::path::Path::new("/data");
        let backends = Backends::from_config(
            &LlmConfig {
                backend: LlmBackend::CopilotCli,
                score_backend: Some(LlmBackend::ClaudeCli),
                command: Some("/opt/claude".into()),
                ..LlmConfig::default()
            },
            data,
        );
        let Backend::Claude(c) = backends.for_task(LlmTask::Score) else {
            panic!("score runs on claude");
        };
        assert_eq!(c.command, std::path::PathBuf::from("/opt/claude"));
        assert_eq!(c.cwd, data.join("llm-cwd"));
        assert_eq!(c.slots, data);
        let Backend::Copilot(c) = backends.for_task(LlmTask::Digest) else {
            panic!("digest runs on copilot");
        };
        assert_eq!(c.command, std::path::PathBuf::from("copilot"));
        assert_eq!(c.cwd, data.join("llm-cwd"));
        assert_eq!(c.homes, data.join("copilot-home"));
        // 呼び出しの枠はバックエンドによらず同じ場所で数える
        assert_eq!(c.slots, data);
        assert_eq!(c.timeout, std::time::Duration::from_secs(300));
        assert_eq!(
            backends.for_task(LlmTask::Translate).backend(),
            "copilot-cli"
        );
        assert_eq!(backends.for_task(LlmTask::Score).backend(), "claude-cli");
    }
}
