//! 採点ステージ：利用者のプロファイルと最近の反応をもとに、要約済みの記事を数件ずつ LLM で採点する。
//! プロファイルのハッシュごとに記録するので、プロファイルを変えれば自動的に採点し直しになる。

use chrono::{DateTime, Utc};

use super::{Cancel, Halt};
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::{Db, DbError};
use crate::llm::Llm;
use crate::quota::Quota;

pub const STAGE: &str = "score";

/// 採点の参考にする直近の反応の件数
const SIGNALS: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum ScoreStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

#[derive(Debug, Default, PartialEq)]
pub struct ScoreSummary {
    pub scored: usize,
    pub failed: usize,
    pub calls: usize,
    pub halted: Option<Halt>,
    pub cancelled: bool,
    /// プロファイルが未登録で、採点しなかった
    pub no_profile: bool,
}

#[allow(clippy::too_many_arguments)]
pub async fn score_articles<L: Llm>(
    _db: &Db,
    _llm: &L,
    _quota: &mut Quota,
    _llm_cfg: &LlmConfig,
    _pipeline_cfg: &PipelineConfig,
    _user_id: i64,
    _now: DateTime<Utc>,
    _cancel: &Cancel,
) -> Result<ScoreSummary, ScoreStageError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{
        ArtifactKind, ContentKind, ContentOrigin, NewArticle, NewArtifact, ScoreKey, SignalKind,
    };
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse};
    use crate::quota::{QuotaConfig, Stop};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-28T02:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn cfg(batch: usize) -> LlmConfig {
        LlmConfig {
            score_batch_size: batch,
            ..LlmConfig::default()
        }
    }

    fn quota(max_calls: u32) -> Quota {
        Quota::new(QuotaConfig::default(), None, Some(max_calls))
    }

    fn setup(n: usize) -> (Db, i64, Vec<i64>) {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let profile = crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        db.save_profile(owner, &profile, now()).unwrap();
        let ids = (0..n)
            .map(|i| {
                let url = format!("https://e.com/{i}");
                let published = format!("2026-09-27T{:02}:00:00.000Z", 20 - i);
                let id = db
                    .insert_article(&NewArticle {
                        source_id: "wnn",
                        url: &url,
                        title: "t",
                        lang: Lang::En,
                        published_at: Some(&published),
                    })
                    .unwrap()
                    .unwrap();
                let c = db
                    .insert_content(id, ContentKind::Body, ContentOrigin::Page, "body")
                    .unwrap();
                let payload = serde_json::json!({
                    "title_ja": format!("記事{i}"), "summary_ja": "要約", "points_ja": ["点"],
                    "implications_ja": "", "lwr_relevant": true, "topics": ["燃料"],
                });
                db.insert_artifact(
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
                id
            })
            .collect();
        (db, owner, ids)
    }

    fn ok(scores: &[(i64, u8)]) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"items": scores.iter().map(|(id, s)| {
                serde_json::json!({"id": id, "score": s, "reason": "理由"})
            }).collect::<Vec<_>>()}),
            rate_limit: None,
        })
    }

    async fn run(
        db: &Db,
        owner: i64,
        llm: &FakeLlm,
        quota: &mut Quota,
        batch: usize,
    ) -> ScoreSummary {
        score_articles(
            db,
            llm,
            quota,
            &cfg(batch),
            &PipelineConfig::default(),
            owner,
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn scores_digests_with_profile_and_signals() {
        let (db, owner, ids) = setup(3);
        db.record_event(owner, ids[2], SignalKind::Down, now())
            .unwrap();
        let llm = FakeLlm::new([ok(&[(ids[0], 90), (ids[1], 40)]), ok(&[(ids[2], 5)])]);
        let summary = run(&db, owner, &llm, &mut quota(10), 2).await;
        assert_eq!((summary.scored, summary.failed, summary.calls), (3, 0, 2));
        let reqs = llm.requests();
        assert_eq!(reqs[0].model, "sonnet");
        assert_eq!(reqs[0].schema, crate::scoring::schema());
        assert!(
            reqs[0].system.contains("規制・審査"),
            "profile in system prompt"
        );
        assert!(reqs[0].system.contains("記事2"), "signals in system prompt");
        assert_eq!(
            db.query_strings(
                "SELECT s.score || '|' || s.backend || '|' || s.model FROM scores AS s
                 JOIN artifacts AS r ON r.id = s.artifact_id ORDER BY r.article_id"
            )
            .unwrap(),
            ["90|fake|sonnet", "40|fake|sonnet", "5|fake|sonnet"]
        );
        assert_eq!(
            db.query_strings("SELECT stage FROM llm_calls").unwrap(),
            ["score", "score"]
        );
    }

    #[tokio::test]
    async fn skips_without_profile() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let summary = run(&db, owner, &FakeLlm::new([]), &mut quota(10), 5).await;
        assert!(summary.no_profile);
        assert_eq!(summary.calls, 0);
    }

    #[tokio::test]
    async fn missing_scores_are_retried_later_for_this_profile() {
        let (db, owner, ids) = setup(2);
        let llm = FakeLlm::new([ok(&[(ids[0], 70)])]);
        let summary = run(&db, owner, &llm, &mut quota(10), 5).await;
        assert_eq!((summary.scored, summary.failed), (1, 1));
        let again = run(&db, owner, &FakeLlm::new([]), &mut quota(10), 5).await;
        assert_eq!(again.calls, 0);
        let (_, hash) = db.load_profile(owner).unwrap().unwrap();
        let stage = crate::db::score_stage(ScoreKey {
            user_id: owner,
            profile_hash: &hash,
            backend: "fake",
            model: "sonnet",
        });
        assert_eq!(
            db.query_strings("SELECT stage FROM stage_errors").unwrap(),
            [stage]
        );
    }

    #[tokio::test]
    async fn halts_on_quota_usage_limit_and_llm_failure() {
        let (db, owner, _) = setup(2);
        let summary = run(&db, owner, &FakeLlm::new([ok(&[])]), &mut quota(1), 1).await;
        assert_eq!(
            summary.halted,
            Some(Halt::Quota(Stop::MaxCalls { limit: 1 }))
        );

        let (db, owner, _) = setup(1);
        let llm = FakeLlm::new([Err(LlmError::RateLimited {
            resets_at: Some(1),
            rate_limit: None,
        })]);
        let summary = run(&db, owner, &llm, &mut quota(10), 1).await;
        assert_eq!(
            summary.halted,
            Some(Halt::UsageLimit { resets_at: Some(1) })
        );
        assert_eq!(summary.failed, 0);

        let (db, owner, _) = setup(1);
        let llm = FakeLlm::new([Err(LlmError::Reported {
            subtype: "error".into(),
            message: "Not logged in".into(),
        })]);
        let summary = run(&db, owner, &llm, &mut quota(10), 1).await;
        assert!(matches!(summary.halted, Some(Halt::LlmFailed(_))));
        assert_eq!(summary.failed, 1);
    }
}
