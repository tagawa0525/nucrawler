//! 和訳ステージ：依頼された記事と、点数の高い英語記事の本文を 1 件ずつ全文和訳する。

use chrono::{DateTime, Utc};

use super::{Cancel, Halt};
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::{Db, DbError};
use crate::llm::Llm;
use crate::quota::Quota;

pub const STAGE: &str = "translate";

#[derive(Debug, thiserror::Error)]
pub enum TranslateStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

#[derive(Debug, Default, PartialEq)]
pub struct TranslateSummary {
    pub translated: usize,
    pub failed: usize,
    pub calls: usize,
    pub halted: Option<Halt>,
    pub cancelled: bool,
}

/// `requests_only` なら依頼された記事だけを和訳する（先回りはしない）。
#[allow(clippy::too_many_arguments)]
pub async fn translate_articles<L: Llm>(
    _db: &Db,
    _llm: &L,
    _quota: &mut Quota,
    _llm_cfg: &LlmConfig,
    _pipeline_cfg: &PipelineConfig,
    _user_id: i64,
    _requests_only: bool,
    _now: DateTime<Utc>,
    _cancel: &Cancel,
) -> Result<TranslateSummary, TranslateStageError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{ArtifactKind, ContentKind, ContentOrigin, NewArticle, NewArtifact, ScoreKey};
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse};
    use crate::quota::{QuotaConfig, Stop};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-28T02:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn quota(max_calls: u32) -> Quota {
        Quota::new(QuotaConfig::default(), None, Some(max_calls))
    }

    /// 本文・digest・採点つきの英語記事を登録する。
    fn article(db: &Db, i: usize, score: u8) -> i64 {
        let owner = db.owner_id().unwrap();
        let url = format!("https://e.com/{i}");
        let published = format!("2026-09-27T{:02}:00:00.000Z", 20 - i);
        let id = db
            .insert_article(&NewArticle {
                source_id: "wnn",
                url: &url,
                title: &format!("Title {i}"),
                lang: Lang::En,
                published_at: Some(&published),
            })
            .unwrap()
            .unwrap();
        let c = db
            .insert_content(
                id,
                ContentKind::Body,
                ContentOrigin::Page,
                &format!("Body {i}"),
            )
            .unwrap();
        let payload = serde_json::json!({
            "title_ja": "題", "summary_ja": "要約", "points_ja": ["点"],
            "implications_ja": "", "lwr_relevant": true, "topics": ["燃料"],
        });
        let digest = db
            .insert_artifact(
                &NewArtifact {
                    article_id: id,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[c],
                },
                now(),
            )
            .unwrap();
        let (_, hash) = db.load_profile(owner).unwrap().unwrap();
        db.insert_score(
            ScoreKey {
                user_id: owner,
                profile_hash: &hash,
                backend: "claude-cli",
                model: "sonnet",
            },
            digest,
            score,
            None,
            now(),
        )
        .unwrap();
        id
    }

    fn setup() -> (Db, i64) {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let profile = crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        db.save_profile(owner, &profile, now()).unwrap();
        (db, owner)
    }

    fn ok(body_ja: &str) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"body_ja": body_ja}),
            rate_limit: None,
        })
    }

    async fn run(
        db: &Db,
        owner: i64,
        llm: &FakeLlm,
        quota: &mut Quota,
        requests_only: bool,
    ) -> TranslateSummary {
        translate_articles(
            db,
            llm,
            quota,
            &LlmConfig::default(),
            &PipelineConfig::default(),
            owner,
            requests_only,
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn translates_requests_then_high_scores_and_completes_requests() {
        let (db, owner) = setup();
        let high = article(&db, 0, 90);
        let _low = article(&db, 1, 40);
        let requested = article(&db, 2, 10);
        db.request_translation(owner, requested, now()).unwrap();
        let llm = FakeLlm::new([ok("依頼の和訳"), ok("高得点の和訳")]);
        let summary = run(&db, owner, &llm, &mut quota(10), false).await;
        assert_eq!(
            (summary.translated, summary.failed, summary.calls),
            (2, 0, 2)
        );
        let reqs = llm.requests();
        assert!(reqs[0].prompt.contains("Body 2"), "requests first");
        assert!(reqs[1].prompt.contains("Body 0"));
        assert_eq!(reqs[0].system, crate::translate::system_prompt());
        assert_eq!(reqs[0].schema, crate::translate::schema());
        assert_eq!(
            db.query_strings(
                "SELECT article_id || '|' || backend || '|' || model || '|' ||
                        json_extract(payload, '$.body_ja')
                 FROM artifacts WHERE kind = 'translation' ORDER BY article_id"
            )
            .unwrap(),
            [
                format!("{high}|fake|sonnet|高得点の和訳"),
                format!("{requested}|fake|sonnet|依頼の和訳")
            ]
        );
        assert_eq!(
            db.query_i64("SELECT count(*) FROM translation_requests WHERE done_at IS NULL")
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_i64("SELECT count(*) FROM artifact_inputs AS i JOIN artifacts AS r ON r.id = i.artifact_id WHERE r.kind = 'translation'").unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn requests_only_skips_proactive_translation() {
        let (db, owner) = setup();
        article(&db, 0, 99);
        let summary = run(&db, owner, &FakeLlm::new([]), &mut quota(10), true).await;
        assert_eq!(summary.calls, 0);
    }

    #[tokio::test]
    async fn invalid_output_is_retried_later() {
        let (db, owner) = setup();
        article(&db, 0, 90);
        let summary = run(&db, owner, &FakeLlm::new([ok("  ")]), &mut quota(10), false).await;
        assert_eq!(
            (summary.translated, summary.failed, summary.calls),
            (0, 1, 1)
        );
        let again = run(&db, owner, &FakeLlm::new([]), &mut quota(10), false).await;
        assert_eq!(again.calls, 0);
    }

    #[tokio::test]
    async fn halts_on_quota_usage_limit_and_llm_failure() {
        let (db, owner) = setup();
        article(&db, 0, 90);
        article(&db, 1, 90);
        let summary = run(&db, owner, &FakeLlm::new([ok("訳")]), &mut quota(1), false).await;
        assert_eq!(
            summary.halted,
            Some(Halt::Quota(Stop::MaxCalls { limit: 1 }))
        );

        let (db, owner) = setup();
        article(&db, 0, 90);
        let llm = FakeLlm::new([Err(LlmError::RateLimited {
            resets_at: None,
            rate_limit: None,
        })]);
        let summary = run(&db, owner, &llm, &mut quota(10), false).await;
        assert_eq!(summary.halted, Some(Halt::UsageLimit { resets_at: None }));
        assert_eq!(summary.failed, 0);

        let (db, owner) = setup();
        article(&db, 0, 90);
        let llm = FakeLlm::new([Err(LlmError::Timeout { secs: 300 })]);
        let summary = run(&db, owner, &llm, &mut quota(10), false).await;
        assert!(matches!(summary.halted, Some(Halt::LlmFailed(_))));
        assert_eq!(summary.failed, 1);
    }

    #[tokio::test]
    async fn stops_when_cancelled() {
        let (db, owner) = setup();
        article(&db, 0, 90);
        let cancel = Cancel::default();
        cancel.request();
        let summary = translate_articles(
            &db,
            &FakeLlm::new([]),
            &mut quota(10),
            &LlmConfig::default(),
            &PipelineConfig::default(),
            owner,
            false,
            now(),
            &cancel,
        )
        .await
        .unwrap();
        assert!(summary.cancelled);
    }
}
