//! 同じ報道の判定ステージ：記事ごとに、文字の類似度で選んだ候補が同じ出来事の報道か、同じ案件の
//! 別の出来事（続報など）かを LLM に判定させ、same の組をつないだグループを作り直す。
//! 候補の無い記事は LLM を呼ばずに判定済みにする。

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use super::llm_call::{Call, LlmStage, MISSING, Tally, Workers, claim_ttl, record_failures};
use super::workers::run_workers;
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::{
    ArtifactKind, ClaimKey, Db, DbError, NewArtifact, StageKey, StoryLink, StoryRelation,
};
use crate::llm::{Llm, LlmRequest};
use crate::prompt::story as p;
use crate::story::{Candidate, Doc, Index, Rejected, WINDOW_DAYS};
use crate::{errors, prompt};

pub const STAGE: &str = "story";

#[derive(Debug, thiserror::Error)]
pub enum StoryStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

#[derive(Debug, Default, PartialEq)]
pub struct StorySummary {
    /// 判定を保存した記事（候補が無く LLM を呼ばなかった記事を含む）
    pub judged: usize,
    pub tally: Tally,
}

impl StorySummary {
    /// 作業者ごとの集計を合わせる。
    fn merge(mut self, other: StorySummary) -> StorySummary {
        self.judged += other.judged;
        self.tally.merge(other.tally);
        self
    }
}

/// プロンプトに載せる記事。
fn prompt_article(doc: &Doc) -> p::Article {
    p::Article {
        article_id: doc.article_id,
        source_id: doc.source_id.clone(),
        date: doc
            .at
            .with_timezone(&crate::jst::offset())
            .format("%Y-%m-%d")
            .to_string(),
        text: doc.text.clone(),
    }
}

/// 判定と組を保存する。候補の単位への same / related は、その単位の記事すべてへの組にし、
/// どちらでもない候補は無関係の組にする。
#[allow(clippy::too_many_arguments)]
fn save(
    db: &Db,
    article_id: i64,
    candidates: &[Candidate],
    same: &[i64],
    related: &[i64],
    backend: &str,
    model: &str,
    now: DateTime<Utc>,
) -> Result<(), DbError> {
    let mut links = Vec::new();
    for c in candidates {
        let relation = if same.contains(&c.id) {
            StoryRelation::Same
        } else if related.contains(&c.id) {
            StoryRelation::Related
        } else {
            StoryRelation::Unrelated
        };
        links.extend(c.members.iter().map(|&other_id| StoryLink {
            other_id,
            relation,
            similarity: c.similarity,
        }));
    }
    let payload = serde_json::json!({
        "candidates": candidates.iter().map(|c| c.id).collect::<Vec<_>>(),
        "same": same,
        "related": related,
    });
    db.insert_story(
        &NewArtifact {
            article_id,
            kind: ArtifactKind::Story,
            backend,
            model,
            prompt_version: p::PROMPT_VERSION,
            payload: &payload,
            inputs: &[],
            glossary_at: None,
        },
        &links,
        now,
    )?;
    Ok(())
}

/// グループを作り直し、つながなかった組を知らせる。
fn rebuild(db: &Db) -> Result<(), DbError> {
    for r in db.rebuild_stories()? {
        match r {
            Rejected::TooLarge(e) => tracing::warn!(
                a = e.a,
                b = e.b,
                "story link not joined: the story would exceed {} articles",
                crate::story::MAX_STORY_SIZE
            ),
            // 判定が割れた組や、別の出来事と判定された記事を介する組。LLM の判定のとおりの結果なので
            // 異常ではない
            Rejected::Apart(e) => tracing::info!(
                a = e.a,
                b = e.b,
                "story link not joined: it would join articles judged as different events"
            ),
        }
    }
    Ok(())
}

