//! LLM ステージ共通の 1 回の呼び出し：呼んで、`llm_calls` に記録し、使用率をクォータに反映し、
//! 失敗を「止める理由」に振り分ける。記事ごとの失敗の記録もここにまとめる。

use chrono::{DateTime, Utc};

use super::{Cancel, Halt};
use crate::db::{Db, DbError, LlmCall, StageKey};
use crate::errors;
use crate::llm::{Llm, LlmError, LlmRequest, LlmResponse};
use crate::quota::Quota;

pub enum Outcome {
    Response(LlmResponse),
    /// `UsageLimit` は記事の問題ではないので、呼び出し側は記事の失敗として記録しない。
    /// `LlmFailed` は認証切れなど記事によらない原因かもしれないので、呼び出し側は
    /// そのバッチだけ失敗にしてステージを止める。
    Halted(Halt),
    /// 止める指示で呼び出しをやめた。記事の失敗にも LLM の失敗にも数えず、次回続きから処理する
    Cancelled,
}

/// LLM ステージが共有する実行環境。クォータは実行全体で 1 つなので、ステージ間で引き継ぐ。
pub struct LlmStage<'a, L> {
    pub db: &'a Db,
    pub llm: &'a L,
    pub quota: &'a mut Quota,
    pub cancel: &'a Cancel,
    /// 作業を予約するときに今の時刻を読む（ステージの `now` は開始時の時刻のまま進まないが、予約の
    /// 期限は実際の時刻で決める。テストでは固定する）
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

/// 次の呼び出しをしてよいか。LLM を呼ぶ実行は並行して動くので、判定の前に DB の最新の使用率を
/// 読み、ほかの実行の呼び出しも判定に入れる。`reserve` は残す呼び出し回数（`permit_reserving`）。
pub fn permit(
    db: &Db,
    quota: &mut Quota,
    now: DateTime<Utc>,
    reserve: u32,
) -> Result<Result<(), crate::quota::Stop>, DbError> {
    quota.observe(db.latest_rate_limit()?);
    Ok(quota.permit_reserving(now, reserve))
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

pub async fn call_recorded<L: Llm>(
    db: &Db,
    llm: &L,
    quota: &mut Quota,
    Call {
        stage,
        n_items,
        req,
    }: Call<'_>,
    now: DateTime<Utc>,
    cancel: &Cancel,
) -> Result<Outcome, DbError> {
    // 既に止める指示が出ていれば呼ばない（応答を優先する下の select は、先に呼び出しを始めてしまう）
    if cancel.is_requested() {
        return Ok(Outcome::Cancelled);
    }
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
        Err(LlmError::Exit {
            interrupted: true,
            ..
        })
    ) && tokio::time::timeout(STOP_GRACE, cancel.requested())
        .await
        .is_ok()
    {
        return Ok(Outcome::Cancelled);
    }
    // 上限で拒否されたときも、そのときの使用率を残して次回の判定に使う。
    let rate_limit = match &result {
        Ok(response) => response.rate_limit,
        Err(LlmError::RateLimited { rate_limit, .. }) => *rate_limit,
        Err(_) => None,
    };
    quota.record_call(rate_limit);
    let error = result.as_ref().err().map(|e| errors::error_chain(e));
    db.record_llm_call(
        &LlmCall {
            stage,
            backend: llm.backend(),
            model: req.model,
            n_items,
            ok: result.is_ok(),
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            error: error.as_deref(),
            rate_limit: rate_limit.as_ref(),
        },
        now,
    )?;
    Ok(match result {
        Ok(response) => Outcome::Response(response),
        Err(LlmError::RateLimited { resets_at, .. }) => {
            Outcome::Halted(Halt::UsageLimit { resets_at })
        }
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
            &mut Quota::new(QuotaConfig::default(), None, None),
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
            Utc::now(),
            &cancel,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Cancelled));
        assert_eq!(db.query_i64("SELECT count(*) FROM llm_calls").unwrap(), 0);
    }

    /// 止める指示と重なっても、シグナルで終わったのでない失敗（利用上限など）は記録する。
    /// 利用上限の使用率を失うと、次回すぐに呼んでしまう。
    #[tokio::test]
    async fn other_failures_near_a_stop_are_recorded() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([Err(crate::llm::LlmError::RateLimited {
            resets_at: Some(1),
            rate_limit: None,
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
            &mut Quota::new(QuotaConfig::default(), None, None),
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
            Utc::now(),
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
            &mut Quota::new(QuotaConfig::default(), None, None),
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
            Utc::now(),
            &cancel,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Cancelled));
        assert!(llm.requests().is_empty());
    }
}
