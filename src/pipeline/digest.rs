//! 要約ステージ：digest の無い記事を数件ずつ LLM に渡し、応答を検証して成果物として保存する。
//! 呼び出しの前にクォータを確かめ、上限に達したら残りは次回に回す。

use chrono::{DateTime, Utc};

use super::Cancel;
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::{Db, DbError};
use crate::llm::Llm;
use crate::quota::{Quota, Stop};

pub const STAGE: &str = "digest";

#[derive(Debug, thiserror::Error)]
pub enum DigestStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

/// ステージを途中で止めた理由。
#[derive(Debug, Clone, PartialEq)]
pub enum Halt {
    /// クォータの判定で止めた（正常。残りは次回）
    Quota(Stop),
    /// サブスクリプションの上限に達した（記事の失敗としては数えない）
    UsageLimit { resets_at: Option<i64> },
    /// 認証切れなど記事によらない失敗の可能性があるので、失敗を広げないよう止めた
    LlmFailed(String),
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
    _db: &Db,
    _llm: &L,
    _quota: &mut Quota,
    _llm_cfg: &LlmConfig,
    _pipeline_cfg: &PipelineConfig,
    _now: DateTime<Utc>,
    _cancel: &Cancel,
) -> Result<DigestSummary, DigestStageError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{ContentKind, ContentOrigin, NewArticle};
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse, RateLimit, Window};
    use crate::quota::QuotaConfig;

    fn now() -> DateTime<Utc> {
        // JST 11:00（10〜15 時の枠）
        DateTime::parse_from_rfc3339("2026-09-28T02:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn llm_cfg(batch: usize) -> LlmConfig {
        LlmConfig {
            digest_batch_size: batch,
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
            "points_ja": ["点"], "implications_ja": "", "lwr_relevant": true, "topics": ["規制"],
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
            db,
            llm,
            quota,
            &llm_cfg(batch),
            &PipelineConfig::default(),
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap()
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
        assert_eq!(reqs[0].schema, crate::digest::schema());
        assert_eq!(reqs[0].system, crate::digest::system_prompt());
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
            ["fake|sonnet|1"]
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

    #[tokio::test]
    async fn usage_limit_halts_without_blaming_articles() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 2);
        let llm = FakeLlm::new([Err(LlmError::RateLimited {
            resets_at: Some(1790457000),
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

    #[tokio::test]
    async fn stops_when_cancelled() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 1);
        let cancel = Cancel::default();
        cancel.request();
        let summary = digest_articles(
            &db,
            &FakeLlm::new([]),
            &mut quota(10),
            &llm_cfg(5),
            &PipelineConfig::default(),
            now(),
            &cancel,
        )
        .await
        .unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.calls, 0);
    }
}
