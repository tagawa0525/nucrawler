//! 採点ステージ：利用者のプロファイルをもとに、要約済みの記事を数件ずつ LLM で採点する。
//! プロファイルのハッシュと採点のプロンプトの版ごとに記録するので、どちらかを変えれば自動的に採点し直しになる。

use std::borrow::Cow;

use chrono::{DateTime, Utc};

use super::Halt;
use super::llm_call::{
    Call, LlmStage, MISSING, Outcome, Reserved, Shared, call_recorded, claim_ttl, held_missing,
    permit, record_failures, reserve,
};
use super::workers::run_workers;
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::{ClaimKey, DbError, ScoreKey, ScoreMatches, ScoreScope, StageKey, score_stage};
use crate::errors;
use crate::llm::{Llm, LlmRequest};
use crate::profile::Profile;
use crate::prompt;

pub const STAGE: &str = "score";

#[derive(Debug, thiserror::Error)]
pub enum ScoreStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

/// 採点の対象。
#[derive(Debug, Clone, Copy)]
pub enum ScoreTarget<'a> {
    /// crawl の採点：保存済みのプロファイルで、`backlog_days` の範囲のまだ採点していない記事
    Saved,
    /// `eval --profile`：渡したプロファイルで、指定した記事のうちまだ採点していないもの
    Candidate {
        profile: &'a Profile,
        articles: &'a [i64],
    },
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

impl ScoreSummary {
    /// 作業者ごとの集計を合わせる。
    fn merge(mut self, other: ScoreSummary) -> ScoreSummary {
        self.scored += other.scored;
        self.failed += other.failed;
        self.calls += other.calls;
        self.halted = Halt::most_severe(self.halted, other.halted);
        self.cancelled |= other.cancelled;
        self.no_profile |= other.no_profile;
        self
    }
}

