//! 見出しの和訳ステージ：本文が取れず要約できない英語記事の見出しを、まとめて和訳する。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{Db, NewArticle};
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse};
    use crate::pipeline::{Cancel, Halt};
    use crate::quota::{Quota, QuotaConfig};

    fn now() -> DateTime<Utc> {
        // JST 11:00（10〜15 時の枠）
        DateTime::parse_from_rfc3339("2026-09-28T02:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn quota(max_calls: u32) -> Quota {
        Quota::new(QuotaConfig::default(), None, Some(max_calls))
    }

    /// 本文の無い英語記事を、新しい順に `n` 件登録して id を返す。
    fn articles(db: &Db, n: usize) -> Vec<i64> {
        (0..n)
            .map(|i| {
                let published = format!("2026-09-27T{:02}:00:00.000Z", 20 - i);
                let url = format!("https://e.com/{i}");
                db.insert_article(&NewArticle {
                    source_id: "iaea",
                    url: &url,
                    title: &format!("Title {i}"),
                    lang: Lang::En,
                    published_at: Some(&published),
                })
                .unwrap()
                .unwrap()
            })
            .collect()
    }

    fn ok(ids: &[i64]) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"items": ids
                .iter()
                .map(|&id| serde_json::json!({"id": id, "title_ja": format!("見出し{id}")}))
                .collect::<Vec<_>>()}),
            rate_limit: None,
        })
    }

    async fn run(db: &Db, llm: &FakeLlm, quota: &mut Quota, batch: usize) -> TitleSummary {
        translate_titles(
            LlmStage {
                db,
                llm,
                quota,
                cancel: &Cancel::default(),
            },
            &LlmConfig {
                title_batch_size: batch,
                ..LlmConfig::default()
            },
            now(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn translates_titles_in_batches_and_saves_them() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 3);
        let llm = FakeLlm::new([ok(&ids[..2]), ok(&ids[2..])]);
        let summary = run(&db, &llm, &mut quota(10), 2).await;
        assert_eq!(
            summary,
            TitleSummary {
                translated: 3,
                calls: 2,
                ..TitleSummary::default()
            }
        );
        let requests = llm.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].prompt.contains("Title 0"),
            "{}",
            requests[0].prompt
        );
        assert!(
            requests[0].prompt.contains("Title 1"),
            "{}",
            requests[0].prompt
        );
        assert_eq!(requests[0].model, "sonnet");
        assert_eq!(
            db.query_strings(
                "SELECT title_ja || '|' || backend || '|' || model || '|' || prompt_version
                 FROM artifacts WHERE kind = 'title' ORDER BY article_id"
            )
            .unwrap(),
            ids.iter()
                .map(|id| format!("見出し{id}|fake|sonnet|1"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            db.query_strings("SELECT stage || '|' || n_items FROM llm_calls ORDER BY id")
                .unwrap(),
            ["title|2", "title|1"]
        );
    }

    /// 応答に無かった記事は失敗として記録し、間隔を置いて再試行する（ほかの記事は保存する）。
    #[tokio::test]
    async fn records_missing_titles_as_failures() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        let llm = FakeLlm::new([ok(&ids[..1])]);
        let summary = run(&db, &llm, &mut quota(10), 5).await;
        assert_eq!((summary.translated, summary.failed), (1, 1));
        assert_eq!(
            db.query_i64(&format!(
                "SELECT count(*) FROM stage_errors WHERE stage = 'title' AND article_id = {}",
                ids[1]
            ))
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn stops_when_the_quota_is_used_up() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 3);
        let llm = FakeLlm::new([ok(&ids[..2])]);
        let summary = run(&db, &llm, &mut quota(1), 2).await;
        assert_eq!((summary.translated, summary.calls), (2, 1));
        assert!(
            matches!(summary.halted, Some(Halt::Quota(_))),
            "{summary:?}"
        );
    }

    #[tokio::test]
    async fn llm_failure_marks_batch_and_halts() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 3);
        let llm = FakeLlm::new([Err(LlmError::Reported {
            subtype: "error".into(),
            message: "Not logged in".into(),
        })]);
        let summary = run(&db, &llm, &mut quota(10), 2).await;
        assert_eq!((summary.calls, summary.failed), (1, 2));
        assert!(matches!(&summary.halted, Some(Halt::LlmFailed(m)) if m.contains("Not logged in")));
    }
}
