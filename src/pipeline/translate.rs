//! 和訳ステージ：依頼された記事と、点数の高い英語記事の本文を 1 件ずつ全文和訳する。

use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use super::llm_call::{Call, LlmStage, Outcome, call_recorded, record_failures};
use super::{Halt, Target};
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::{
    ArtifactKind, Db, DbError, NewArtifact, RedoKey, StageKey, TranslateInput, TranslateQuery,
};
use crate::llm::{Llm, LlmRequest};
use crate::prompt;
use crate::{errors, glossary};

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

/// 通常は依頼と先回りの対象を、`requests_only` なら依頼だけを、`Redo` なら条件に合う記事を和訳する。
pub async fn translate_articles<L: Llm>(
    LlmStage {
        db,
        llm,
        quota,
        cancel,
    }: LlmStage<'_, L>,
    llm_cfg: &LlmConfig,
    pipeline_cfg: &PipelineConfig,
    user_id: i64,
    target: &Target,
    now: DateTime<Utc>,
) -> Result<TranslateSummary, TranslateStageError> {
    let backend = llm.backend();
    let model = llm_cfg.translate_model.as_str();
    let profile_hash = db.profile_hash(user_id)?;
    let query = TranslateQuery {
        user_id,
        profile_hash: profile_hash.as_deref(),
        min_score: llm_cfg.translate_min_score,
        requests_only: matches!(
            target,
            Target::Pending {
                requests_only: true
            }
        ),
        backend,
        model,
    };
    let cutoff = now - chrono::Duration::days(i64::from(pipeline_cfg.backlog_days));
    let schema = prompt::translate::schema();
    let mut summary = TranslateSummary::default();
    // 訳語集の変更による作り直しは、先に対象を決めて順に訳す
    let mut outdated = match target {
        Target::Redo(spec) if spec.glossary => Some(outdated_translations(
            db,
            RedoKey {
                user_id: spec.user_id,
                profile_hash: spec.profile_hash.as_deref(),
                backend,
                model,
                prompt_version: prompt::translate::PROMPT_VERSION,
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
        if let Err(stop) = quota.permit(now) {
            tracing::info!("translate stops: {stop}");
            summary.halted = Some(Halt::Quota(stop));
            break;
        }
        // 全文は長いので 1 件ずつ訳す
        let Some(input) = (match (&mut outdated, target) {
            (Some(queue), _) => queue.pop_front().into_iter().collect(),
            (None, Target::Pending { .. }) => db.pending_translate(query, cutoff, now, 1)?,
            (None, Target::Redo(spec)) => db.redo_translate(
                RedoKey {
                    user_id: spec.user_id,
                    profile_hash: spec.profile_hash.as_deref(),
                    backend,
                    model,
                    prompt_version: prompt::translate::PROMPT_VERSION,
                },
                &spec.filter,
                now,
                1,
            )?,
        })
        .into_iter()
        .next() else {
            break;
        };
        let key = StageKey {
            article_id: input.article_id,
            stage: STAGE,
            backend,
            model,
        };
        let prompt = prompt::translate::build_prompt(&input, llm_cfg.translate_max_input_chars);
        let relevant = glossary::relevant(&db.glossary_entries()?, &prompt);
        let system = prompt::translate::system_prompt(&relevant.terms);
        let outcome = call_recorded(
            db,
            llm,
            quota,
            Call {
                stage: STAGE,
                n_items: 1,
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
                    summary.failed += record_failures(db, std::iter::once(key), message, now)?;
                }
                summary.halted = Some(halt);
                break;
            }
        };
        let body_ja = match prompt::translate::parse(&response.output) {
            Ok(body_ja) => body_ja,
            Err(e) => {
                let message = errors::error_chain(&e);
                tracing::warn!(
                    article_id = input.article_id,
                    "translation rejected: {message}"
                );
                summary.failed += record_failures(db, std::iter::once(key), &message, now)?;
                continue;
            }
        };
        let inputs: Vec<i64> = input.contents.iter().map(|c| c.id).collect();
        // 保存と依頼の完了は同じトランザクションで行う
        db.insert_translation(
            &NewArtifact {
                article_id: input.article_id,
                kind: ArtifactKind::Translation,
                backend,
                model,
                prompt_version: prompt::translate::PROMPT_VERSION,
                payload: &serde_json::json!({ "body_ja": body_ja }),
                inputs: &inputs,
                glossary_at: relevant.glossary_at.as_deref(),
            },
            now,
        )?;
        db.clear_stage_failure(key)?;
        summary.translated += 1;
    }
    Ok(summary)
}

/// このモデルの最新の和訳が、記事に当たる訳語の変更より前に作られた記事。時点は訳すときと同じ
/// プロンプトで決める（切り詰めた本文の外の語で作り直しを繰り返さないように）。
fn outdated_translations(
    db: &Db,
    key: RedoKey,
    filter: &crate::db::RedoFilter,
    llm_cfg: &LlmConfig,
    now: DateTime<Utc>,
) -> Result<VecDeque<TranslateInput>, DbError> {
    let entries = db.glossary_entries()?;
    Ok(db
        .redo_translate_existing(key, filter, now)?
        .into_iter()
        .filter(|(input, made_with)| {
            let prompt = prompt::translate::build_prompt(input, llm_cfg.translate_max_input_chars);
            glossary::relevant(&entries, &prompt).glossary_at > *made_with
        })
        .map(|(input, _)| input)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{
        ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact, ScoreKey,
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
                    glossary_at: None,
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
            LlmStage {
                db,
                llm,
                quota,
                cancel: &Cancel::default(),
            },
            &LlmConfig::default(),
            &PipelineConfig::default(),
            owner,
            &Target::Pending { requests_only },
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

    /// 和訳には、記事に当たった訳語の時点を残す。当たる語が無ければ残さない。
    #[tokio::test]
    async fn translation_records_the_glossary_time() {
        let (db, owner) = setup();
        let with_edg = article(&db, 0, 10);
        let without = article(&db, 1, 10);
        mention_edg(&db, with_edg);
        let term = crate::glossary::Term {
            sources: vec!["emergency diesel generator".into()],
            target: "非常用ディーゼル発電機".into(),
            abbr: None,
            note: None,
        };
        db.add_glossary_term(&term, now()).unwrap();
        for id in [with_edg, without] {
            db.request_translation(owner, id, now()).unwrap();
        }
        let llm = FakeLlm::new([ok("和訳"), ok("和訳")]);
        run(&db, owner, &llm, &mut quota(10), true).await;
        assert_eq!(
            db.query_strings(
                "SELECT article_id || '|' || coalesce(glossary_at, '-') FROM artifacts
                 WHERE kind = 'translation' ORDER BY article_id"
            )
            .unwrap(),
            [
                format!("{with_edg}|{}", crate::db::timestamp(now())),
                format!("{without}|-")
            ]
        );
    }

    #[tokio::test]
    async fn system_prompt_carries_only_glossary_terms_in_the_articles() {
        let (db, owner) = setup();
        let requested = article(&db, 0, 10);
        db.request_translation(owner, requested, now()).unwrap();
        mention_edg(&db, requested);
        add_edg_term(&db);
        let llm = FakeLlm::new([ok("和訳")]);
        run(&db, owner, &llm, &mut quota(10), true).await;
        let system = &llm.requests()[0].system;
        assert!(system.contains(EDG_LINE), "{system}");
        // 記事に出てこない語は載せない
        assert!(!system.contains("refueling outage"), "{system}");
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
        assert_eq!(reqs[0].system, crate::prompt::translate::system_prompt(&[]));
        assert_eq!(reqs[0].schema, crate::prompt::translate::schema());
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
    async fn requests_only_translates_requested_articles() {
        let (db, owner) = setup();
        let _high = article(&db, 0, 99);
        let requested = article(&db, 1, 10);
        db.request_translation(owner, requested, now()).unwrap();
        let llm = FakeLlm::new([ok("依頼の和訳")]);
        let summary = run(&db, owner, &llm, &mut quota(10), true).await;
        assert_eq!((summary.translated, summary.calls), (1, 1));
        assert!(llm.requests()[0].prompt.contains("Body 1"));
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

    /// 訳語集が変わった後に作られていない和訳だけを、同じモデルで新しい版として作り直す。
    /// 作り直した版は時点が新しいので、もう一度実行しても対象にならない。
    #[tokio::test]
    async fn redo_glossary_retranslates_only_outdated_translations() {
        let (db, owner) = setup();
        let with_edg = article(&db, 0, 90);
        let without = article(&db, 1, 90);
        mention_edg(&db, with_edg);
        run(
            &db,
            owner,
            &FakeLlm::new([ok("初訳"), ok("初訳")]),
            &mut quota(10),
            false,
        )
        .await;
        let later = now() + chrono::Duration::hours(1);
        let term = crate::glossary::Term {
            sources: vec!["emergency diesel generator".into()],
            target: "非常用ディーゼル発電機".into(),
            abbr: None,
            note: None,
        };
        db.add_glossary_term(&term, later).unwrap();
        let llm = FakeLlm::new([ok("再訳")]);
        let summary = redo_glossary(&db, owner, &llm, later).await;
        assert_eq!((summary.translated, summary.calls), (1, 1));
        assert!(llm.requests()[0].system.contains("非常用ディーゼル発電機"));
        assert_eq!(
            db.query_strings(
                "SELECT article_id || '|' || json_extract(payload, '$.body_ja') || '|'
                        || coalesce(glossary_at, '-')
                 FROM artifacts WHERE kind = 'translation' ORDER BY id"
            )
            .unwrap(),
            [
                format!("{with_edg}|初訳|-"),
                format!("{without}|初訳|-"),
                format!("{with_edg}|再訳|{}", crate::db::timestamp(later)),
            ]
        );
        let again = redo_glossary(&db, owner, &FakeLlm::new([]), later).await;
        assert_eq!(again.calls, 0);
    }

    async fn redo_glossary(
        db: &Db,
        owner: i64,
        llm: &FakeLlm,
        now: DateTime<Utc>,
    ) -> TranslateSummary {
        let target = Target::Redo(crate::pipeline::RedoSpec {
            filter: crate::db::RedoFilter::default(),
            user_id: owner,
            profile_hash: None,
            glossary: true,
        });
        translate_articles(
            LlmStage {
                db,
                llm,
                quota: &mut quota(10),
                cancel: &Cancel::default(),
            },
            &LlmConfig::default(),
            &PipelineConfig::default(),
            owner,
            &target,
            now,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn redo_translates_again_with_another_model() {
        let (db, owner) = setup();
        let a = article(&db, 0, 90);
        run(
            &db,
            owner,
            &FakeLlm::new([ok("初訳")]),
            &mut quota(10),
            false,
        )
        .await;
        let target = Target::Redo(crate::pipeline::RedoSpec {
            filter: crate::db::RedoFilter::default(),
            user_id: owner,
            profile_hash: None,
            glossary: false,
        });
        let opus = LlmConfig {
            translate_model: "opus".into(),
            ..LlmConfig::default()
        };
        let llm = FakeLlm::new([ok("再訳")]);
        let summary = translate_articles(
            LlmStage {
                db: &db,
                llm: &llm,
                quota: &mut quota(10),
                cancel: &Cancel::default(),
            },
            &opus,
            &PipelineConfig::default(),
            owner,
            &target,
            now(),
        )
        .await
        .unwrap();
        assert_eq!((summary.translated, summary.calls), (1, 1));
        assert_eq!(llm.requests()[0].model, "opus");
        assert_eq!(
            db.query_strings(&format!(
                "SELECT model FROM artifacts WHERE article_id = {a} AND kind = 'translation' ORDER BY id"
            ))
            .unwrap(),
            ["sonnet", "opus"]
        );
    }

    #[tokio::test]
    async fn stops_when_cancelled() {
        let (db, owner) = setup();
        article(&db, 0, 90);
        let cancel = Cancel::default();
        cancel.request();
        let summary = translate_articles(
            LlmStage {
                db: &db,
                llm: &FakeLlm::new([]),
                quota: &mut quota(10),
                cancel: &cancel,
            },
            &LlmConfig::default(),
            &PipelineConfig::default(),
            owner,
            &Target::Pending {
                requests_only: false,
            },
            now(),
        )
        .await
        .unwrap();
        assert!(summary.cancelled);
    }
}
