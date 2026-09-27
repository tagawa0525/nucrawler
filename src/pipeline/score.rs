//! 採点ステージ：利用者のプロファイルと最近の反応をもとに、要約済みの記事を数件ずつ LLM で採点する。
//! プロファイルのハッシュごとに記録するので、プロファイルを変えれば自動的に採点し直しになる。

use chrono::{DateTime, Utc};

use super::Halt;
use super::llm_call::{Call, LlmStage, Outcome, call_recorded};
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::{DbError, ScoreKey, StageKey, score_stage};
use crate::llm::{Llm, LlmRequest};
use crate::{errors, scoring};

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

pub async fn score_articles<L: Llm>(
    LlmStage {
        db,
        llm,
        quota,
        cancel,
    }: LlmStage<'_, L>,
    llm_cfg: &LlmConfig,
    pipeline_cfg: &PipelineConfig,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<ScoreSummary, ScoreStageError> {
    let mut summary = ScoreSummary::default();
    let Some((profile, profile_hash)) = db.load_profile(user_id)? else {
        tracing::warn!("no profile yet; run `nucrawler profile import FILE` to enable scoring");
        summary.no_profile = true;
        return Ok(summary);
    };
    let backend = llm.backend();
    let model = llm_cfg.score_model.as_str();
    let key = ScoreKey {
        user_id,
        profile_hash: &profile_hash,
        backend,
        model,
    };
    let failure_stage = score_stage(key);
    let failure_key = |article_id| StageKey {
        article_id,
        stage: &failure_stage,
        backend,
        model,
    };
    let cutoff = now - chrono::Duration::days(i64::from(pipeline_cfg.backlog_days));
    let system = scoring::system_prompt(&profile, &db.recent_signals(user_id, SIGNALS)?);
    let schema = scoring::schema();
    loop {
        if cancel.is_requested() {
            summary.cancelled = true;
            break;
        }
        if let Err(stop) = quota.permit(now) {
            tracing::info!("score stops: {stop}");
            summary.halted = Some(Halt::Quota(stop));
            break;
        }
        let batch = db.pending_score(key, cutoff, now, llm_cfg.score_batch_size)?;
        if batch.is_empty() {
            break;
        }
        let ids: Vec<i64> = batch.iter().map(|b| b.article_id).collect();
        let prompt = scoring::build_prompt(&batch);
        let outcome = call_recorded(
            db,
            llm,
            quota,
            Call {
                stage: STAGE,
                n_items: batch.len(),
                req: LlmRequest {
                    system: &system,
                    prompt: &prompt,
                    schema: &schema,
                    model,
                },
            },
            now,
            cancel,
        )
        .await?;
        summary.calls += 1;
        let response = match outcome {
            Outcome::Response(response) => response,
            Outcome::Cancelled => {
                summary.cancelled = true;
                break;
            }
            Outcome::Halted(halt) => {
                if let Halt::LlmFailed(message) = &halt {
                    for &id in &ids {
                        db.record_stage_failure(failure_key(id), message, now, false)?;
                    }
                    summary.failed += ids.len();
                }
                summary.halted = Some(halt);
                break;
            }
        };
        let parsed = match scoring::parse(&response.output, &ids) {
            Ok(parsed) => parsed,
            Err(e) => {
                let message = errors::error_chain(&e);
                tracing::warn!("score output rejected: {message}");
                for &id in &ids {
                    db.record_stage_failure(failure_key(id), &message, now, false)?;
                }
                summary.failed += ids.len();
                continue;
            }
        };
        for (article_id, score, reason) in &parsed.items {
            let Some(input) = batch.iter().find(|b| b.article_id == *article_id) else {
                continue;
            };
            db.insert_score(key, input.artifact_id, *score, Some(reason), now)?;
            db.clear_stage_failure(failure_key(*article_id))?;
            summary.scored += 1;
        }
        for &id in &parsed.missing {
            db.record_stage_failure(
                failure_key(id),
                "missing or invalid in the llm output",
                now,
                false,
            )?;
        }
        summary.failed += parsed.missing.len();
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{
        ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact, SignalKind,
    };
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse};
    use crate::pipeline::Cancel;
    use crate::quota::{Quota, QuotaConfig, Stop};

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
            LlmStage {
                db,
                llm,
                quota,
                cancel: &Cancel::default(),
            },
            &cfg(batch),
            &PipelineConfig::default(),
            owner,
            now(),
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
