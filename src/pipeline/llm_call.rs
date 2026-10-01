//! LLM ステージ共通の 1 回の呼び出し：呼んで、`llm_calls` に記録し、使用率をクォータに反映し、
//! 失敗を「止める理由」に振り分ける。記事ごとの失敗の記録もここにまとめる。

use std::cell::{Cell, RefCell};

use chrono::{DateTime, Utc};

use super::{Cancel, Halt};
use crate::db::{Db, DbError, LlmCall, StageKey};
use crate::errors;
use crate::llm::{Llm, LlmError, LlmFailure, LlmRequest, LlmResponse};
use crate::quota::Quota;

pub enum Outcome {
    Response(LlmResponse),
    /// `UsageLimit` は記事の問題ではないので、呼び出し側は記事の失敗として記録しない。
    /// `LlmFailed` は認証切れなど記事によらない原因かもしれないので、呼び出し側は
    /// そのバッチだけ失敗にしてステージを止める。
    Halted(Halt),
    /// 止める指示で呼び出しをやめた。記事の失敗にも LLM の失敗にも数えず、次回続きから処理する
    Cancelled,
    /// 止める指示が先に出ていたので呼ばなかった（呼び出しにも数えない）
    NotStarted,
}

/// LLM ステージの集計のうち、どのステージにもある項目。
#[derive(Debug, Default, PartialEq)]
pub struct Tally {
    /// 失敗を記録した（再試行に回した）記事の数
    pub failed: usize,
    pub calls: usize,
    /// 止めた理由（クォータ・LLM の失敗）
    pub halted: Option<Halt>,
    /// 止める指示で止まった
    pub cancelled: bool,
}

impl Tally {
    /// 作業者ごとの集計を合わせる。
    pub fn merge(&mut self, other: Tally) {
        self.failed += other.failed;
        self.calls += other.calls;
        self.halted = Halt::most_severe(self.halted.take(), other.halted);
        self.cancelled |= other.cancelled;
    }
}

/// LLM ステージが共有する実行環境。クォータは実行全体で 1 つなので、ステージ間で引き継ぐ。
pub struct LlmStage<'a, L> {
    pub db: &'a Db,
    pub llm: &'a L,
    pub quota: &'a mut Quota,
    pub cancel: &'a Cancel,
    /// クォータの判定・作業の予約と延長・呼び出しの記録のたびに今の時刻を読む（ステージの `now` は
    /// 開始時の時刻のまま進まないが、これらは実際の時刻で決める。テストでは固定する）
    pub clock: &'a dyn Fn() -> DateTime<Utc>,
}

/// 作業の予約の期限。呼び出しのタイムアウトの 2 倍（下限は `MIN_CLAIM_TTL`）で、それを過ぎた予約は
/// 落ちたプロセスが残したものとみなす。処理がそれより長引いても、保存の前に延長できなければ
/// 結果を捨てるので、2 つの実行が同じ記事を保存することはない。
pub fn claim_ttl(cfg: &crate::config::LlmConfig) -> chrono::Duration {
    let call = i64::try_from(cfg.timeout_secs.saturating_mul(2)).unwrap_or(i64::MAX);
    chrono::Duration::seconds(call).max(MIN_CLAIM_TTL)
}

/// 予約の期限の下限。タイムアウトを短くしても、プロンプトの組み立てや保存の分の余裕を残す。
const MIN_CLAIM_TTL: chrono::Duration = chrono::Duration::minutes(10);

/// 応答に無かった記事のうち、まだ予約を持っているもの（取り直された記事の失敗は記録しない）。
pub fn held_missing<'a>(missing: &'a [i64], held: &'a [i64]) -> impl Iterator<Item = i64> + 'a {
    missing.iter().copied().filter(|id| held.contains(id))
}

/// ステージの中で同時に回す作業者が共有する状態。作業者は同じタスクの中で動くので、`RefCell` で
/// 共有し、`await` をまたいで借りない。
struct Shared<'q> {
    quota: RefCell<&'q mut Quota>,
    stop: Cell<bool>,
}

