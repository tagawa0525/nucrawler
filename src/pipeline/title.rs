//! 見出しの和訳ステージ：本文が取れず要約できない英語記事の見出しを、まとめて和訳する。
//! 本文が後から取れて要約ができれば、表示には要約の見出しが使われる。

use chrono::{DateTime, Utc};

use super::llm_call::{Call, LlmStage, MISSING, Tally, Workers, claim_ttl, record_failures};
use super::workers::run_workers;
use crate::config::LlmConfig;
use crate::db::{ArtifactKind, ClaimKey, DbError, NewArtifact, StageKey};
use crate::llm::{Llm, LlmRequest};
use crate::{errors, glossary, prompt};

pub const STAGE: &str = "title";

#[derive(Debug, thiserror::Error)]
pub enum TitleStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

#[derive(Debug, Default, PartialEq)]
pub struct TitleSummary {
    pub translated: usize,
    pub tally: Tally,
}

impl TitleSummary {
    /// 作業者ごとの集計を合わせる。
    fn merge(mut self, other: TitleSummary) -> TitleSummary {
        self.translated += other.translated;
        self.tally.merge(other.tally);
        self
    }
}

pub async fn translate_titles<L: Llm>(
    env: LlmStage<'_, L>,
    llm_cfg: &LlmConfig,
    now: DateTime<Utc>,
) -> Result<TitleSummary, TitleStageError> {
    let workers = Workers::new(env);
    let (db, llm, clock) = (workers.db, workers.llm, workers.clock);
    let backend = llm.backend();
    let model = llm_cfg.title_model.as_str();
    let schema = prompt::title::schema();
    let key = |article_id| StageKey {
        article_id,
        stage: STAGE,
        backend,
        model,
    };
    // `llm.concurrency` 個の作業者を同時に回す。同じ記事は作業の予約で分かれる
    let parts = run_workers(llm_cfg.concurrency, |_| async {
        let mut summary = TitleSummary::default();
        loop {
            let Some(_slot) = workers.begin_round(STAGE, &mut summary.tally).await? else {
                break;
            };
            // 予約は処理を終える（この周の終わりで drop する）まで持つ
            let (batch, claim) = db.claim_selected(
                ClaimKey {
                    stage: STAGE,
                    backend,
                    model,
                },
                clock(),
                claim_ttl(llm_cfg),
                |db| db.pending_titles(now, backend, model, llm_cfg.title_batch_size),
                |b| b.article_id,
            )?;
            if batch.is_empty() {
                break;
            }
            let ids: Vec<i64> = batch.iter().map(|b| b.article_id).collect();
            let prompt = prompt::title::build_prompt(&batch);
            let entries = db.glossary_entries()?;
            let system = prompt::title::system_prompt(&glossary::relevant(&entries, &prompt).terms);
            let call = Call {
                stage: STAGE,
                n_items: batch.len(),
                req: LlmRequest {
                    system: &system,
                    prompt: &prompt,
                    schema: &schema,
                    model,
                },
            };
            let Some((response, held)) = workers
                .call_held(call, &claim, &ids, &mut summary.tally, key, now)
                .await?
            else {
                break;
            };
            let parsed = match prompt::title::parse(&response.output, &ids) {
                Ok(parsed) => parsed,
                Err(e) => {
                    let message = errors::error_chain(&e);
                    tracing::warn!("title output rejected: {message}");
                    summary.tally.failed +=
                        record_failures(db, held.iter().map(key), &message, now)?;
                    continue;
                }
            };
            for (id, title_ja) in &parsed.items {
                if !held.keeps(STAGE, *id) {
                    continue;
                }
                // 時点はバッチ全体ではなく、その記事の見出しに当たった訳語から決める
                let glossary_at = batch.iter().find(|b| b.article_id == *id).and_then(|b| {
                    let own = prompt::title::build_prompt(std::slice::from_ref(b));
                    glossary::relevant(&entries, &own).glossary_at
                });
                db.insert_artifact(
                    &NewArtifact {
                        article_id: *id,
                        kind: ArtifactKind::Title,
                        backend,
                        model,
                        prompt_version: prompt::title::PROMPT_VERSION,
                        payload: &serde_json::json!({ "title_ja": title_ja }),
                        inputs: &[],
                        glossary_at: glossary_at.as_deref(),
                    },
                    now,
                )?;
                db.clear_stage_failure(key(*id))?;
                summary.translated += 1;
            }
            summary.tally.failed +=
                record_failures(db, held.of(&parsed.missing).map(key), MISSING, now)?;
        }
        workers.finish(&summary.tally);
        Ok::<_, TitleStageError>(summary)
    })
    .await?;
    Ok(parts
        .into_iter()
        .fold(TitleSummary::default(), TitleSummary::merge))
}

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
            usage: None,
        })
    }

    async fn run(db: &Db, llm: &FakeLlm, quota: &mut Quota, batch: usize) -> TitleSummary {
        translate_titles(
            LlmStage {
                db,
                llm,
                quota,
                cancel: &Cancel::default(),
                clock: &now,
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
                tally: Tally {
                    calls: 2,
                    ..Tally::default()
                },
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
        assert_eq!((summary.translated, summary.tally.failed), (1, 1));
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
        assert_eq!((summary.translated, summary.tally.calls), (2, 1));
        assert!(
            matches!(summary.tally.halted, Some(Halt::Quota(_))),
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
        assert_eq!((summary.tally.calls, summary.tally.failed), (1, 2));
        assert!(
            matches!(&summary.tally.halted, Some(Halt::LlmFailed(m)) if m.contains("Not logged in"))
        );
    }

    /// ほかの実行が予約している記事は飛ばし、自分の予約は処理を終えたら外す。
    #[tokio::test]
    async fn skips_articles_claimed_elsewhere_and_releases_its_own() {
        let db = Db::open_in_memory().unwrap();
        let ids = articles(&db, 2);
        let key = crate::db::ClaimKey {
            stage: "title",
            backend: "fake",
            model: "sonnet",
        };
        let other = db
            .claim(key, &[ids[0]], now(), chrono::Duration::minutes(10))
            .unwrap();
        let llm = FakeLlm::new([ok(&ids[1..])]);
        let summary = run(&db, &llm, &mut quota(10), 5).await;
        assert_eq!(summary.translated, 1);
        assert!(!llm.requests()[0].prompt.contains("Title 0"));
        assert_eq!(db.query_i64("SELECT count(*) FROM work_claims").unwrap(), 1);
        drop(other);
    }

    /// クォータの判定は、ステージを始めた時刻ではなく判定する時点の時刻で行う（枠を待つ間や長い
    /// ステージの途中で時間帯が変われば、その時間帯の上限を使う）。
    #[tokio::test]
    async fn checks_the_quota_at_the_current_time() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 1);
        // JST 23:00 の時間帯の上限は 20%（ステージを始めた JST 11:00 は 85%）
        let late = now() + chrono::Duration::hours(12);
        db.record_llm_call(
            &crate::db::LlmCall {
                stage: "digest",
                backend: "fake",
                model: "sonnet",
                n_items: 1,
                ok: true,
                duration_ms: 1,
                error: None,
                usage: Some(&crate::llm::Usage::Subscription(crate::llm::RateLimit {
                    five_hour: Some(crate::llm::Window {
                        utilization: 0.5,
                        resets_at: late.timestamp() + 3600,
                    }),
                    seven_day: None,
                })),
            },
            now(),
        )
        .unwrap();
        let llm = FakeLlm::new([]);
        let summary = translate_titles(
            LlmStage {
                db: &db,
                llm: &llm,
                quota: &mut quota(10),
                cancel: &Cancel::default(),
                clock: &|| late,
            },
            &LlmConfig::default(),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(summary.tally.calls, 0);
        assert!(
            matches!(summary.tally.halted, Some(Halt::Quota(_))),
            "{summary:?}"
        );
    }

    /// 同時に `llm.concurrency` 本まで呼び出す。同じ記事は取り合わない。
    #[tokio::test]
    async fn calls_concurrently_up_to_the_limit() {
        let db = Db::open_in_memory().unwrap();
        articles(&db, 4);
        let llm = FakeLlm::responding(std::time::Duration::from_millis(50), |req| {
            let id: i64 = req
                .prompt
                .split("<article id=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap()
                .parse()
                .unwrap();
            ok(&[id])
        });
        let summary = translate_titles(
            LlmStage {
                db: &db,
                llm: &llm,
                quota: &mut quota(10),
                cancel: &Cancel::default(),
                clock: &now,
            },
            &LlmConfig {
                title_batch_size: 1,
                concurrency: 2,
                ..LlmConfig::default()
            },
            now(),
        )
        .await
        .unwrap();
        assert_eq!((summary.translated, summary.tally.calls), (4, 4));
        assert_eq!(llm.max_in_flight(), 2);
        assert_eq!(
            db.query_i64("SELECT count(DISTINCT article_id) FROM artifacts WHERE kind = 'title'")
                .unwrap(),
            4
        );
    }
}
