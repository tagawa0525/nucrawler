//! LLM ステージ共通の 1 回の呼び出し：呼んで、`llm_calls` に記録し、使用率をクォータに反映し、
//! 失敗を「止める理由」に振り分ける。

use chrono::{DateTime, Utc};

use super::{Cancel, Halt};
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
    /// 止める指示で呼び出しをやめた。記事の失敗にも LLM の失敗にも数えず、次回続きから処理する
    Cancelled,
}

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
    let started = std::time::Instant::now();
    // 応答を待たずに止める。呼び出しの future を捨てると子プロセスも止まる（kill_on_drop）
    let result = tokio::select! {
        // 応答と止める指示が同時に届いたら、応答を捨てずに使う
        biased;
        result = llm.call(req) => result,
        () = cancel.requested() => return Ok(Outcome::Cancelled),
    };
    // 止める指示と同時に子プロセスが終了させられたときも（systemd が unit の全プロセスに
    // SIGTERM を送った場合など）、LLM の失敗ではない
    if result.is_err() && cancel.is_requested() {
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