impl<'q> Shared<'q> {
    pub fn new(quota: &'q mut Quota) -> Self {
        Self {
            quota: RefCell::new(quota),
            stop: Cell::new(false),
        }
    }

    /// ほかの作業者を止める（クォータ・LLM の失敗・中断で止まった作業者が呼ぶ）
    pub fn stop(&self) {
        self.stop.set(true);
    }

    /// ほかの作業者が止まったか。作業者は周の最初に確かめ、止まっていれば新しい作業を始めない
    pub fn stopped(&self) -> bool {
        self.stop.get()
    }
}

/// ステージの作業者が共有する実行環境と、周の進め方。各ステージは「対象を選ぶ・プロンプトを作る・応答を解釈して
/// 保存する」だけを書き、周の始めの判定（`begin_round`）・呼び出し（`call`）・結果の振り分け（`settle`）・
/// 終わり（`finish`）はここで行う。
pub struct Workers<'a, L> {
    pub db: &'a Db,
    pub llm: &'a L,
    pub cancel: &'a Cancel,
    pub clock: &'a dyn Fn() -> DateTime<Utc>,
    shared: Shared<'a>,
}

impl<'a, L: Llm> Workers<'a, L> {
    pub fn new(
        LlmStage {
            db,
            llm,
            quota,
            cancel,
            clock,
        }: LlmStage<'a, L>,
    ) -> Self {
        Self {
            db,
            llm,
            cancel,
            clock,
            shared: Shared::new(quota),
        }
    }

    /// 周の始め。ほかの作業者に譲り、止まっていないか・止める指示が無いかを見て、呼び出しの枠を取り、クォータで
    /// 判定する。呼んでよければ枠を返す（判定・作業の予約・呼び出しをその中で行い、周の終わりまで持つ）。止まる
    /// なら理由を `tally` に記録して `None`。`reserve_calls` は残す呼び出し回数（`permit` を参照）。
    pub async fn begin_round(
        &self,
        stage: &str,
        reserve_calls: u32,
        tally: &mut Tally,
    ) -> Result<Option<L::Slot>, DbError> {
        // 同時に終わったほかの作業者の結果（止める旗）が伝わってから次の周に入る
        // （作業者は決まった順で進むので、譲らないと先の作業者が次の呼び出しを始めてしまう）
        tokio::task::yield_now().await;
        if self.shared.stopped() {
            return Ok(None);
        }
        if self.cancel.is_requested() {
            tally.cancelled = true;
            return Ok(None);
        }
        let slot = match reserve(self.llm, self.cancel).await {
            Reserved::Slot(slot) => slot,
            Reserved::Cancelled => {
                tally.cancelled = true;
                return Ok(None);
            }
            Reserved::Failed(message) => {
                tally.halted = Some(Halt::LlmFailed(message));
                return Ok(None);
            }
        };
        // 枠を待つ間にほかの作業者が止まっていたら、呼ばずに止まる
        if self.shared.stopped() {
            return Ok(None);
        }
        if let Err(stop) = permit(
            self.db,
            &self.shared,
            self.llm.backend(),
            (self.clock)(),
            reserve_calls,
        )? {
            tracing::info!("{stage} stops: {stop}");
            tally.halted = Some(Halt::Quota(stop));
            return Ok(None);
        }
        Ok(Some(slot))
    }

    /// 呼び出す（`call_recorded`）。`begin_round` の判定からここまでの間に `await` を挟まないこと。
    pub async fn call(&self, call: Call<'_>) -> Result<Outcome, DbError> {
        call_recorded(
            self.db,
            self.llm,
            &self.shared,
            call,
            self.clock,
            self.cancel,
        )
        .await
    }

