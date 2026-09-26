//! LLM ステージ共通の 1 回の呼び出し：呼んで、`llm_calls` に記録し、使用率をクォータに反映し、
//! 失敗を「止める理由」に振り分ける。

use chrono::{DateTime, Utc};

use super::Halt;
use crate::db::{Db, DbError, LlmCall};
use crate::errors;
use crate::llm::{Llm, LlmError, LlmRequest, LlmResponse};
use crate::quota::Quota;

pub enum Outcome {
    Response(LlmResponse),
    /// `UsageLimit` は記事の問題ではないので、呼び出し側は記事の失敗として記録しない。
    /// `LlmFailed` は認証切れなど記事によらない原因かもしれないので、呼び出し側は
    /// そのバッチだけ失敗にしてステージを止める。
    Halted(Halt),
}

pub async fn call_recorded<L: Llm>(
    db: &Db,
    llm: &L,
    quota: &mut Quota,
    stage: &str,
    n_items: usize,
    req: LlmRequest<'_>,
    now: DateTime<Utc>,
) -> Result<Outcome, DbError> {
    let started = std::time::Instant::now();
    let result = llm.call(req).await;
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
