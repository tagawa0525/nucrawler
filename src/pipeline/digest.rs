//! 要約ステージ：digest の無い記事を数件ずつ LLM に渡し、応答を検証して成果物として保存する。
//! 呼び出しの前にクォータを確かめ、上限に達したら残りは次回に回す。

use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use super::llm_call::{
    Call, LlmStage, MISSING, Outcome, call_recorded, claim_ttl, record_failures,
};
use super::{Halt, Target};
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::{ArtifactKind, ClaimKey, Db, DbError, DigestInput, NewArtifact, RedoKey, StageKey};
use crate::llm::{Llm, LlmRequest};
use crate::prompt;
use crate::{errors, glossary};

pub const STAGE: &str = "digest";

#[derive(Debug, thiserror::Error)]
pub enum DigestStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

#[derive(Debug, Default, PartialEq)]
pub struct DigestSummary {
    pub digested: usize,
    /// 再試行に回した記事の数
    pub failed: usize,
    pub calls: usize,
    pub halted: Option<Halt>,
    pub cancelled: bool,
}

pub async fn digest_articles<L: Llm>(
    LlmStage {
        db,
        llm,
        quota,
        cancel,
        clock,
    }: LlmStage<'_, L>,
    llm_cfg: &LlmConfig,
    pipeline_cfg: &PipelineConfig,
    target: &Target,
    now: DateTime<Utc>,
) -> Result<DigestSummary, DigestStageError> {
    let backend = llm.backend();
    let model = llm_cfg.digest_model.as_str();
    let cutoff = now - chrono::Duration::days(i64::from(pipeline_cfg.backlog_days));
    let mut summary = DigestSummary::default();
    // 訳語集の変更による作り直しは、先に対象を決めてバッチに分けて要約する
    let mut outdated = match target {
        Target::Redo(spec) if spec.glossary => Some(outdated_digests(
            db,
            RedoKey {
                user_id: spec.user_id,
                profile_hash: spec.profile_hash.as_deref(),
                backend,
                model,
                prompt_version: prompt::digest::PROMPT_VERSION,
            },
            &spec.filter,
            llm_cfg,
            now,
        )?),
        _ => None,
    };
    loop {
        if cancel.is_requested() {
            summary.cancelled = true;
            break;
        }
        // 採点のための回数を残して止める（要約待ちが多くても推薦が止まらないように）
        if let Err(stop) = quota.permit_reserving(now, llm_cfg.score_reserved_calls) {
            tracing::info!("digest stops: {stop}");
            summary.halted = Some(Halt::Quota(stop));
            break;
        }
        let claim_key = ClaimKey {
            stage: STAGE,
            backend,
            model,
        };
        // 予約は処理を終える（この周の終わりで drop する）まで持つ
        let (batch, _claim) = match (&mut outdated, target) {
            (Some(queue), _) => {
                let n = llm_cfg.digest_batch_size.min(queue.len());
                let items: Vec<DigestInput> = queue.drain(..n).collect();
                let ids: Vec<i64> = items.iter().map(|b| b.article_id).collect();
                let claim = db.claim(claim_key, &ids, clock(), claim_ttl(llm_cfg))?;
                let items = items
                    .into_iter()
                    .filter(|b| claim.ids().contains(&b.article_id))
                    .collect();
                (items, claim)
            }
            (None, Target::Pending { .. }) => db.claim_selected(
                claim_key,
                clock(),
                claim_ttl(llm_cfg),
                |db| db.pending_digest(cutoff, now, backend, model, llm_cfg.digest_batch_size),
                |b| b.article_id,
            )?,
            (None, Target::Redo(spec)) => db.claim_selected(
                claim_key,
                clock(),
                claim_ttl(llm_cfg),
                |db| {
                    db.redo_digest(
                        RedoKey {
                            user_id: spec.user_id,
                            profile_hash: spec.profile_hash.as_deref(),
                            backend,
                            model,
                            prompt_version: prompt::digest::PROMPT_VERSION,
                        },
                        &spec.filter,
                        now,
                        llm_cfg.digest_batch_size,
                    )
                },
                |b| b.article_id,
            )?,
        };
        if batch.is_empty() {
            // 先に決めた作り直しの対象がほかの実行に予約されていたら、残りに進む
            if outdated.as_ref().is_some_and(|queue| !queue.is_empty()) {
                continue;
            }
            break;
        }
        let ids: Vec<i64> = batch.iter().map(|b| b.article_id).collect();
        let prompt = prompt::digest::build_prompt(&batch, llm_cfg.max_input_chars);
        // 前のバッチで提案された語も選べるよう、語彙はバッチごとに読み直す
        let vocab = db.topics()?;
        let entries = db.glossary_entries()?;
        let system =
            prompt::digest::system_prompt(&vocab, &glossary::relevant(&entries, &prompt).terms);
        let schema = prompt::digest::schema(&vocab);
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
        let key = |article_id| StageKey {
            article_id,
            stage: STAGE,
            backend,
            model,
        };
        let response = match outcome {
            Outcome::Response(response) => response,
            Outcome::Cancelled => {
                summary.cancelled = true;
                break;
            }
            Outcome::Halted(halt) => {
                if let Halt::LlmFailed(message) = &halt {
                    summary.failed +=
                        record_failures(db, ids.iter().map(|&id| key(id)), message, now)?;
                }
                summary.halted = Some(halt);
                break;
            }
        };
        let parsed = match prompt::digest::parse(&response.output, &ids, &vocab) {
            Ok(parsed) => parsed,
            Err(e) => {
                let message = errors::error_chain(&e);
                tracing::warn!("digest output rejected: {message}");
                summary.failed +=
                    record_failures(db, ids.iter().map(|&id| key(id)), &message, now)?;
                continue;
            }
        };
        for (id, payload) in &parsed.items {
            let input = batch.iter().find(|b| b.article_id == *id);
            let inputs: Vec<i64> = input
                .map(|b| b.contents.iter().map(|c| c.id).collect())
                .unwrap_or_default();
            // 時点はバッチ全体ではなく、その記事の部分に当たった訳語から決める
            let glossary_at = input.and_then(|b| {
                let own =
                    prompt::digest::build_prompt(std::slice::from_ref(b), llm_cfg.max_input_chars);
                glossary::relevant(&entries, &own).glossary_at
            });
            db.insert_artifact(
                &NewArtifact {
                    article_id: *id,
                    kind: ArtifactKind::Digest,
                    backend,
                    model,
                    prompt_version: prompt::digest::PROMPT_VERSION,
                    payload,
                    inputs: &inputs,
                    glossary_at: glossary_at.as_deref(),
                },
                now,
            )?;
            db.clear_stage_failure(key(*id))?;
            summary.digested += 1;
        }
        summary.failed +=
            record_failures(db, parsed.missing.iter().map(|&id| key(id)), MISSING, now)?;
    }
    Ok(summary)
}