    /// 始めた呼び出しを数え（止める指示で終わった呼び出しも数える）、結果を振り分ける。応答なら返す。
    /// 止める指示・止める理由なら `tally` に記録して `None`（作業者は止まる）。LLM の失敗は認証切れなど記事に
    /// よらない原因かもしれないので、そのバッチの記事（`batch`。予約を持っているものだけを渡す）だけを失敗にする。
    pub fn settle<'k>(
        &self,
        outcome: Outcome,
        tally: &mut Tally,
        batch: impl IntoIterator<Item = StageKey<'k>>,
        now: DateTime<Utc>,
    ) -> Result<Option<LlmResponse>, DbError> {
        if !matches!(outcome, Outcome::NotStarted) {
            tally.calls += 1;
        }
        match outcome {
            Outcome::Response(response) => Ok(Some(response)),
            Outcome::Cancelled | Outcome::NotStarted => {
                tally.cancelled = true;
                Ok(None)
            }
            Outcome::Halted(halt) => {
                if let Halt::LlmFailed(message) = &halt {
                    tally.failed += record_failures(self.db, batch, message, now)?;
                }
                tally.halted = Some(halt);
                Ok(None)
            }
        }
    }

    /// 作業者を終える。止まった作業者は、ほかの作業者も止める（空になって終わったときは止めない）。
    pub fn finish(&self, tally: &Tally) {
        if tally.halted.is_some() || tally.cancelled {
            self.shared.stop();
        }
    }
}

/// 呼び出しの枠を取った結果。
enum Reserved<S> {
    Slot(S),
    /// 止める指示で待つのをやめた
    Cancelled,
    /// 枠を取れなかった（ロックファイルを開けないなど）
    Failed(String),
}

/// 呼び出しの枠を取る。止める指示が出れば待つのをやめる。ステージは周の最初に枠を取り、
/// クォータの判定・作業の予約・呼び出しをその中で行う。
async fn reserve<L: Llm>(llm: &L, cancel: &Cancel) -> Reserved<L::Slot> {
    if cancel.is_requested() {
        return Reserved::Cancelled;
    }
    tokio::select! {
        biased;
        slot = llm.reserve() => match slot {
            Ok(slot) => Reserved::Slot(slot),
            Err(e) => Reserved::Failed(errors::error_chain(&e)),
        },
        () = cancel.requested() => Reserved::Cancelled,
    }
}

/// 次の呼び出しをしてよいか。LLM を呼ぶ実行は並行して動くので、判定の前に DB の最新の使用率を
/// 読み、ほかの実行の呼び出しも判定に入れる。`reserve` は残す呼び出し回数（`permit_reserving`）。
/// `now` は判定する時点の時刻（`LlmStage::clock`）。ステージを始めた時刻を使うと、枠を待つ間や
/// 長いステージの途中で時間帯が変わっても、前の時間帯の上限で判定してしまう。
fn permit(
    db: &Db,
    shared: &Shared<'_>,
    backend: &str,
    now: DateTime<Utc>,
    reserve: u32,
) -> Result<Result<(), crate::quota::Stop>, DbError> {
    let mut quota = shared.quota.borrow_mut();
    if quota.credits_backend() == Some(backend) {
        quota.observe_credits(db.credits_since(backend, crate::quota::month_start(now))?);
    } else {
        quota.observe(db.latest_rate_limit(now)?);
    }
    Ok(quota.permit_reserving(backend, now, reserve))
}

/// 依頼したのに応答に無かった、またはスキーマに合わなかった記事の失敗の理由。
pub const MISSING: &str = "missing or invalid in the llm output";

/// 各記事の失敗を記録し、記録した件数を返す（記事は再試行に回る）。
pub fn record_failures<'k>(
    db: &Db,
    keys: impl IntoIterator<Item = StageKey<'k>>,
    message: &str,
    now: DateTime<Utc>,
) -> Result<usize, DbError> {
    let mut n = 0;
    for key in keys {
        db.record_stage_failure(key, message, now, false)?;
        n += 1;
    }
    Ok(n)
}

