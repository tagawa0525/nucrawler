//! プロファイルの見直し（計画 016）：評価が増えた利用者について、評価を根拠に LLM が更新案を作り、
//! 今のプロファイルと案を同じ評価で採点して比べ、案を保存する。利用者の設定が「当てる」で、案が十分に
//! 良ければ、そのまま新しい版にする（履歴から戻せる）。

use chrono::{DateTime, Utc};

use super::llm_call::{LlmStage, Tally};
use super::{score, suggest};
use crate::config::Config;
use crate::db::{
    DbError, NewSuggestion, ProfileOrigin, SuggestionStatus, SuggestionTrigger, VersionStats,
};
use crate::llm::Llm;

pub const STAGE: &str = "review";

/// 前の案（無ければ今の版）の後に、評価がこれだけ増えたら案を作る
pub const NEW_RATINGS: usize = 10;
/// 自動で当てるのに要る、一致率の上げ幅（案は根拠にした評価で測るので甘く出る。その分を見込む）
pub const MIN_GAIN: f64 = 0.05;
/// 自動で当てるのに要る、高い評価（★4〜5）と低い評価（★1〜2、無ければ ★3 以下）のそれぞれの件数
pub const MIN_EACH: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum ReviewStageError {
    #[error("database error")]
    Db(#[from] DbError),
    #[error(transparent)]
    Suggest(#[from] suggest::SuggestStageError),
    #[error(transparent)]
    Score(#[from] score::ScoreStageError),
}

#[derive(Debug, Default, PartialEq)]
pub struct ReviewSummary {
    /// 案を保存した利用者の数
    pub suggested: usize,
    /// そのうち自動で当てた数
    pub applied: usize,
    pub tally: Tally,
}

/// 評価が `NEW_RATINGS` 件以上増えた利用者のプロファイルを見直す。LLM が止まったら（上限・失敗・中断）、
/// その利用者の案は保存せずに終える（評価の件数は前の案から数えるので、次の実行で作り直す）。
pub async fn review_profiles<L: Llm>(
    mut env: LlmStage<'_, L>,
    config: &Config,
    requests_only: bool,
    now: DateTime<Utc>,
) -> Result<ReviewSummary, ReviewStageError> {
    let _ = requests_only;
    let db = env.db;
    let mut summary = ReviewSummary::default();
    for user in db.scoring_profiles()? {
        if env.cancel.is_requested() {
            break;
        }
        if db.ratings_since_review(user.user_id)? < NEW_RATINGS {
            continue;
        }
        let evidence = db.label_evidence(user.user_id)?;
        if evidence.is_empty() {
            continue;
        }
        let suggested =
            suggest::suggest_profile(reborrow(&mut env), &config.llm, &user.profile, &evidence)
                .await?;
        summary.tally.merge(suggested.tally);
        let Some(suggestion) = suggested.suggestion else {
            break;
        };
        let evidence_ids: Vec<i64> = evidence.iter().map(|e| e.article_id).collect();
        let new = |current: VersionStats, candidate: VersionStats, status| NewSuggestion {
            user_id: user.user_id,
            base_hash: &user.hash,
            profile: &suggestion.profile,
            reasons: &suggestion.reasons,
            evidence: &evidence_ids,
            current,
            candidate,
            trigger: SuggestionTrigger::Auto,
            status,
        };
        if crate::profile::diff(&user.profile, &suggestion.profile).is_empty() {
            // 変える根拠が無かったことも残し、評価の件数をここから数え直す
            let (current, _) = db.paired_stats(user.user_id, &user.hash, &user.hash)?;
            db.save_suggestion(&new(current, current, SuggestionStatus::Unchanged), now)?;
            summary.suggested += 1;
            continue;
        }
        // 今と案を、評価したすべての記事で採点する（採点済みの記事では LLM を呼ばない）
        let rated: Vec<i64> = db
            .eval_labels(user.user_id)?
            .iter()
            .map(|l| l.article_id)
            .collect();
        for profile in [&user.profile, &suggestion.profile] {
            let scored = score::score_articles(
                reborrow(&mut env),
                &config.llm,
                &config.pipeline,
                user.user_id,
                score::ScoreTarget::Candidate {
                    profile,
                    articles: &rated,
                },
                now,
            )
            .await?;
            let stopped = scored.tally.halted.is_some() || scored.tally.cancelled;
            summary.tally.merge(scored.tally);
            if stopped {
                return Ok(summary);
            }
        }
        // 片方の採点に失敗した記事を除き、同じ記事の集合で比べる
        let (current, candidate) = db.paired_stats(
            user.user_id,
            &user.hash,
            &crate::profile::hash(&suggestion.profile),
        )?;
        // 案を作る間にプロファイルが変わっていたら（取り込み・戻し）、古い案は捨てて次の実行で作り直す
        let Some(id) =
            db.save_suggestion(&new(current, candidate, SuggestionStatus::Pending), now)?
        else {
            continue;
        };
        summary.suggested += 1;
        let ratings: Vec<u8> = evidence.iter().map(|e| e.rating.get()).collect();
        if db.auto_apply_profile(user.user_id)?
            && worth_applying(current, candidate, &ratings)
            && db.apply_suggestion(user.user_id, id, ProfileOrigin::Auto, now)?
        {
            summary.applied += 1;
        }
    }
    Ok(summary)
}

/// 同じ工程の環境を、続けて呼ぶ処理に貸す（クォータは共有する）。
fn reborrow<'b, L>(env: &'b mut LlmStage<'_, L>) -> LlmStage<'b, L> {
    LlmStage {
        db: env.db,
        llm: env.llm,
        quota: &mut *env.quota,
        cancel: env.cancel,
        clock: env.clock,
    }
}

/// 案を自動で当てる条件：案の一致率が今より `MIN_GAIN` 以上高く、評価に高いものと低いものがそれぞれ
/// `MIN_EACH` 件以上ある。
pub fn worth_applying(current: VersionStats, candidate: VersionStats, ratings: &[u8]) -> bool {
    let (Some(current), Some(candidate)) = (current.concordance, candidate.concordance) else {
        return false;
    };
    let count = |f: fn(u8) -> bool| ratings.iter().filter(|&&r| f(r)).count();
    let high = count(|r| r >= 4);
    let low = match count(|r| r <= 2) {
        0 => count(|r| r <= 3),
        n => n,
    };
    // 一致率の差は浮動小数で誤差が出るので、ちょうど `MIN_GAIN` の上げ幅を落とさないよう許容幅を持たせる
    candidate - current + 1e-9 >= MIN_GAIN && high >= MIN_EACH && low >= MIN_EACH
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{
        ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact, ProfileOrigin,
        Rating, SuggestionStatus,
    };
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmRequest, LlmResponse};
    use crate::pipeline::Cancel;
    use crate::profile::{Interest, Profile};
    use crate::quota::{Quota, QuotaConfig};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-04T00:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn profile(topic: &str) -> Profile {
        Profile {
            interests: vec![Interest {
                topic: topic.into(),
                weight: 1.0,
                note: None,
            }],
            exclude: vec![],
        }
    }

    /// 要約を付けて評価した記事。
    fn rated(db: &Db, n: usize, value: u8) -> i64 {
        let id = db
            .insert_article(&NewArticle {
                source_id: "s",
                url: &format!("https://e.com/{n}"),
                title: "t",
                lang: Lang::En,
                published_at: Some("2026-10-01T00:00:00.000Z"),
            })
            .unwrap()
            .unwrap();
        let c = db
            .insert_content(id, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        db.insert_artifact(
            &NewArtifact {
                article_id: id,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &serde_json::json!({
                    "title_ja": format!("記事{n}"), "summary_ja": "要約", "points_ja": [],
                    "implications_ja": "", "lwr_relevant": true, "topics": [],
                }),
                inputs: &[c],
                glossary_at: None,
            },
            DateTime::parse_from_rfc3339("2026-10-01T01:00:00Z")
                .unwrap()
                .to_utc(),
        )
        .unwrap();
        db.rate(
            db.owner_id().unwrap(),
            id,
            Rating::new(value),
            DateTime::parse_from_rfc3339("2026-10-02T00:00:00Z")
                .unwrap()
                .to_utc(),
        )
        .unwrap();
        id
    }

    /// 今のプロファイル「今の関心」と、★5 と ★1 の記事 `each` 件ずつ（評価はプロファイルの後）。
    /// 好きな記事の id を返す。
    fn setup(db: &Db, each: usize) -> Vec<i64> {
        db.save_profile(
            db.owner_id().unwrap(),
            &profile("今の関心"),
            DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
                .unwrap()
                .to_utc(),
        )
        .unwrap();
        let liked = (0..each).map(|n| rated(db, n, 5)).collect();
        for n in 0..each {
            rated(db, 100 + n, 1);
        }
        liked
    }

    /// 案は `suggested` のプロファイル。採点は、`good` を含むプロファイルなら好きな記事に高い点、
    /// そうでなければ逆に付ける。
    fn llm(suggested: &'static str, good: &'static [&'static str], liked: Vec<i64>) -> FakeLlm {
        FakeLlm::responding(std::time::Duration::ZERO, move |req: &LlmRequest<'_>| {
            if req.system == crate::prompt::suggest::system_prompt() {
                return Ok(LlmResponse {
                    output: serde_json::json!({
                        "interests": [{"topic": suggested, "weight": 1.0, "note": ""}],
                        "exclude": [],
                        "reasons": [{"change": "変えた", "evidence": "★5 が 5 件"}],
                    }),
                    usage: None,
                });
            }
            let fits = good.iter().any(|g| req.system.contains(g));
            let items: Vec<serde_json::Value> = req
                .prompt
                .split("<article id=\"")
                .skip(1)
                .map(|rest| {
                    let id: i64 = rest[..rest.find('"').unwrap()].parse().unwrap();
                    let score = if liked.contains(&id) == fits { 90 } else { 10 };
                    serde_json::json!({
                        "id": id, "score": score, "reason": "理由", "matched": [], "excluded": [],
                    })
                })
                .collect();
            Ok::<_, LlmError>(LlmResponse {
                output: serde_json::json!({ "items": items }),
                usage: None,
            })
        })
    }

    async fn run(db: &Db, llm: &FakeLlm) -> ReviewSummary {
        run_with(db, llm, false).await
    }

    async fn run_with(db: &Db, llm: &FakeLlm, requests_only: bool) -> ReviewSummary {
        let mut quota = Quota::new(QuotaConfig::default(), None, Some(100));
        review_profiles(
            LlmStage {
                db,
                llm,
                quota: &mut quota,
                cancel: &Cancel::default(),
                clock: &now,
            },
            &Config::default(),
            requests_only,
            now(),
        )
        .await
        .unwrap()
    }

    /// 評価が増えたら案を作り、今と案を同じ評価で採点して比べる。十分に良ければ自動で版にする。
    #[tokio::test]
    async fn applies_a_better_suggestion() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let liked = setup(&db, 5);
        let llm = llm("新しい関心", &["新しい関心"], liked);
        let summary = run(&db, &llm).await;
        assert_eq!((summary.suggested, summary.applied), (1, 1));
        let current = &db.profile_versions(owner).unwrap()[0];
        assert_eq!(
            (&current.profile, current.origin),
            (&profile("新しい関心"), ProfileOrigin::Auto)
        );
        let s = &db.profile_suggestions(owner).unwrap()[0];
        assert_eq!(s.status, SuggestionStatus::Applied);
        assert_eq!(
            (s.current.concordance, s.candidate.concordance),
            (Some(0.0), Some(1.0))
        );
        assert_eq!((s.current.rated, s.candidate.rated), (10, 10));
        assert_eq!(s.evidence.len(), 10);
        // 案を作った後は、評価が増えるまで作らない
        let calls = llm.requests().len();
        assert_eq!(run(&db, &llm).await, ReviewSummary::default());
        assert_eq!(llm.requests().len(), calls);
    }

    /// 設定が「当てない」か、案が十分に良くなければ、案は待たせて人の判断に回す。
    #[tokio::test]
    async fn keeps_a_suggestion_pending() {
        for (auto, good) in [
            (false, &["新しい関心"][..]),
            // 今のプロファイルも同じく当たる（上げ幅が無い）
            (true, &["新しい関心", "今の関心"][..]),
        ] {
            let db = Db::open_in_memory().unwrap();
            let owner = db.owner_id().unwrap();
            let liked = setup(&db, 5);
            if !auto {
                db.conn()
                    .execute("UPDATE users SET auto_apply_profile = 0", [])
                    .unwrap();
            }
            let summary = run(&db, &llm("新しい関心", good, liked)).await;
            assert_eq!((summary.suggested, summary.applied), (1, 0), "{auto}");
            assert_eq!(
                db.load_profile(owner).unwrap().unwrap().0,
                profile("今の関心")
            );
            assert_eq!(
                db.profile_suggestions(owner).unwrap()[0].status,
                SuggestionStatus::Pending
            );
        }
    }

    /// 評価の増え方が足りなければ LLM を呼ばない。案が今と同じなら、採点せずに「変えない」と残す。
    #[tokio::test]
    async fn skips_until_ratings_grow_and_records_no_change() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let liked = setup(&db, 4);
        let llm = llm("今の関心", &[], liked);
        assert_eq!(run(&db, &llm).await, ReviewSummary::default());
        assert!(llm.requests().is_empty());
        rated(&db, 200, 5);
        rated(&db, 201, 1);
        let summary = run(&db, &llm).await;
        assert_eq!((summary.suggested, summary.applied), (1, 0));
        assert_eq!(llm.requests().len(), 1);
        assert_eq!(
            db.profile_suggestions(owner).unwrap()[0].status,
            SuggestionStatus::Unchanged
        );
    }

    /// 頼まれた利用者は、評価の件数によらず案を作る（手動の案。依頼は片付く）。`requests_only` なら
    /// 頼まれた利用者だけを見る。評価が無ければ依頼を取り下げる。
    #[tokio::test]
    async fn reviews_requested_profiles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let liked = setup(&db, 5);
        let llm = llm("新しい関心", &["新しい関心"], liked.clone());
        // 評価は 10 件あるが、頼まれた利用者だけを見る
        assert_eq!(run_with(&db, &llm, true).await, ReviewSummary::default());
        assert!(llm.requests().is_empty());
        db.request_review(owner, now()).unwrap();
        let summary = run_with(&db, &llm, true).await;
        assert_eq!((summary.suggested, summary.applied), (1, 1));
        let s = &db.profile_suggestions(owner).unwrap()[0];
        assert_eq!(s.trigger, crate::db::SuggestionTrigger::Manual);
        assert!(db.review_requests().unwrap().is_empty());
        // 前の案の後に評価が無くても、頼まれれば作る
        db.request_review(owner, now()).unwrap();
        let calls = llm.requests().len();
        assert_eq!(run_with(&db, &llm, false).await.suggested, 1);
        assert!(llm.requests().len() > calls);
        // 評価が無い利用者の依頼は、LLM を呼ばずに取り下げる
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        db.save_profile(other, &profile("今の関心"), now()).unwrap();
        db.request_review(other, now()).unwrap();
        let calls = llm.requests().len();
        assert_eq!(run_with(&db, &llm, true).await, ReviewSummary::default());
        assert_eq!(llm.requests().len(), calls);
        assert!(db.review_requests().unwrap().is_empty());
    }

    #[test]
    fn applies_only_clear_gains_with_enough_ratings() {
        let stats = |c: f64| VersionStats {
            rated: 10,
            concordance: Some(c),
        };
        let balanced = [5, 5, 4, 4, 4, 2, 2, 1, 1, 1];
        assert!(worth_applying(stats(0.4), stats(0.46), &balanced));
        // ちょうど 0.05 も含む（0.45 - 0.4 は浮動小数では 0.05 をわずかに下回る）
        assert!(worth_applying(stats(0.4), stats(0.45), &balanced));
        assert!(!worth_applying(stats(0.4), stats(0.44), &balanced));
        // 低い評価が ★1〜2 に無ければ ★3 以下で数える
        assert!(worth_applying(
            stats(0.4),
            stats(0.6),
            &[5, 5, 4, 4, 4, 3, 3, 3, 3, 3]
        ));
        // 高い評価が足りない
        assert!(!worth_applying(
            stats(0.4),
            stats(0.6),
            &[5, 4, 4, 4, 2, 2, 2, 1, 1, 1]
        ));
        // どちらかの一致率が測れない
        let none = VersionStats {
            rated: 0,
            concordance: None,
        };
        assert!(!worth_applying(none, stats(0.6), &balanced));
        assert!(!worth_applying(stats(0.4), none, &balanced));
    }
}
