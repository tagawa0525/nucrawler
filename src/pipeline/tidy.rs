//! 語彙の整理ステージ：要約で LLM が提案して増えた語の表記揺れを、LLM に既存の語へ統合させる。
//! 前回の整理から `tidy_interval_days` 日たったときだけ、1 回呼び出す。

use chrono::{DateTime, Utc};

use super::Halt;
use super::llm_call::{Call, LlmStage, Outcome, Reserved, Shared, call_recorded, permit, reserve};
use crate::config::LlmConfig;
use crate::db::DbError;
use crate::errors;
use crate::llm::{Llm, LlmRequest};
use crate::prompt;

pub const STAGE: &str = "tidy";

#[derive(Debug, thiserror::Error)]
pub enum TidyStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

#[derive(Debug, Default, PartialEq)]
pub struct TidySummary {
    pub merged: usize,
    pub calls: usize,
    pub halted: Option<Halt>,
    pub cancelled: bool,
}

/// `force` なら前回の整理からの間隔によらず整理する（`crawl --only tidy`）。
pub async fn tidy_topics<L: Llm>(
    LlmStage {
        db,
        llm,
        quota,
        cancel,
        clock,
    }: LlmStage<'_, L>,
    cfg: &LlmConfig,
    force: bool,
    now: DateTime<Utc>,
) -> Result<TidySummary, TidyStageError> {
    let shared = Shared::new(quota);
    let mut summary = TidySummary::default();
    let interval = chrono::Duration::days(i64::from(cfg.tidy_interval_days));
    if !force && db.llm_succeeded_since(STAGE, now - interval)? {
        return Ok(summary);
    }
    let usage = db.topic_usage()?;
    let proposed = usage.iter().filter(|u| u.added_at.is_some()).count();
    // LLM が足した語が無ければ、統合するものが無い
    if proposed == 0 {
        return Ok(summary);
    }
    // 呼び出しの枠を先に取り、判定と呼び出しをその中で行う
    let _slot = match reserve(llm, cancel).await {
        Reserved::Slot(slot) => slot,
        Reserved::Cancelled => {
            summary.cancelled = true;
            return Ok(summary);
        }
        Reserved::Failed(message) => {
            summary.halted = Some(Halt::LlmFailed(message));
            return Ok(summary);
        }
    };
    if let Err(stop) = permit(db, &shared, clock(), 0)? {
        tracing::info!("tidy stops: {stop}");
        summary.halted = Some(Halt::Quota(stop));
        return Ok(summary);
    }
    let prompt = prompt::tidy::build_prompt(&usage);
    let schema = prompt::tidy::schema(&usage);
    let outcome = call_recorded(
        db,
        llm,
        &shared,
        Call {
            stage: STAGE,
            n_items: proposed,
            req: LlmRequest {
                system: prompt::tidy::system_prompt(),
                prompt: &prompt,
                schema: &schema,
                model: &cfg.tidy_model,
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
    // 形の崩れた応答は捨てて、次の整理の機会を待つ（統合しなくても要約や検索は困らない）
    let merges = match prompt::tidy::parse(&response.output, &usage) {
        Ok(merges) => merges,
        Err(e) => {
            tracing::warn!("tidy output rejected: {}", errors::error_chain(&e));
            return Ok(summary);
        }
    };
    db.merge_topics(&merges, llm.backend(), &cfg.tidy_model, now)?;
    summary.merged = merges.len();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact};
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse};
    use crate::pipeline::Cancel;
    use crate::quota::{Quota, QuotaConfig, Stop};

    fn now() -> DateTime<Utc> {
        // JST 11:00（10〜15 時の枠）
        DateTime::parse_from_rfc3339("2026-10-04T02:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn quota(max_calls: u32) -> Quota {
        Quota::new(QuotaConfig::default(), None, Some(max_calls))
    }

    /// 新しい語を提案した要約を保存する。
    fn propose(db: &Db, name: &str) {
        let url = format!("https://e.com/{name}");
        let a = db
            .insert_article(&NewArticle {
                source_id: "wnn",
                url: &url,
                title: "t",
                lang: crate::config::Lang::En,
                published_at: None,
            })
            .unwrap()
            .unwrap();
        let c = db
            .insert_content(a, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Digest,
                backend: "fake",
                model: "sonnet",
                prompt_version: 2,
                payload: &serde_json::json!({
                    "title_ja": "題", "summary_ja": "要約",
                    "topics": [name], "new_topics": [{"name": name, "facet": "分野"}],
                }),
                inputs: &[c],
                glossary_at: None,
            },
            now() - chrono::Duration::days(1),
        )
        .unwrap();
    }

    fn merges(pairs: &[(&str, &str)]) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"merges": pairs
                .iter()
                .map(|(from, into)| serde_json::json!({"from": from, "into": into, "reason": "同じ意味"}))
                .collect::<Vec<_>>()}),
            rate_limit: None,
        })
    }

    async fn run(db: &Db, llm: &FakeLlm, quota: &mut Quota, force: bool) -> TidySummary {
        tidy_topics(
            LlmStage {
                db,
                llm,
                quota,
                cancel: &Cancel::default(),
                clock: &now,
            },
            &LlmConfig::default(),
            force,
            now(),
        )
        .await
        .unwrap()
    }

    fn topic_names(db: &Db) -> Vec<String> {
        db.topics().unwrap().into_iter().map(|t| t.name).collect()
    }

    #[tokio::test]
    async fn merges_proposed_topics_and_records_the_call() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        propose(&db, "データセンター需要");
        let before = db.topic_usage().unwrap();
        let llm = FakeLlm::new([merges(&[("新設炉", "新設・建設")])]);
        let summary = run(&db, &llm, &mut quota(10), false).await;
        assert_eq!(
            summary,
            TidySummary {
                merged: 1,
                calls: 1,
                halted: None,
                cancelled: false
            }
        );
        let reqs = llm.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].model, "sonnet");
        assert_eq!(reqs[0].system, crate::prompt::tidy::system_prompt());
        assert_eq!(reqs[0].prompt, crate::prompt::tidy::build_prompt(&before));
        assert_eq!(reqs[0].schema, crate::prompt::tidy::schema(&before));
        let names = topic_names(&db);
        assert!(!names.contains(&"新設炉".to_string()));
        assert!(names.contains(&"データセンター需要".to_string()));
        assert_eq!(
            db.query_strings("SELECT alias || '|' || backend || '|' || model FROM topic_aliases")
                .unwrap(),
            ["新設炉|fake|sonnet"]
        );
        assert_eq!(
            db.query_strings("SELECT stage || '|' || n_items || '|' || ok FROM llm_calls")
                .unwrap(),
            ["tidy|2|1"]
        );
    }

    /// 前回の整理から間隔がたっていなければ呼ばない。`force` なら間隔によらず整理する。
    #[tokio::test]
    async fn waits_for_the_interval_unless_forced() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        let llm = FakeLlm::new([merges(&[]), merges(&[]), merges(&[])]);
        let mut q = quota(10);
        run(&db, &llm, &mut q, false).await;
        assert_eq!(llm.requests().len(), 1);
        // 同じ時刻にもう一度：間隔（7 日）がたっていない
        let summary = run(&db, &llm, &mut q, false).await;
        assert_eq!(summary, TidySummary::default());
        assert_eq!(llm.requests().len(), 1);
        run(&db, &llm, &mut q, true).await;
        assert_eq!(llm.requests().len(), 2);
    }

    /// LLM が足した語が無ければ、統合するものが無いので呼ばない。
    #[tokio::test]
    async fn skips_without_proposed_topics() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([]);
        let summary = run(&db, &llm, &mut quota(10), true).await;
        assert_eq!(summary, TidySummary::default());
        assert!(llm.requests().is_empty());
    }

    #[tokio::test]
    async fn stops_at_the_quota() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        let llm = FakeLlm::new([]);
        let summary = run(&db, &llm, &mut quota(0), true).await;
        assert_eq!(
            summary.halted,
            Some(Halt::Quota(Stop::MaxCalls { limit: 0 }))
        );
        assert!(llm.requests().is_empty());
    }

    /// 応答の形が崩れていたら何も統合しない（次の整理の機会を待つ）。
    #[tokio::test]
    async fn malformed_output_merges_nothing() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        let llm = FakeLlm::new([Ok(LlmResponse {
            output: serde_json::json!({"unexpected": true}),
            rate_limit: None,
        })]);
        let summary = run(&db, &llm, &mut quota(10), true).await;
        assert_eq!(summary.merged, 0);
        assert_eq!(summary.calls, 1);
        assert!(topic_names(&db).contains(&"新設炉".to_string()));
    }
}