/// このモデルの最新の要約が、記事に当たる訳語の変更より前に作られた記事。時点は要約するときと
/// 同じく、その記事の部分のプロンプトで決める（切り詰めた本文の外の語で作り直しを繰り返さないように）。
fn outdated_digests(
    db: &Db,
    key: RedoKey,
    filter: &crate::db::RedoFilter,
    llm_cfg: &LlmConfig,
    now: DateTime<Utc>,
) -> Result<VecDeque<DigestInput>, DbError> {
    let entries = db.glossary_entries()?;
    Ok(db
        .redo_digest_existing(key, filter, now)?
        .into_iter()
        .filter(|(input, made_with)| {
            let own =
                prompt::digest::build_prompt(std::slice::from_ref(input), llm_cfg.max_input_chars);
            glossary::relevant(&entries, &own).glossary_at > *made_with
        })
        .map(|(input, _)| input)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{ContentKind, ContentOrigin, Db, NewArticle};
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse, RateLimit, Window};
    use crate::pipeline::Cancel;
    use crate::quota::{Quota, QuotaConfig, Stop};

    fn now() -> DateTime<Utc> {
        // JST 11:00（10〜15 時の枠）
        DateTime::parse_from_rfc3339("2026-09-28T02:00:00Z")
            .unwrap()
            .to_utc()
    }

    /// 採点のための予約は `leaves_reserved_calls_for_scoring` で確かめるので、ほかのテストでは 0 にする。
    fn llm_cfg(batch: usize) -> LlmConfig {
        LlmConfig {
            digest_batch_size: batch,
            score_reserved_calls: 0,
            ..LlmConfig::default()
        }
    }

    fn quota(max_calls: u32) -> Quota {
        Quota::new(QuotaConfig::default(), None, Some(max_calls))
    }

    /// 本文つきの記事を、新しい順に `n` 件登録して id を返す。
    fn articles(db: &Db, n: usize) -> Vec<i64> {
        (0..n)
            .map(|i| {
                let published = format!("2026-09-27T{:02}:00:00.000Z", 20 - i);
                let url = format!("https://e.com/{i}");
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
                db.insert_content(id, ContentKind::Body, ContentOrigin::Page, "body")
                    .unwrap();
                id
            })
            .collect()
    }

    fn item(id: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "title_ja": format!("題{id}"), "summary_ja": "要約",
            "points_ja": ["点"], "implications_ja": "", "lwr_relevant": true,
            "topics": ["規制・審査"], "new_topics": [],
        })
    }

    fn ok(ids: &[i64], five_hour: f64) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"items": ids.iter().map(|&id| item(id)).collect::<Vec<_>>()}),
            rate_limit: Some(RateLimit {
                five_hour: Some(Window {
                    utilization: five_hour,
                    resets_at: now().timestamp() + 3600,
                }),
                seven_day: None,
            }),
        })
    }

    async fn run(db: &Db, llm: &FakeLlm, quota: &mut Quota, batch: usize) -> DigestSummary {
        digest_articles(
            LlmStage {
                db,
                llm,
                quota,
                cancel: &Cancel::default(),
                clock: &now,
            },
            &llm_cfg(batch),
            &PipelineConfig::default(),
            &Target::Pending {
                requests_only: false,
            },
            now(),
        )
        .await
        .unwrap()
    }

    fn mention_edg(db: &Db, article_id: i64) {
        db.insert_content(
            article_id,
            ContentKind::Body,
            ContentOrigin::Page,
            "The emergency diesel\ngenerators (EDGs) started.",
        )
        .unwrap();
    }

    /// DB に加えた訳語は、記事に原語が出てくれば次の呼び出しから system prompt に載る。
    fn add_edg_term(db: &Db) {
        db.conn()
            .execute_batch(
                "INSERT INTO glossary_terms (target, abbr) VALUES ('非常用ディーゼル発電機', 'EDG');
                 INSERT INTO glossary_sources (term_id, source)
                 SELECT id, 'emergency diesel generator' FROM glossary_terms WHERE abbr = 'EDG';
                 INSERT INTO glossary_sources (term_id, source)
                 SELECT id, 'EDG' FROM glossary_terms WHERE abbr = 'EDG';",
            )
            .unwrap();
    }

    const EDG_LINE: &str = "emergency diesel generator / EDG → 非常用ディーゼル発電機（EDG）";

    /// 要約には、バッチ全体ではなくその記事に当たった訳語の時点を残す。
    #[tokio::test]
    async fn digests_record_the_glossary_time_of_each_article() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        mention_edg(&db, ids[0]);
        let term = crate::glossary::Term {
            sources: vec!["emergency diesel generator".into()],
            target: "非常用ディーゼル発電機".into(),
            abbr: None,
            note: None,
        };
        db.add_glossary_term(&term, now()).unwrap();
        let llm = FakeLlm::new([ok(&ids, 0.1)]);
        run(&db, &llm, &mut quota(10), 2).await;
        assert_eq!(
            db.query_strings(
                "SELECT article_id || '|' || coalesce(glossary_at, '-') FROM artifacts
                 WHERE kind = 'digest' ORDER BY article_id"
            )
            .unwrap(),
            [
                format!("{}|{}", ids[0], crate::db::timestamp(now())),
                format!("{}|-", ids[1])
            ]
        );
    }

    #[tokio::test]
    async fn system_prompt_carries_only_glossary_terms_in_the_articles() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 1);
        mention_edg(&db, ids[0]);
        add_edg_term(&db);
        let llm = FakeLlm::new([ok(&ids, 0.1)]);
        run(&db, &llm, &mut quota(10), 1).await;
        let system = &llm.requests()[0].system;
        assert!(system.contains(EDG_LINE), "{system}");
        // バッチのどの記事にも出てこない語は載せない
        assert!(!system.contains("refueling outage"), "{system}");
    }

    /// 提案された語は語彙に加わって要約に付き、次のバッチからは語彙として選べる。
    #[tokio::test]
    async fn proposed_topics_join_the_vocabulary_for_later_batches() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        let mut proposal = item(ids[0]);
        proposal["new_topics"] =
            serde_json::json!([{"name": "データセンター需要", "facet": "分野"}]);
        let first = Ok(LlmResponse {
            output: serde_json::json!({"items": [proposal]}),
            rate_limit: None,
        });
        let llm = FakeLlm::new([first, ok(&ids[1..], 0.1)]);
        let summary = run(&db, &llm, &mut quota(10), 1).await;
        assert_eq!(summary.digested, 2);
        let reqs = llm.requests();
        assert!(!reqs[0].system.contains("データセンター需要"));
        assert!(
            reqs[1].system.contains("データセンター需要"),
            "{}",
            reqs[1].system
        );
        assert_eq!(
            db.query_strings(
                "SELECT a.article_id || ':' || t.name FROM artifact_topics AS at
                 JOIN artifacts AS a ON a.id = at.artifact_id
                 JOIN topics AS t ON t.id = at.topic_id
                 ORDER BY a.article_id, t.id"
            )
            .unwrap(),
            [
                format!("{}:規制・審査", ids[0]),
                format!("{}:データセンター需要", ids[0]),
                format!("{}:規制・審査", ids[1]),
            ]
        );
    }

    #[tokio::test]
    async fn digests_in_batches_and_records_calls() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 3);
        let llm = FakeLlm::new([ok(&ids[..2], 0.1), ok(&ids[2..], 0.2)]);
        let summary = run(&db, &llm, &mut quota(10), 2).await;
        assert_eq!(
            summary,
            DigestSummary {
                digested: 3,
                failed: 0,
                calls: 2,
                halted: None,
                cancelled: false,
            }
        );
        let reqs = llm.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].model, "sonnet");
        let vocab = db.topics().unwrap();
        assert_eq!(reqs[0].schema, crate::prompt::digest::schema(&vocab));
        assert_eq!(
            reqs[0].system,
            crate::prompt::digest::system_prompt(&vocab, &[])
        );
        assert!(
            reqs[0]
                .prompt
                .contains(&format!("<article id=\"{}\"", ids[0]))
        );
        assert_eq!(
            db.query_strings("SELECT title_ja FROM artifacts ORDER BY article_id")
                .unwrap(),
            ["題1", "題2", "題3"]
        );
        assert_eq!(
            db.query_strings(
                "SELECT backend || '|' || model || '|' || prompt_version FROM artifacts LIMIT 1"
            )
            .unwrap(),
            ["fake|sonnet|3"]
        );
        assert_eq!(
            db.query_strings(
                "SELECT stage || '|' || n_items || '|' || ok FROM llm_calls ORDER BY id"
            )
            .unwrap(),
            ["digest|2|1", "digest|1|1"]
        );
    }

    #[tokio::test]
    async fn missing_items_are_retried_later() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        let llm = FakeLlm::new([ok(&ids[..1], 0.1)]);
        let summary = run(&db, &llm, &mut quota(10), 5).await;
        assert_eq!((summary.digested, summary.failed, summary.calls), (1, 1, 1));
        // 同じ時刻の再実行では、欠けた記事は再試行待ちなので呼び出さない
        let again = run(&db, &FakeLlm::new([]), &mut quota(10), 5).await;
        assert_eq!(again.calls, 0);
    }

    #[tokio::test]
    async fn stops_when_quota_says_so() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        let llm = FakeLlm::new([ok(&ids[..1], 0.1)]);
        let summary = run(&db, &llm, &mut quota(1), 1).await;
        assert_eq!(summary.calls, 1);
        assert_eq!(
            summary.halted,
            Some(Halt::Quota(Stop::MaxCalls { limit: 1 }))
        );
        // 応答の使用率で止まる場合（11 時の枠は 85%）
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        let llm = FakeLlm::new([ok(&ids[..1], 0.9)]);
        let summary = run(&db, &llm, &mut quota(10), 1).await;
        assert_eq!(summary.calls, 1);
        assert!(matches!(
            summary.halted,
            Some(Halt::Quota(Stop::FiveHour { .. }))
        ));
    }

    /// 採点のために残す回数に達したら、要約は止まる。
    #[tokio::test]
    async fn leaves_reserved_calls_for_scoring() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 3);
        let llm = FakeLlm::new([ok(&ids[..1], 0.1), ok(&ids[1..2], 0.1)]);
        let mut q = quota(3);
        let cfg = LlmConfig {
            score_reserved_calls: 1,
            ..llm_cfg(1)
        };
        let summary = digest_articles(
            LlmStage {
                db: &db,
                llm: &llm,
                quota: &mut q,
                cancel: &Cancel::default(),
                clock: &now,
            },
            &cfg,
            &PipelineConfig::default(),
            &Target::Pending {
                requests_only: false,
            },
            now(),
        )
        .await
        .unwrap();
        assert_eq!(summary.calls, 2);
        assert_eq!(
            summary.halted,
            Some(Halt::Quota(Stop::Reserved { reserved: 1 }))
        );
        assert!(q.permit(now()).is_ok(), "one call is left for scoring");
    }

    #[tokio::test]
    async fn usage_limit_halts_without_blaming_articles() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 2);
        let llm = FakeLlm::new([Err(LlmError::RateLimited {
            resets_at: Some(1790457000),
            rate_limit: Some(RateLimit {
                five_hour: Some(Window {
                    utilization: 1.0,
                    resets_at: 1790457000,
                }),
                seven_day: None,
            }),
        })]);
        let summary = run(&db, &llm, &mut quota(10), 1).await;
        assert_eq!(
            summary.halted,
            Some(Halt::UsageLimit {
                resets_at: Some(1790457000)
            })
        );
        assert_eq!(summary.failed, 0);
        assert_eq!(
            db.query_i64("SELECT count(*) FROM stage_errors").unwrap(),
            0
        );
        // 拒否されたときの使用率も記録し、次回の判定に使えるようにする
        assert_eq!(
            db.latest_rate_limit()
                .unwrap()
                .and_then(|r| r.five_hour)
                .map(|w| w.utilization),
            Some(1.0)
        );
        assert_eq!(
            db.query_strings("SELECT ok || '|' || error FROM llm_calls")
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn llm_failure_marks_batch_and_halts() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 4);
        let llm = FakeLlm::new([Err(LlmError::Reported {
            subtype: "error".into(),
            message: "Not logged in".into(),
        })]);
        let summary = run(&db, &llm, &mut quota(10), 2).await;
        assert_eq!(summary.calls, 1);
        assert_eq!(summary.failed, 2);
        assert!(matches!(&summary.halted, Some(Halt::LlmFailed(m)) if m.contains("Not logged in")));
        assert_eq!(
            db.query_i64("SELECT count(*) FROM stage_errors WHERE stage = 'digest'")
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn malformed_output_marks_batch_and_continues() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        let llm = FakeLlm::new([
            Ok(LlmResponse {
                output: serde_json::json!({"nope": 1}),
                rate_limit: None,
            }),
            ok(&ids[1..], 0.1),
        ]);
        let summary = run(&db, &llm, &mut quota(10), 1).await;
        assert_eq!((summary.digested, summary.failed, summary.calls), (1, 1, 2));
        assert_eq!(summary.halted, None);
    }

    /// 訳語集が変わった後に作られていない要約だけを、同じモデルで新しい版として作り直す。
    #[tokio::test]
    async fn redo_glossary_rebuilds_only_outdated_digests() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        mention_edg(&db, ids[0]);
        run(&db, &FakeLlm::new([ok(&ids, 0.1)]), &mut quota(10), 5).await;
        let later = now() + chrono::Duration::hours(1);
        let term = crate::glossary::Term {
            sources: vec!["EDG".into()],
            target: "非常用ディーゼル発電機".into(),
            abbr: Some("EDG".into()),
            note: None,
        };
        db.add_glossary_term(&term, later).unwrap();
        let llm = FakeLlm::new([ok(&ids[..1], 0.1)]);
        let summary = redo_glossary(&db, &llm, later).await;
        assert_eq!((summary.digested, summary.calls), (1, 1));
        let prompt = &llm.requests()[0].prompt;
        assert!(
            prompt.contains(&format!("<article id=\"{}\"", ids[0])),
            "{prompt}"
        );
        assert!(
            !prompt.contains(&format!("<article id=\"{}\"", ids[1])),
            "{prompt}"
        );
        assert_eq!(
            db.query_strings(&format!(
                "SELECT coalesce(glossary_at, '-') FROM artifacts
                 WHERE kind = 'digest' AND article_id = {} ORDER BY id",
                ids[0]
            ))
            .unwrap(),
            ["-".to_string(), crate::db::timestamp(later)]
        );
        let again = redo_glossary(&db, &FakeLlm::new([]), later).await;
        assert_eq!(again.calls, 0);
    }

    async fn redo_glossary(db: &Db, llm: &FakeLlm, now: DateTime<Utc>) -> DigestSummary {
        let target = Target::Redo(crate::pipeline::RedoSpec {
            filter: crate::db::RedoFilter::default(),
            user_id: db.owner_id().unwrap(),
            profile_hash: None,
            glossary: true,
        });
        digest_articles(
            LlmStage {
                db,
                llm,
                quota: &mut quota(10),
                cancel: &Cancel::default(),
                clock: &|| now,
            },
            &llm_cfg(5),
            &PipelineConfig::default(),
            &target,
            now,
        )
        .await
        .unwrap()
    }

    /// 別のモデルで作り直す。同じ条件で再実行しても、作り直した記事は対象にならない（続きから）。
    #[tokio::test]
    async fn redo_rebuilds_with_another_model_and_resumes() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        run(&db, &FakeLlm::new([ok(&ids, 0.1)]), &mut quota(10), 5).await;

        let target = Target::Redo(crate::pipeline::RedoSpec {
            filter: crate::db::RedoFilter {
                ids: vec![ids[1]],
                ..Default::default()
            },
            user_id: db.owner_id().unwrap(),
            profile_hash: None,
            glossary: false,
        });
        let opus = LlmConfig {
            digest_model: "opus".into(),
            ..llm_cfg(5)
        };
        let llm = FakeLlm::new([ok(&ids[1..], 0.1)]);
        let summary = digest_articles(
            LlmStage {
                db: &db,
                llm: &llm,
                quota: &mut quota(10),
                cancel: &Cancel::default(),
                clock: &now,
            },
            &opus,
            &PipelineConfig::default(),
            &target,
            now(),
        )
        .await
        .unwrap();
        assert_eq!((summary.digested, summary.calls), (1, 1));
        assert_eq!(llm.requests()[0].model, "opus");
        assert_eq!(
            db.query_strings(&format!(
                "SELECT model FROM artifacts WHERE article_id = {} ORDER BY id",
                ids[1]
            ))
            .unwrap(),
            ["sonnet", "opus"]
        );
        let again = digest_articles(
            LlmStage {
                db: &db,
                llm: &FakeLlm::new([]),
                quota: &mut quota(10),
                cancel: &Cancel::default(),
                clock: &now,
            },
            &opus,
            &PipelineConfig::default(),
            &target,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(again.calls, 0);
    }

    /// 応答を返さない LLM（中断されるまで待ち続ける呼び出し）。
    struct Hanging;

    impl Llm for Hanging {
        fn backend(&self) -> &'static str {
            "fake"
        }

        async fn call(&self, _: LlmRequest<'_>) -> Result<LlmResponse, LlmError> {
            std::future::pending().await
        }
    }

    /// 止める指示と同時に子プロセスが終了させられた（systemd が cgroup 全体に SIGTERM を送った）。
    struct KilledWithCancel(Cancel);

    impl Llm for KilledWithCancel {
        fn backend(&self) -> &'static str {
            "fake"
        }

        async fn call(&self, _: LlmRequest<'_>) -> Result<LlmResponse, LlmError> {
            self.0.request();
            Err(LlmError::Exit {
                status: "exit status: 143".into(),
                stderr: String::new(),
                interrupted: true,
            })
        }
    }

    fn assert_nothing_recorded(db: &Db) {
        assert_eq!(
            db.query_i64("SELECT count(*) FROM stage_errors").unwrap(),
            0
        );
        assert_eq!(db.query_i64("SELECT count(*) FROM llm_calls").unwrap(), 0);
    }

    /// 呼び出しの途中で止める指示が来たら、応答を待たずに止め、記事の失敗にも LLM の失敗にも
    /// 数えない（次回そのまま続きから処理する）。
    #[tokio::test]
    async fn cancel_during_a_call_stops_without_recording_failures() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 2);
        let cancel = Cancel::default();
        let requester = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            requester.request();
        });
        let summary = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            digest_articles(
                LlmStage {
                    db: &db,
                    llm: &Hanging,
                    quota: &mut quota(10),
                    cancel: &cancel,
                    clock: &now,
                },
                &llm_cfg(5),
                &PipelineConfig::default(),
                &Target::Pending {
                    requests_only: false,
                },
                now(),
            ),
        )
        .await
        .expect("the stage must stop without waiting for the response")
        .unwrap();
        assert!(summary.cancelled, "{summary:?}");
        assert_eq!((summary.failed, summary.halted), (0, None));
        assert_nothing_recorded(&db);
    }

    #[tokio::test]
    async fn llm_killed_by_the_stop_is_not_a_failure() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 2);
        let cancel = Cancel::default();
        let summary = digest_articles(
            LlmStage {
                db: &db,
                llm: &KilledWithCancel(cancel.clone()),
                quota: &mut quota(10),
                cancel: &cancel,
                clock: &now,
            },
            &llm_cfg(5),
            &PipelineConfig::default(),
            &Target::Pending {
                requests_only: false,
            },
            now(),
        )
        .await
        .unwrap();
        assert!(summary.cancelled, "{summary:?}");
        assert_eq!((summary.failed, summary.halted), (0, None));
        assert_nothing_recorded(&db);
    }

    /// 応答と止める指示が同時に届いたら、応答を捨てずに保存してから止める。
    struct AnswerWithCancel(Cancel);

    impl Llm for AnswerWithCancel {
        fn backend(&self) -> &'static str {
            "fake"
        }

        async fn call(&self, _: LlmRequest<'_>) -> Result<LlmResponse, LlmError> {
            self.0.request();
            ok(&[1, 2], 0.1)
        }
    }

    #[tokio::test]
    async fn response_arriving_with_cancel_is_kept() {
        // 同時に準備できたときの選び方が偶然に左右されないことを、繰り返して確かめる
        for _ in 0..20 {
            let db = Db::open_in_memory().unwrap();
            articles(&db, 2);
            let cancel = Cancel::default();
            let summary = digest_articles(
                LlmStage {
                    db: &db,
                    llm: &AnswerWithCancel(cancel.clone()),
                    quota: &mut quota(10),
                    cancel: &cancel,
                    clock: &now,
                },
                &llm_cfg(5),
                &PipelineConfig::default(),
                &Target::Pending {
                    requests_only: false,
                },
                now(),
            )
            .await
            .unwrap();
            assert_eq!(summary.digested, 2, "{summary:?}");
            assert!(summary.cancelled, "{summary:?}");
        }
    }

    #[tokio::test]
    async fn stops_when_cancelled() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 1);
        let cancel = Cancel::default();
        cancel.request();
        let summary = digest_articles(
            LlmStage {
                db: &db,
                llm: &FakeLlm::new([]),
                quota: &mut quota(10),
                cancel: &cancel,
                clock: &now,
            },
            &llm_cfg(5),
            &PipelineConfig::default(),
            &Target::Pending {
                requests_only: false,
            },
            now(),
        )
        .await
        .unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.calls, 0);
    }

    /// ほかの実行が予約している記事は飛ばし、自分の予約は処理を終えたら外す。
    #[tokio::test]
    async fn skips_articles_claimed_elsewhere_and_releases_its_own() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 3);
        let key = crate::db::ClaimKey {
            stage: "digest",
            backend: "fake",
            model: "sonnet",
        };
        let other = db
            .claim(key, &[ids[0]], now(), chrono::Duration::minutes(10))
            .unwrap();
        let llm = FakeLlm::new([ok(&ids[1..], 0.1)]);
        let summary = run(&db, &llm, &mut quota(10), 5).await;
        assert_eq!(summary.digested, 2);
        let prompt = &llm.requests()[0].prompt;
        assert!(
            !prompt.contains(&format!("<article id=\"{}\"", ids[0])),
            "{prompt}"
        );
        assert_eq!(db.query_i64("SELECT count(*) FROM work_claims").unwrap(), 1);
        drop(other);
    }
}