/// 呼び出しが失敗したとき、それが止める指示によるものかを見極めるために待つ時間。
const STOP_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// 1 回の呼び出しの内容。
pub struct Call<'a> {
    /// `llm_calls` に記録するステージ名
    pub stage: &'a str,
    /// この呼び出しでまとめて処理する記事数
    pub n_items: usize,
    pub req: LlmRequest<'a>,
}

/// 判定（`permit`）から呼び出しを始めるまでの間に `await` を挟まないこと。呼び出しは始めた時点で
/// 数えるので、その間に並行する作業者が判定すると、上限を超えて呼んでしまう。
async fn call_recorded<L: Llm>(
    db: &Db,
    llm: &L,
    shared: &Shared<'_>,
    Call {
        stage,
        n_items,
        req,
    }: Call<'_>,
    clock: &dyn Fn() -> DateTime<Utc>,
    cancel: &Cancel,
) -> Result<Outcome, DbError> {
    // 既に止める指示が出ていれば呼ばない（応答を優先する下の select は、先に呼び出しを始めてしまう）
    if cancel.is_requested() {
        return Ok(Outcome::NotStarted);
    }
    shared.quota.borrow_mut().start_call();
    // 記録する時刻は呼び出しを始めた時刻（ステージを始めた時刻では、長いステージの呼び出しがすべて
    // 同じ時刻になり、呼び出しの時系列を組み立てられない）
    let at = clock();
    let started = std::time::Instant::now();
    // 応答を待たずに止める。呼び出しの future を捨てると子プロセスも止まる（kill_on_drop）
    let result = tokio::select! {
        // 応答と止める指示が同時に届いたら、応答を捨てずに使う
        biased;
        result = llm.call(req) => result,
        () = cancel.requested() => return Ok(Outcome::Cancelled),
    };
    // 止める指示と同時に claude が終了させられたとき（端末の Ctrl-C は claude にも届く）は、
    // LLM の失敗ではない。シグナルの受け取りは非同期で、claude が落ちたことの方が先に分かることが
    // あるので、claude が SIGINT・SIGTERM で終わったときだけ止める指示を少し待つ。利用上限などの
    // ほかの失敗は、止める指示と重なってもそのまま記録する
    if matches!(
        result,
        Err(LlmFailure {
            error: LlmError::Exit {
                interrupted: true,
                ..
            },
            ..
        })
    ) && tokio::time::timeout(STOP_GRACE, cancel.requested())
        .await
        .is_ok()
    {
        return Ok(Outcome::Cancelled);
    }
    // 失敗しても、それまでに分かった使用量（上限で拒否されたときの使用率、消費したクレジット）を
    // 残して、次回の判定に使う。
    let usage = match &result {
        Ok(response) => response.usage,
        Err(failure) => failure.usage,
    };
    shared
        .quota
        .borrow_mut()
        .observe(usage.and_then(|u| u.rate_limit()));
    let error = result.as_ref().err().map(|f| errors::error_chain(&f.error));
    db.record_llm_call(
        &LlmCall {
            stage,
            backend: llm.backend(),
            model: req.model,
            n_items,
            ok: result.is_ok(),
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            error: error.as_deref(),
            usage: usage.as_ref(),
        },
        at,
    )?;
    Ok(match result {
        Ok(response) => Outcome::Response(response),
        Err(LlmFailure {
            error: LlmError::RateLimited { resets_at },
            ..
        }) => Outcome::Halted(Halt::UsageLimit { resets_at }),
        Err(_) => Outcome::Halted(Halt::LlmFailed(error.unwrap_or_default())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::fake::FakeLlm;
    use crate::quota::QuotaConfig;

    /// 端末の Ctrl-C は claude にも届くので、claude が落ちたことの方が、nucrawler が止める指示を
    /// 受け取るより先に分かることがある。その場合も失敗として記録しない。
    #[tokio::test]
    async fn failure_just_before_the_stop_arrives_is_not_recorded() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([Err(crate::llm::LlmError::Exit {
            status: "signal: 2 (SIGINT)".into(),
            stderr: String::new(),
            interrupted: true,
        })]);
        let cancel = Cancel::default();
        let requester = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            requester.request();
        });
        let schema = serde_json::json!({});
        let outcome = call_recorded(
            &db,
            &llm,
            &Shared::new(&mut Quota::new(QuotaConfig::default(), None, None)),
            Call {
                stage: "digest",
                n_items: 1,
                req: LlmRequest {
                    system: "s",
                    prompt: "p",
                    schema: &schema,
                    model: "m",
                },
            },
            &Utc::now,
            &cancel,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Cancelled));
        assert_eq!(db.query_i64("SELECT count(*) FROM llm_calls").unwrap(), 0);
    }

    /// 止める指示で終わった呼び出しも、始めたので数える。どのステージ（1 回だけ呼ぶ `profile suggest`・
    /// 語彙の整理を含む）も `Workers::settle` で数える。
    #[tokio::test]
    async fn a_call_ended_by_the_stop_is_counted() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([Err(crate::llm::LlmError::Exit {
            status: "signal: 2 (SIGINT)".into(),
            stderr: String::new(),
            interrupted: true,
        })]);
        let cancel = Cancel::default();
        let requester = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            requester.request();
        });
        let mut quota = Quota::new(QuotaConfig::default(), None, None);
        let workers = Workers::new(LlmStage {
            db: &db,
            llm: &llm,
            quota: &mut quota,
            cancel: &cancel,
            clock: &Utc::now,
        });
        let schema = serde_json::json!({});
        let outcome = workers
            .call(Call {
                stage: "suggest",
                n_items: 1,
                req: LlmRequest {
                    system: "s",
                    prompt: "p",
                    schema: &schema,
                    model: "m",
                },
            })
            .await
            .unwrap();
        let mut tally = Tally::default();
        let response = workers
            .settle(outcome, &mut tally, std::iter::empty(), Utc::now())
            .unwrap();
        assert!(response.is_none());
        assert_eq!(
            tally,
            Tally {
                calls: 1,
                cancelled: true,
                ..Tally::default()
            }
        );
    }

    /// 呼び出しを始める前に止める指示が出ていれば、呼ばないので数えない。
    #[tokio::test]
    async fn a_call_not_started_for_the_stop_is_not_counted() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([]);
        let cancel = Cancel::default();
        cancel.request();
        let mut quota = Quota::new(QuotaConfig::default(), None, None);
        let workers = Workers::new(LlmStage {
            db: &db,
            llm: &llm,
            quota: &mut quota,
            cancel: &cancel,
            clock: &Utc::now,
        });
        let schema = serde_json::json!({});
        let outcome = workers
            .call(Call {
                stage: "suggest",
                n_items: 1,
                req: LlmRequest {
                    system: "s",
                    prompt: "p",
                    schema: &schema,
                    model: "m",
                },
            })
            .await
            .unwrap();
        let mut tally = Tally::default();
        let response = workers
            .settle(outcome, &mut tally, std::iter::empty(), Utc::now())
            .unwrap();
        assert!(response.is_none());
        assert!(llm.requests().is_empty());
        assert_eq!(
            tally,
            Tally {
                cancelled: true,
                ..Tally::default()
            }
        );
    }

    /// クレジットで判定するときは、DB にある今月の消費（ほかの実行の分も含む）で判定する。
    #[test]
    fn permit_counts_this_months_credits() {
        const NANO: i64 = 1_000_000_000;
        let db = Db::open_in_memory().unwrap();
        let record = |nano_aiu: i64, at: &str| {
            db.record_llm_call(
                &LlmCall {
                    stage: "title",
                    backend: "copilot-cli",
                    model: "gpt-6-luna",
                    n_items: 1,
                    ok: true,
                    duration_ms: 1,
                    error: None,
                    usage: Some(&crate::llm::Usage::Credits { nano_aiu }),
                },
                DateTime::parse_from_rfc3339(at).unwrap().to_utc(),
            )
            .unwrap();
        };
        // 先月の分は数えない
        record(10_000 * NANO, "2026-08-31T00:00:00Z");
        record(399 * NANO, "2026-09-10T00:00:00Z");
        let now = DateTime::parse_from_rfc3339("2026-09-16T00:00:00Z")
            .unwrap()
            .to_utc();
        let credits = crate::quota::CreditsConfig {
            monthly_credits: 1000.0,
            pace: 0.8,
        };
        let mut quota = Quota::with_credits(QuotaConfig::default(), credits, "copilot-cli", None);
        let shared = Shared::new(&mut quota);
        assert_eq!(permit(&db, &shared, "copilot-cli", now, 0).unwrap(), Ok(()));
        record(NANO, "2026-09-15T00:00:00Z");
        assert!(matches!(
            permit(&db, &shared, "copilot-cli", now, 0).unwrap(),
            Err(crate::quota::Stop::MonthlyCredits { .. })
        ));
    }

    /// 応答の形が崩れて失敗しても、消費した分は記録する（月の消費を少なく数えないように）。
    #[tokio::test]
    async fn failed_calls_keep_the_consumed_credits() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::failing([LlmFailure {
            error: LlmError::Protocol("not json".into()),
            usage: Some(crate::llm::Usage::Credits {
                nano_aiu: 37_405_000,
            }),
        }]);
        let schema = serde_json::json!({});
        let outcome = call_recorded(
            &db,
            &llm,
            &Shared::new(&mut Quota::new(QuotaConfig::default(), None, None)),
            Call {
                stage: "title",
                n_items: 1,
                req: LlmRequest {
                    system: "s",
                    prompt: "p",
                    schema: &schema,
                    model: "m",
                },
            },
            &Utc::now,
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Halted(Halt::LlmFailed(_))));
        assert_eq!(
            db.query_strings("SELECT ok || '|' || coalesce(credits_nano, '-') FROM llm_calls")
                .unwrap(),
            ["0|37405000"]
        );
    }

    /// 止める指示と重なっても、シグナルで終わったのでない失敗（利用上限など）は記録する。
    /// 利用上限の使用率を失うと、次回すぐに呼んでしまう。
    #[tokio::test]
    async fn other_failures_near_a_stop_are_recorded() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([Err(crate::llm::LlmError::RateLimited {
            resets_at: Some(1),
        })]);
        let cancel = Cancel::default();
        let requester = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            requester.request();
        });
        let schema = serde_json::json!({});
        let outcome = call_recorded(
            &db,
            &llm,
            &Shared::new(&mut Quota::new(QuotaConfig::default(), None, None)),
            Call {
                stage: "digest",
                n_items: 1,
                req: LlmRequest {
                    system: "s",
                    prompt: "p",
                    schema: &schema,
                    model: "m",
                },
            },
            &Utc::now,
            &cancel,
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, Outcome::Halted(Halt::UsageLimit { .. })),
            "the usage limit must not be mistaken for a stop"
        );
        assert_eq!(db.query_i64("SELECT count(*) FROM llm_calls").unwrap(), 1);
    }

    /// 既に止める指示が出ていれば、LLM を呼ばない（claude を起動しない）。
    #[tokio::test]
    async fn does_not_call_after_cancel() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([]);
        let cancel = Cancel::default();
        cancel.request();
        let schema = serde_json::json!({});
        let outcome = call_recorded(
            &db,
            &llm,
            &Shared::new(&mut Quota::new(QuotaConfig::default(), None, None)),
            Call {
                stage: "digest",
                n_items: 1,
                req: LlmRequest {
                    system: "s",
                    prompt: "p",
                    schema: &schema,
                    model: "m",
                },
            },
            &Utc::now,
            &cancel,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::NotStarted));
        assert!(llm.requests().is_empty());
    }
}
