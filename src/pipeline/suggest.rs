//! プロファイルの更新案（`profile suggest`）：反応を根拠に、LLM にプロファイルの更新案を 1 回で作らせる。
//! 案は保存しない（人が差分を読み、`eval --profile` で比べてから取り込む）。

use chrono::{DateTime, Utc};

use super::Halt;
use super::llm_call::{Call, LlmStage, Outcome, call_recorded};
use crate::config::LlmConfig;
use crate::db::{DbError, Evidence};
use crate::llm::{Llm, LlmRequest};
use crate::profile::Profile;
use crate::prompt;
use crate::prompt::suggest::{SuggestError, Suggestion};

pub const STAGE: &str = "suggest";

#[derive(Debug, thiserror::Error)]
pub enum SuggestStageError {
    #[error("database error")]
    Db(#[from] DbError),
    /// 応答がスキーマやプロファイルの規則に合わなかった（案は書かない）
    #[error("the suggested profile was rejected")]
    Rejected(#[from] SuggestError),
}

#[derive(Debug, Default)]
pub struct SuggestSummary {
    pub suggestion: Option<Suggestion>,
    pub calls: usize,
    pub halted: Option<Halt>,
    pub cancelled: bool,
}

/// モデルは採点と同じ `llm.score_model`（点数の付け方を知っているモデルに、その元を見直させる）。
pub async fn suggest_profile<L: Llm>(
    LlmStage {
        db,
        llm,
        quota,
        cancel,
    }: LlmStage<'_, L>,
    cfg: &LlmConfig,
    profile: &Profile,
    evidence: &[Evidence],
    now: DateTime<Utc>,
) -> Result<SuggestSummary, SuggestStageError> {
    let mut summary = SuggestSummary::default();
    if let Err(stop) = quota.permit(now) {
        tracing::info!("suggest stops: {stop}");
        summary.halted = Some(Halt::Quota(stop));
        return Ok(summary);
    }
    let prompt = prompt::suggest::build_prompt(profile, evidence);
    let schema = prompt::suggest::schema();
    let outcome = call_recorded(
        db,
        llm,
        quota,
        Call {
            stage: STAGE,
            n_items: evidence.len(),
            req: LlmRequest {
                system: prompt::suggest::system_prompt(),
                prompt: &prompt,
                schema: &schema,
                model: &cfg.score_model,
            },
        },
        now,
        cancel,
    )
    .await?;
    let response = match outcome {
        Outcome::Response(response) => response,
        Outcome::Cancelled => {
            summary.cancelled = true;
            return Ok(summary);
        }
        Outcome::Halted(halt) => {
            summary.calls += 1;
            summary.halted = Some(halt);
            return Ok(summary);
        }
    };
    summary.calls += 1;
    summary.suggestion = Some(prompt::suggest::parse(&response.output)?);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse};
    use crate::pipeline::Cancel;
    use crate::quota::{Quota, QuotaConfig};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-28T02:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn profile() -> Profile {
        crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap()
    }

    fn evidence() -> Vec<Evidence> {
        vec![Evidence {
            article_id: 1,
            positive: true,
            title_ja: "SMR の建設許可".into(),
            topics: vec!["新設・建設".into()],
            at: "2026-09-27T00:00:00.000Z".into(),
        }]
    }

    fn answer(weight: f64) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({
                "interests": [{"topic": "規制・審査", "weight": weight, "note": ""}],
                "exclude": [],
                "reasons": [{"change": "重みを変えた", "evidence": "関心 1 件"}],
            }),
            rate_limit: None,
        })
    }

    async fn run(
        db: &Db,
        llm: &FakeLlm,
        quota: &mut Quota,
    ) -> Result<SuggestSummary, SuggestStageError> {
        suggest_profile(
            LlmStage {
                db,
                llm,
                quota,
                cancel: &Cancel::default(),
            },
            &LlmConfig::default(),
            &profile(),
            &evidence(),
            now(),
        )
        .await
    }

    fn quota(max_calls: u32) -> Quota {
        Quota::new(QuotaConfig::default(), None, Some(max_calls))
    }

    #[tokio::test]
    async fn asks_once_and_returns_the_suggestion() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([answer(0.8)]);
        let summary = run(&db, &llm, &mut quota(10)).await.unwrap();
        let suggestion = summary.suggestion.unwrap();
        assert_eq!(suggestion.profile.interests[0].weight, 0.8);
        assert_eq!(summary.calls, 1);
        let reqs = llm.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].model, LlmConfig::default().score_model);
        assert_eq!(reqs[0].system, crate::prompt::suggest::system_prompt());
        assert_eq!(
            reqs[0].prompt,
            crate::prompt::suggest::build_prompt(&profile(), &evidence())
        );
        assert_eq!(reqs[0].schema, crate::prompt::suggest::schema());
        assert_eq!(
            db.query_strings("SELECT stage FROM llm_calls").unwrap(),
            [STAGE]
        );
    }

    #[tokio::test]
    async fn rejects_an_invalid_suggestion() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([answer(1.5)]);
        assert!(matches!(
            run(&db, &llm, &mut quota(10)).await,
            Err(SuggestStageError::Rejected(_))
        ));
    }

    #[tokio::test]
    async fn stops_without_calling_when_the_quota_is_used_up() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([]);
        let summary = run(&db, &llm, &mut quota(0)).await.unwrap();
        assert!(summary.suggestion.is_none());
        assert!(matches!(summary.halted, Some(Halt::Quota(_))));
        assert!(llm.requests().is_empty());
    }
}