pub async fn score_articles<L: Llm>(
    LlmStage {
        db,
        llm,
        quota,
        cancel,
        clock,
    }: LlmStage<'_, L>,
    llm_cfg: &LlmConfig,
    pipeline_cfg: &PipelineConfig,
    user_id: i64,
    target: ScoreTarget<'_>,
    now: DateTime<Utc>,
) -> Result<ScoreSummary, ScoreStageError> {
    let mut summary = ScoreSummary::default();
    let (profile, profile_hash, scope) = match target {
        ScoreTarget::Saved => {
            let Some((profile, hash)) = db.load_profile(user_id)? else {
                tracing::warn!(
                    "no profile yet; run `nucrawler profile import FILE` to enable scoring"
                );
                summary.no_profile = true;
                return Ok(summary);
            };
            let cutoff = now - chrono::Duration::days(i64::from(pipeline_cfg.backlog_days));
            (Cow::Owned(profile), hash, ScoreScope::Since(cutoff))
        }
        ScoreTarget::Candidate { profile, articles } => (
            Cow::Borrowed(profile),
            crate::profile::hash(profile),
            ScoreScope::Articles(articles),
        ),
    };
    let backend = llm.backend();
    let model = llm_cfg.score_model.as_str();
    let key = ScoreKey {
        user_id,
        profile_hash: &profile_hash,
        backend,
        model,
        prompt_version: prompt::score::PROMPT_VERSION,
    };
    let failure_stage = score_stage(key);
    let failure_key = |article_id| StageKey {
        article_id,
        stage: &failure_stage,
        backend,
        model,
    };
    let system = prompt::score::system_prompt(&profile);
    let schema = prompt::score::schema(&profile);
    let shared = Shared::new(quota);
    // `llm.concurrency` 個の作業者を同時に回す。同じ記事は作業の予約で分かれる
    let parts = run_workers(llm_cfg.concurrency, |_| async {
        let mut summary = ScoreSummary::default();
        loop {
            // 同時に終わったほかの作業者の結果（止める旗）が伝わってから次の周に入る
            // （作業者は決まった順で進むので、譲らないと先の作業者が次の呼び出しを始めてしまう）
            tokio::task::yield_now().await;
            if shared.stopped() {
                break;
            }
            if cancel.is_requested() {
                summary.cancelled = true;
                break;
            }
            // 呼び出しの枠を先に取り、判定・予約・呼び出しをその中で行う（枠はこの周の終わりまで持つ）
            let _slot = match reserve(llm, cancel).await {
                Reserved::Slot(slot) => slot,
                Reserved::Cancelled => {
                    summary.cancelled = true;
                    break;
                }
                Reserved::Failed(message) => {
                    summary.halted = Some(Halt::LlmFailed(message));
                    break;
                }
            };
            // 枠を待つ間にほかの作業者が止まっていたら、呼ばずに止まる
            if shared.stopped() {
                break;
            }
            if let Err(stop) = permit(db, &shared, clock(), 0)? {
                tracing::info!("score stops: {stop}");
                summary.halted = Some(Halt::Quota(stop));
                break;
            }
            // 予約は処理を終える（この周の終わりで drop する）まで持つ
            let (batch, claim) = db.claim_selected(
                ClaimKey {
                    stage: &failure_stage,
                    backend,
                    model,
                },
                clock(),
                claim_ttl(llm_cfg),
                |db| db.pending_score(key, scope, now, llm_cfg.score_batch_size),
                |b| b.article_id,
            )?;
            if batch.is_empty() {
                break;
            }
            let ids: Vec<i64> = batch.iter().map(|b| b.article_id).collect();
            let prompt = prompt::score::build_prompt(&batch);
            let outcome = call_recorded(
                db,
                llm,
                &shared,
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
                clock,
                cancel,
            )
            .await?;
            summary.calls += 1;
            // 結果を書く前に予約を延長する。呼び出しの最中に期限が切れてほかの実行に取り直された記事は
            // 延長できないので、以降は保存も失敗の記録もしない（予約を持っている実行だけが書く）
            let held = claim.renew(clock(), claim_ttl(llm_cfg))?;
            let response = match outcome {
                Outcome::Response(response) => response,
                Outcome::Cancelled => {
                    summary.cancelled = true;
                    break;
                }
                Outcome::Halted(halt) => {
                    if let Halt::LlmFailed(message) = &halt {
                        summary.failed += record_failures(
                            db,
                            held.iter().map(|&id| failure_key(id)),
                            message,
                            now,
                        )?;
                    }
                    summary.halted = Some(halt);
                    break;
                }
            };
            let parsed = match prompt::score::parse(&response.output, &ids, &profile) {
                Ok(parsed) => parsed,
                Err(e) => {
                    let message = errors::error_chain(&e);
                    tracing::warn!("score output rejected: {message}");
                    summary.failed +=
                        record_failures(db, held.iter().map(|&id| failure_key(id)), &message, now)?;
                    continue;
                }
            };
            for item in &parsed.items {
                let Some(input) = batch.iter().find(|b| b.article_id == item.id) else {
                    continue;
                };
                if !held.contains(&item.id) {
                    tracing::warn!(
                        article_id = item.id,
                        "{STAGE} result dropped: the claim was taken over"
                    );
                    continue;
                }
                db.insert_score_with_matches(
                    key,
                    input.artifact_id,
                    item.score,
                    Some(&item.reason),
                    ScoreMatches {
                        interests: &item.matched,
                        excludes: &item.excluded,
                    },
                    now,
                )?;
                db.clear_stage_failure(failure_key(item.id))?;
                summary.scored += 1;
            }
            summary.failed += record_failures(
                db,
                held_missing(&parsed.missing, &held).map(failure_key),
                MISSING,
                now,
            )?;
        }
        // 止まった作業者は、ほかの作業者も止める（空になって終わったときは止めない）
        if summary.halted.is_some() || summary.cancelled {
            shared.stop();
        }
        Ok::<_, ScoreStageError>(summary)
    })
    .await?;
    Ok(parts
        .into_iter()
        .fold(ScoreSummary::default(), ScoreSummary::merge))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{
        ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact, Rating,
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
                        glossary_at: None,
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
                serde_json::json!({
                    "id": id, "score": s, "reason": "理由", "matched": ["燃料"], "excluded": [],
                })
            }).collect::<Vec<_>>()}),
            usage: None,
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
                clock: &now,
            },
            &cfg(batch),
            &PipelineConfig::default(),
            owner,
            ScoreTarget::Saved,
            now(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn scores_digests_with_profile_only() {
        let (db, owner, ids) = setup(3);
        db.rate(owner, ids[2], Rating::new(2), now()).unwrap();
        let llm = FakeLlm::new([ok(&[(ids[0], 90), (ids[1], 40)]), ok(&[(ids[2], 5)])]);
        let summary = run(&db, owner, &llm, &mut quota(10), 2).await;
        assert_eq!((summary.scored, summary.failed, summary.calls), (3, 0, 2));
        let reqs = llm.requests();
        assert_eq!(reqs[0].model, "sonnet");
        assert_eq!(
            reqs[0].schema,
            crate::prompt::score::schema(
                &crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap()
            )
        );
        assert!(
            reqs[0].system.contains("規制・審査"),
            "profile in system prompt"
        );
        assert!(
            !reqs[0].system.contains("記事2"),
            "reactions are not in the system prompt"
        );
        assert_eq!(
            db.query_strings(
                "SELECT s.score || '|' || s.backend || '|' || s.model || '|' || s.prompt_version
                 FROM scores AS s
                 JOIN artifacts AS r ON r.id = s.artifact_id ORDER BY r.article_id"
            )
            .unwrap(),
            [90, 40, 5].map(|score| format!(
                "{score}|fake|sonnet|{}",
                crate::prompt::score::PROMPT_VERSION
            ))
        );
        assert_eq!(
            db.query_strings("SELECT stage FROM llm_calls").unwrap(),
            ["score", "score"]
        );
        // 当たった分野も採点と一緒に残す
        assert_eq!(
            db.query_i64(
                "SELECT count(*) FROM score_matches WHERE kind = 'interest' AND topic = '燃料'"
            )
            .unwrap(),
            3
        );
    }

    /// 候補のプロファイルは保存せず、その hash で指定した記事だけを採点する。
    #[tokio::test]
    async fn scores_given_articles_with_a_candidate_profile() {
        let (db, owner, ids) = setup(3);
        let mut candidate =
            crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        candidate.interests[0].topic = "候補の分野".into();
        let candidate_hash = crate::profile::hash(&candidate);
        let llm = FakeLlm::new([ok(&[(ids[2], 60)])]);
        let summary = score_articles(
            LlmStage {
                db: &db,
                llm: &llm,
                quota: &mut quota(10),
                cancel: &Cancel::default(),
                clock: &now,
            },
            &cfg(5),
            &PipelineConfig::default(),
            owner,
            ScoreTarget::Candidate {
                profile: &candidate,
                articles: &[ids[2]],
            },
            now(),
        )
        .await
        .unwrap();
        assert_eq!((summary.scored, summary.calls), (1, 1));
        assert!(llm.requests()[0].system.contains("候補の分野"));
        assert_eq!(
            db.query_strings("SELECT profile_hash || ':' || score FROM scores")
                .unwrap(),
            [format!("{candidate_hash}:60")]
        );
        // 保存済みのプロファイルは変えない
        let (saved, _) = db.load_profile(owner).unwrap().unwrap();
        assert_ne!(saved.interests[0].topic, "候補の分野");
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
            prompt_version: crate::prompt::score::PROMPT_VERSION,
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

    /// ほかの実行が予約している記事は飛ばし、自分の予約は処理を終えたら外す。
    #[tokio::test]
    async fn skips_articles_claimed_elsewhere_and_releases_its_own() {
        let (db, owner, ids) = setup(2);
        let (_, hash) = db.load_profile(owner).unwrap().unwrap();
        let stage = crate::db::score_stage(ScoreKey {
            user_id: owner,
            profile_hash: &hash,
            backend: "fake",
            model: "sonnet",
            prompt_version: crate::prompt::score::PROMPT_VERSION,
        });
        let key = crate::db::ClaimKey {
            stage: &stage,
            backend: "fake",
            model: "sonnet",
        };
        let other = db
            .claim(key, &[ids[0]], now(), chrono::Duration::minutes(10))
            .unwrap();
        let llm = FakeLlm::new([ok(&[(ids[1], 50)])]);
        let summary = run(&db, owner, &llm, &mut quota(10), 5).await;
        assert_eq!((summary.scored, summary.calls), (1, 1));
        assert_eq!(db.query_i64("SELECT count(*) FROM work_claims").unwrap(), 1);
        drop(other);
    }
}