pub async fn judge_stories<L: Llm>(
    env: LlmStage<'_, L>,
    llm_cfg: &LlmConfig,
    pipeline_cfg: &PipelineConfig,
    now: DateTime<Utc>,
) -> Result<StorySummary, StoryStageError> {
    let workers = Workers::new(env);
    let (db, llm, clock) = (workers.db, workers.llm, workers.clock);
    let backend = llm.backend();
    let model = llm_cfg.story_model.as_str();
    let schema = p::schema();
    let system = p::system_prompt();
    let cutoff = now - chrono::Duration::days(i64::from(pipeline_cfg.backlog_days));
    let window = chrono::Duration::days(WINDOW_DAYS);
    let key = |article_id| StageKey {
        article_id,
        stage: STAGE,
        backend,
        model,
    };
    // 前の実行が判定を保存してからグループを作り直す前に落ちていても、候補を選ぶ前に直しておく
    rebuild(db)?;
    // `llm.concurrency` 個の作業者を同時に回す。同じ記事は作業の予約で分かれる
    let parts = run_workers(llm_cfg.concurrency, |_| async {
        let mut summary = StorySummary::default();
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
                |db| db.pending_stories(cutoff, now, backend, model, llm_cfg.story_batch_size),
                |b| b.article_id,
            )?;
            let (Some(first), Some(last)) = (
                batch.iter().map(|b| b.at).min(),
                batch.iter().map(|b| b.at).max(),
            ) else {
                break;
            };
            let pool = db.story_pool(first - window, last + window)?;
            let mut docs: HashMap<i64, Doc> =
                pool.iter().map(|d| (d.article_id, d.clone())).collect();
            let index = Index::new(pool);
            let stories = db.stories()?;
            // 候補の無い記事は、LLM を呼ばずに判定済みにする
            let mut targets: Vec<(i64, Vec<Candidate>)> = Vec::new();
            for b in &batch {
                let candidates = index.candidates(b.article_id, &stories);
                if candidates.is_empty() {
                    save(db, b.article_id, &[], &[], &[], backend, model, now)?;
                    db.clear_stage_failure(key(b.article_id))?;
                    summary.judged += 1;
                } else {
                    targets.push((b.article_id, candidates));
                }
            }
            if targets.is_empty() {
                continue;
            }
            // グループの記事は期間の外にもいるので、プールに無い分を読み足す
            let outside: Vec<i64> = targets
                .iter()
                .flat_map(|(_, cs)| cs.iter().flat_map(|c| c.members.iter().copied()))
                .filter(|id| !docs.contains_key(id))
                .collect();
            docs.extend(
                db.story_docs(&outside)?
                    .into_iter()
                    .map(|d| (d.article_id, d)),
            );
            let requested: Vec<p::Target> = targets
                .iter()
                .filter_map(|(id, candidates)| {
                    Some(p::Target {
                        article: prompt_article(docs.get(id)?),
                        candidates: candidates
                            .iter()
                            .map(|c| p::Candidate {
                                id: c.id,
                                members: c
                                    .members
                                    .iter()
                                    .filter_map(|m| docs.get(m).map(prompt_article))
                                    .collect(),
                            })
                            .collect(),
                    })
                })
                .collect();
            let prompt = p::build_prompt(&requested);
            let judged_ids: Vec<i64> = requested.iter().map(|t| t.article.article_id).collect();
            let call = Call {
                stage: STAGE,
                n_items: requested.len(),
                req: LlmRequest {
                    system: &system,
                    prompt: &prompt,
                    schema: &schema,
                    model,
                },
            };
            let Some((response, held)) = workers
                .call_held(call, &claim, &judged_ids, &mut summary.tally, key, now)
                .await?
            else {
                break;
            };
            let parsed = match prompt::story::parse(&response.output, &requested) {
                Ok(parsed) => parsed,
                Err(e) => {
                    let message = errors::error_chain(&e);
                    tracing::warn!("story output rejected: {message}");
                    summary.tally.failed +=
                        record_failures(db, held.iter().map(key), &message, now)?;
                    continue;
                }
            };
            for j in &parsed.items {
                if !held.keeps(STAGE, j.target) {
                    continue;
                }
                let Some((_, candidates)) = targets.iter().find(|(id, _)| *id == j.target) else {
                    continue;
                };
                save(
                    db, j.target, candidates, &j.same, &j.related, backend, model, now,
                )?;
                db.clear_stage_failure(key(j.target))?;
                summary.judged += 1;
            }
            summary.tally.failed +=
                record_failures(db, held.of(&parsed.missing).map(key), MISSING, now)?;
            rebuild(db)?;
        }
        workers.finish(&summary.tally);
        Ok::<_, StoryStageError>(summary)
    })
    .await?;
    Ok(parts
        .into_iter()
        .fold(StorySummary::default(), StorySummary::merge))
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

    /// 本文の無い日本語の記事（原題で比べる）。
    fn article(db: &Db, source: &str, day: u32, title: &str) -> i64 {
        let url = format!("https://e.com/{source}/{title}");
        let published = format!("2026-09-{day:02}T00:00:00.000Z");
        db.insert_article(&NewArticle {
            source_id: source,
            url: &url,
            title,
            lang: Lang::Ja,
            published_at: Some(&published),
        })
        .unwrap()
        .unwrap()
    }

    /// 無関係な記事。IDF が効くよう、プールを水増しする。
    fn fillers(db: &Db) -> Vec<i64> {
        [
            "九州電力、玄海原子力発電所3号機の定期検査を開始",
            "関西電力、高浜発電所の防災業務計画を修正",
            "IAEA事務局長、ウクライナ情勢について声明",
            "米エネルギー省、次世代炉の燃料供給に資金",
            "英国、核融合の実証炉計画で企業を選定",
        ]
        .iter()
        .enumerate()
        .map(|(i, t)| article(db, &format!("f{i}"), 20, t))
        .collect()
    }

    const EIB_WNN: &str = "欧州投資銀行、初のSMR向け融資をフィンランドのSteady Energyに供与";
    const EIB_JAIF: &str = "欧州投資銀行、フィンランドのSMR開発に初融資";
    const EIB_ANS: &str = "欧州投資銀行がフィンランドのSMRに初の融資";

    fn judgments(items: &[(i64, &[i64], &[i64])]) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"items": items
                .iter()
                .map(|(id, same, related)| {
                    serde_json::json!({"id": id, "same": same, "related": related})
                })
                .collect::<Vec<_>>()}),
            usage: None,
        })
    }

    async fn run(db: &Db, llm: &FakeLlm, quota: &mut Quota) -> StorySummary {
        judge_stories(
            LlmStage {
                db,
                llm,
                quota,
                cancel: &Cancel::default(),
                clock: &now,
            },
            &LlmConfig::for_tests(),
            &PipelineConfig::default(),
            now(),
        )
        .await
        .unwrap()
    }

    fn stories(db: &Db) -> Vec<(i64, i64)> {
        db.stories().unwrap().grouped()
    }

    /// 候補の無い記事は、LLM を呼ばずに判定済みにする。
    #[tokio::test]
    async fn saves_articles_without_candidates_without_calling() {
        let db = Db::open_in_memory().unwrap();
        let ids = fillers(&db);
        let llm = FakeLlm::new([]);
        let summary = run(&db, &llm, &mut quota(10)).await;
        assert_eq!(
            summary,
            StorySummary {
                judged: ids.len(),
                ..StorySummary::default()
            }
        );
        assert!(llm.requests().is_empty());
        assert_eq!(
            db.query_strings("SELECT DISTINCT payload FROM artifacts WHERE kind = 'story'")
                .unwrap(),
            [r#"{"candidates":[],"related":[],"same":[]}"#]
        );
        assert!(stories(&db).is_empty());
    }

    /// 候補のある記事をまとめて 1 回で判定し、組とグループを作る。
    #[tokio::test]
    async fn judges_candidates_and_groups_the_same_story() {
        let db = Db::open_in_memory().unwrap();
        fillers(&db);
        let wnn = article(&db, "wnn", 15, EIB_WNN);
        let jaif = article(&db, "jaif", 27, EIB_JAIF);
        let llm = FakeLlm::new([judgments(&[(jaif, &[wnn], &[]), (wnn, &[jaif], &[])])]);
        let summary = run(&db, &llm, &mut quota(10)).await;
        assert_eq!((summary.judged, summary.tally.calls), (7, 1), "{summary:?}");
        let requests = llm.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model, "sonnet");
        assert!(
            requests[0]
                .prompt
                .contains(&format!("<target id=\"{jaif}\"")),
            "{}",
            requests[0].prompt
        );
        assert!(
            requests[0].prompt.contains(EIB_WNN),
            "{}",
            requests[0].prompt
        );
        assert_eq!(stories(&db), [(wnn, wnn), (jaif, wnn)]);
        assert_eq!(
            db.query_strings(&format!(
                "SELECT l.relation || '|' || l.other_id FROM story_links AS l
                 JOIN artifacts AS r ON r.id = l.artifact_id WHERE r.article_id = {jaif}"
            ))
            .unwrap(),
            [format!("same|{wnn}")]
        );
        assert_eq!(
            db.query_strings("SELECT stage || '|' || n_items FROM llm_calls")
                .unwrap(),
            ["story|2"]
        );
    }

    /// 既存のグループは 1 つの候補として見せ、そのグループとの same は全員への組に展開する。
    #[tokio::test]
    async fn joins_an_existing_story() {
        let db = Db::open_in_memory().unwrap();
        fillers(&db);
        let wnn = article(&db, "wnn", 15, EIB_WNN);
        let jaif = article(&db, "jaif", 20, EIB_JAIF);
        let llm = FakeLlm::new([judgments(&[(jaif, &[wnn], &[]), (wnn, &[jaif], &[])])]);
        run(&db, &llm, &mut quota(10)).await;
        assert_eq!(stories(&db), [(wnn, wnn), (jaif, wnn)]);

        let ans = article(&db, "ans", 27, EIB_ANS);
        let llm = FakeLlm::new([judgments(&[(ans, &[wnn], &[])])]);
        let summary = run(&db, &llm, &mut quota(10)).await;
        assert_eq!((summary.judged, summary.tally.calls), (1, 1), "{summary:?}");
        let prompt = &llm.requests()[0].prompt;
        assert!(
            prompt.contains(&format!("<story id=\"{wnn}\">")),
            "{prompt}"
        );
        assert_eq!(stories(&db), [(wnn, wnn), (jaif, wnn), (ans, wnn)]);
        assert_eq!(
            db.query_i64(&format!(
                "SELECT count(*) FROM story_links AS l
                 JOIN artifacts AS r ON r.id = l.artifact_id
                 WHERE r.article_id = {ans} AND l.relation = 'same'"
            ))
            .unwrap(),
            2
        );
    }

    /// 期間の外に広がったグループも、全員を候補の記事として見せ、全員への組にする。
    #[tokio::test]
    async fn shows_story_members_outside_the_window() {
        let db = Db::open_in_memory().unwrap();
        fillers(&db);
        let old = db
            .insert_article(&NewArticle {
                source_id: "ans",
                url: "https://e.com/old",
                title: EIB_ANS,
                lang: Lang::Ja,
                published_at: Some("2026-08-01T00:00:00.000Z"),
            })
            .unwrap()
            .unwrap();
        let wnn = article(&db, "wnn", 15, EIB_WNN);
        // old と wnn は、前に同じ報道と判定したグループ（old は期間の外）
        for (a, b) in [(old, wnn), (wnn, old)] {
            db.insert_story(
                &crate::db::NewArtifact {
                    article_id: a,
                    kind: crate::db::ArtifactKind::Story,
                    backend: "fake",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &serde_json::json!({"candidates": [], "same": [], "related": []}),
                    inputs: &[],
                    glossary_at: None,
                },
                &[crate::db::StoryLink {
                    other_id: b,
                    relation: crate::db::StoryRelation::Same,
                    similarity: 0.5,
                }],
                now(),
            )
            .unwrap();
        }
        db.rebuild_stories().unwrap();
        let jaif = article(&db, "jaif", 27, EIB_JAIF);
        let llm = FakeLlm::new([judgments(&[(jaif, &[old], &[])])]);
        run(&db, &llm, &mut quota(10)).await;
        let prompt = &llm.requests()[0].prompt;
        assert!(
            prompt.contains(&format!("<article id=\"{old}\"")),
            "{prompt}"
        );
        assert_eq!(stories(&db), [(old, old), (wnn, old), (jaif, old)]);
        assert_eq!(
            db.query_i64(&format!(
                "SELECT count(*) FROM story_links AS l
                 JOIN artifacts AS r ON r.id = l.artifact_id
                 WHERE r.article_id = {jaif} AND l.relation = 'same'"
            ))
            .unwrap(),
            2
        );
    }

    /// 判定を保存した後、グループを作り直す前に落ちた実行の分も、次の実行の最初に作り直す
    /// （判定する記事が無くても）。
    #[tokio::test]
    async fn rebuilds_stories_left_stale_by_an_earlier_run() {
        let db = Db::open_in_memory().unwrap();
        let a = article(&db, "wnn", 15, EIB_WNN);
        let b = article(&db, "jaif", 27, EIB_JAIF);
        for (x, y) in [(a, b), (b, a)] {
            db.insert_story(
                &crate::db::NewArtifact {
                    article_id: x,
                    kind: crate::db::ArtifactKind::Story,
                    backend: "fake",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &serde_json::json!({"candidates": [], "same": [], "related": []}),
                    inputs: &[],
                    glossary_at: None,
                },
                &[crate::db::StoryLink {
                    other_id: y,
                    relation: crate::db::StoryRelation::Same,
                    similarity: 0.5,
                }],
                now(),
            )
            .unwrap();
        }
        assert!(stories(&db).is_empty());
        let llm = FakeLlm::new([]);
        run(&db, &llm, &mut quota(10)).await;
        assert_eq!(stories(&db), [(a, a), (b, a)]);
    }

    /// 1 件の記事が複数の候補と same なら、それらが 1 つのグループにまとまる。
    #[tokio::test]
    async fn one_article_can_join_several_candidates() {
        let db = Db::open_in_memory().unwrap();
        fillers(&db);
        let wnn = article(&db, "wnn", 15, EIB_WNN);
        let ans = article(&db, "ans", 16, EIB_ANS);
        let jaif = article(&db, "jaif", 27, EIB_JAIF);
        let llm = FakeLlm::new([judgments(&[
            (jaif, &[wnn, ans], &[]),
            (ans, &[wnn, jaif], &[]),
            (wnn, &[ans, jaif], &[]),
        ])]);
        run(&db, &llm, &mut quota(10)).await;
        assert_eq!(stories(&db), [(wnn, wnn), (ans, wnn), (jaif, wnn)]);
    }

    /// 候補にしたが same でも related でもない記事も、無関係の組として残す（逆向きの判定で使う）。
    #[tokio::test]
    async fn records_rejected_candidates_as_unrelated() {
        let db = Db::open_in_memory().unwrap();
        fillers(&db);
        let wnn = article(&db, "wnn", 15, EIB_WNN);
        let jaif = article(&db, "jaif", 27, EIB_JAIF);
        let llm = FakeLlm::new([judgments(&[(jaif, &[], &[]), (wnn, &[jaif], &[])])]);
        run(&db, &llm, &mut quota(10)).await;
        assert_eq!(
            db.query_strings(&format!(
                "SELECT l.relation || '|' || l.other_id FROM story_links AS l
                 JOIN artifacts AS r ON r.id = l.artifact_id WHERE r.article_id = {jaif}"
            ))
            .unwrap(),
            [format!("unrelated|{wnn}")]
        );
        // 判定が割れたのでまとめない
        assert!(stories(&db).is_empty());
    }

    /// 応答に無かった記事は失敗として記録する（ほかの記事は保存する）。
    #[tokio::test]
    async fn records_missing_judgments_as_failures() {
        let db = Db::open_in_memory().unwrap();
        fillers(&db);
        let wnn = article(&db, "wnn", 15, EIB_WNN);
        let jaif = article(&db, "jaif", 27, EIB_JAIF);
        let llm = FakeLlm::new([judgments(&[(jaif, &[wnn], &[])])]);
        let summary = run(&db, &llm, &mut quota(10)).await;
        assert_eq!(
            (summary.judged, summary.tally.failed),
            (6, 1),
            "{summary:?}"
        );
        assert_eq!(
            db.query_i64(&format!(
                "SELECT count(*) FROM stage_errors WHERE stage = 'story' AND article_id = {wnn}"
            ))
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn llm_failure_marks_the_judged_articles_and_halts() {
        let db = Db::open_in_memory().unwrap();
        fillers(&db);
        article(&db, "wnn", 15, EIB_WNN);
        article(&db, "jaif", 27, EIB_JAIF);
        let llm = FakeLlm::new([Err(LlmError::Reported {
            subtype: "error".into(),
            message: "Not logged in".into(),
        })]);
        let summary = run(&db, &llm, &mut quota(10)).await;
        assert_eq!(
            (summary.tally.calls, summary.tally.failed, summary.judged),
            (1, 2, 5),
            "{summary:?}"
        );
        assert!(
            matches!(&summary.tally.halted, Some(Halt::LlmFailed(m)) if m.contains("Not logged in"))
        );
    }

    #[tokio::test]
    async fn stops_when_the_quota_is_used_up() {
        let db = Db::open_in_memory().unwrap();
        article(&db, "wnn", 15, EIB_WNN);
        let llm = FakeLlm::new([]);
        let summary = run(&db, &llm, &mut quota(0)).await;
        assert_eq!((summary.judged, summary.tally.calls), (0, 0));
        assert!(
            matches!(summary.tally.halted, Some(Halt::Quota(_))),
            "{summary:?}"
        );
    }
}
