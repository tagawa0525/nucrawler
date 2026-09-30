//! 同じ報道の判定ステージ：記事ごとに、文字の類似度で選んだ候補が同じ出来事の報道か、同じ案件の
//! 別の出来事（続報など）かを LLM に判定させ、same の組をつないだグループを作り直す。
//! 候補の無い記事は LLM を呼ばずに判定済みにする。

use chrono::{DateTime, Utc};

use super::Halt;
use super::llm_call::LlmStage;
use crate::config::{LlmConfig, PipelineConfig};
use crate::db::DbError;
use crate::llm::Llm;

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
    pub failed: usize,
    pub calls: usize,
    pub halted: Option<Halt>,
    pub cancelled: bool,
}

pub async fn judge_stories<L: Llm>(
    stage: LlmStage<'_, L>,
    llm_cfg: &LlmConfig,
    pipeline_cfg: &PipelineConfig,
    now: DateTime<Utc>,
) -> Result<StorySummary, StoryStageError> {
    let _ = (stage.db, llm_cfg, pipeline_cfg, now);
    todo!()
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
        .map(|(i, t)| article(db, &format!("f{i}"), 10, t))
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
            &LlmConfig::default(),
            &PipelineConfig::default(),
            now(),
        )
        .await
        .unwrap()
    }

    fn stories(db: &Db) -> Vec<(i64, i64)> {
        let mut v: Vec<_> = db.story_ids().unwrap().into_iter().collect();
        v.sort_unstable();
        v
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
        assert_eq!((summary.judged, summary.calls), (7, 1), "{summary:?}");
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
        assert_eq!((summary.judged, summary.calls), (1, 1), "{summary:?}");
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
            (ans, &[], &[]),
            (wnn, &[], &[]),
        ])]);
        run(&db, &llm, &mut quota(10)).await;
        assert_eq!(stories(&db), [(wnn, wnn), (ans, wnn), (jaif, wnn)]);
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
        assert_eq!((summary.judged, summary.failed), (6, 1), "{summary:?}");
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
            (summary.calls, summary.failed, summary.judged),
            (1, 2, 5),
            "{summary:?}"
        );
        assert!(matches!(&summary.halted, Some(Halt::LlmFailed(m)) if m.contains("Not logged in")));
    }

    #[tokio::test]
    async fn stops_when_the_quota_is_used_up() {
        let db = Db::open_in_memory().unwrap();
        article(&db, "wnn", 15, EIB_WNN);
        let llm = FakeLlm::new([]);
        let summary = run(&db, &llm, &mut quota(0)).await;
        assert_eq!((summary.judged, summary.calls), (0, 0));
        assert!(
            matches!(summary.halted, Some(Halt::Quota(_))),
            "{summary:?}"
        );
    }
}
