//! ステージを順に流す：`crawl` は計画したステージを実行し、`redo` は要約か和訳を作り直す。
//! LLM が使えなくなった後の扱いと、止めた理由の報告をここで決める。ロック・シグナル・終了コードは
//! 呼び出し側（バイナリ）で扱う。

use chrono::{DateTime, Utc};

use super::llm_call::LlmStage;
use super::{Cancel, Halt, RedoKind, RedoSpec, Stage, Target};
use super::{
    digest, embed, embed_profiles, extract, fetch, review, score, story, suggest, tidy, title,
    translate,
};
use crate::config::{Config, LlmConfig, LlmTask, Source};
use crate::db::{Db, DbError, Evidence, RedoFilter};
use crate::http::Fetcher;
use crate::llm::{Llm, LlmSet};
use crate::profile::Profile;
use crate::quota::Quota;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Fetch(#[from] fetch::FetchError),
    #[error(transparent)]
    Extract(#[from] extract::ExtractStageError),
    #[error(transparent)]
    Digest(#[from] digest::DigestStageError),
    #[error(transparent)]
    Score(#[from] score::ScoreStageError),
    #[error(transparent)]
    Translate(#[from] translate::TranslateStageError),
    #[error(transparent)]
    Tidy(#[from] tidy::TidyStageError),
    #[error(transparent)]
    Title(#[from] title::TitleStageError),
    #[error("story stage failed")]
    Story(#[from] story::StoryStageError),
    #[error(transparent)]
    Suggest(#[from] suggest::SuggestStageError),
    #[error(transparent)]
    Review(#[from] review::ReviewStageError),
}

/// 1 回の実行で共有する環境。クォータは実行全体に効くので、ステージ間で引き継ぐ。
pub struct RunEnv<'a, L> {
    pub db: &'a Db,
    pub llm: &'a L,
    pub quota: &'a mut Quota,
    pub cancel: &'a Cancel,
    /// ステージを始めるたびに今の時刻を読む。ステージの中でも、クォータの判定・作業の予約と延長・
    /// 呼び出しの記録のたびに読む（`LlmStage::clock`。テストでは固定する）
    pub clock: &'a dyn Fn() -> DateTime<Utc>,
}

impl<L: LlmSet> RunEnv<'_, L> {
    /// `task` の工程を、その工程のバックエンドで動かす環境。
    fn stage(&mut self, task: LlmTask) -> LlmStage<'_, L::Llm> {
        LlmStage {
            db: self.db,
            llm: self.llm.for_task(task),
            quota: self.quota,
            cancel: self.cancel,
            clock: self.clock,
        }
    }

    /// `task` の工程のバックエンド（`llm_calls` などに記録する名前）
    fn backend(&self, task: LlmTask) -> &'static str {
        self.llm.for_task(task).backend()
    }
}

/// 実行の結果。どれをエラーや終了コードにするかは呼び出し側が決める。
#[derive(Debug, Default, PartialEq)]
pub struct RunReport {
    pub failed_sources: usize,
    /// 認証切れなど、利用者が対処すべき LLM の失敗
    pub llm_failure: Option<String>,
    pub cancelled: bool,
    /// 利用上限や LLM の失敗で止まったバックエンド。この実行では、そのバックエンドを使う後続の
    /// LLM ステージを呼ばない（crawl をロックの単位に分けて呼んでも引き継ぐ）
    pub llm_blocked: Vec<&'static str>,
    /// embedding を作れなかった理由（サービスの失敗や、モデルが替わって作り直しが要るとき）
    pub embedding_failure: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CrawlOptions {
    /// 和訳は依頼されたものだけを処理する
    pub requests_only: bool,
    /// 前回の整理からの間隔によらず、語彙を整理する（`--only tidy`）
    pub force_tidy: bool,
}

pub async fn crawl<L: LlmSet>(
    mut env: RunEnv<'_, L>,
    stages: &[Stage],
    opts: CrawlOptions,
    config: &Config,
    sources: &[Source],
    fetcher: &Fetcher,
    report: &mut RunReport,
) -> Result<(), RunError> {
    let db = env.db;
    for &stage in stages {
        if env.cancel.is_requested() {
            break;
        }
        let task = stage.llm_task();
        if let Some(task) = task
            && report.llm_blocked.contains(&env.backend(task))
        {
            tracing::warn!(
                stage = stage.name(),
                "skipped: the llm is unavailable in this run"
            );
            continue;
        }
        // LLM のステージは止めた理由を返し、使えなくなったバックエンドを上の判定と同じ工程（`task`）で記録する
        let halted = match stage {
            Stage::Fetch => {
                let summary =
                    fetch::fetch_sources(db, fetcher, sources, env.cancel, (env.clock)()).await?;
                tracing::info!(
                    new_articles = summary.new_articles,
                    failed_sources = summary.failed_sources.len(),
                    "fetch stage finished"
                );
                report.failed_sources += summary.failed_sources.len();
                None
            }
            Stage::Digest => {
                // 採点が計画に無ければ、採点のための予約はしない
                let digest_cfg = LlmConfig {
                    score_reserved_calls: super::score_reserve(
                        stages,
                        &config.llm,
                        db.profile_hash(db.owner_id()?)?.is_some(),
                    ),
                    ..config.llm.clone()
                };
                let now = (env.clock)();
                let summary = digest::digest_articles(
                    env.stage(LlmTask::Digest),
                    &digest_cfg,
                    &config.pipeline,
                    &Target::Pending {
                        requests_only: false,
                    },
                    now,
                )
                .await?;
                tracing::info!(
                    digested = summary.digested,
                    failed = summary.tally.failed,
                    calls = summary.tally.calls,
                    "digest stage finished"
                );
                summary.tally.halted
            }
            Stage::Score => {
                let now = (env.clock)();
                let summary = score::score_articles(
                    env.stage(LlmTask::Score),
                    &config.llm,
                    &config.pipeline,
                    db.owner_id()?,
                    score::ScoreTarget::Saved,
                    now,
                )
                .await?;
                tracing::info!(
                    scored = summary.scored,
                    failed = summary.tally.failed,
                    calls = summary.tally.calls,
                    "score stage finished"
                );
                summary.tally.halted
            }
            Stage::Translate => {
                let now = (env.clock)();
                let summary = translate::translate_articles(
                    env.stage(LlmTask::Translate),
                    &config.llm,
                    &config.pipeline,
                    db.owner_id()?,
                    &Target::Pending {
                        requests_only: opts.requests_only,
                    },
                    now,
                )
                .await?;
                tracing::info!(
                    translated = summary.translated,
                    failed = summary.tally.failed,
                    calls = summary.tally.calls,
                    "translate stage finished"
                );
                summary.tally.halted
            }
            Stage::Title => {
                let now = (env.clock)();
                let summary =
                    title::translate_titles(env.stage(LlmTask::Title), &config.llm, now).await?;
                tracing::info!(
                    translated = summary.translated,
                    failed = summary.tally.failed,
                    calls = summary.tally.calls,
                    "title stage finished"
                );
                summary.tally.halted
            }
            Stage::Story => {
                let now = (env.clock)();
                let summary = story::judge_stories(
                    env.stage(LlmTask::Story),
                    &config.llm,
                    &config.pipeline,
                    now,
                )
                .await?;
                tracing::info!(
                    judged = summary.judged,
                    failed = summary.tally.failed,
                    calls = summary.tally.calls,
                    "story stage finished"
                );
                summary.tally.halted
            }
            Stage::Review => {
                let now = (env.clock)();
                let summary =
                    review::review_profiles(env.stage(LlmTask::Score), config, now).await?;
                tracing::info!(
                    suggested = summary.suggested,
                    applied = summary.applied,
                    calls = summary.tally.calls,
                    "review stage finished"
                );
                summary.tally.halted
            }
            Stage::Tidy => {
                let now = (env.clock)();
                let summary =
                    tidy::tidy_topics(env.stage(LlmTask::Tidy), &config.llm, opts.force_tidy, now)
                        .await?;
                tracing::info!(
                    merged = summary.merged,
                    calls = summary.tally.calls,
                    "tidy stage finished"
                );
                summary.tally.halted
            }
            Stage::Embed => {
                let Some(cfg) = &config.embedding else {
                    tracing::debug!("embed stage skipped: no [embedding] settings");
                    continue;
                };
                match embed_stage(db, cfg, config.web.list_days, env.cancel, env.clock).await {
                    Ok((articles, profiles)) => tracing::info!(
                        embedded = articles.embedded,
                        failed = articles.failed,
                        profile_texts = profiles.embedded,
                        scored_users = profiles.users,
                        scored = profiles.scored,
                        calls = articles.calls + profiles.calls,
                        "embed stage finished"
                    ),
                    // 記事の embedding が作れなくても、ほかのステージは続ける（最後に報告する）
                    Err(e) => {
                        let message = crate::errors::error_chain(&e);
                        tracing::error!("embed stage stopped: {message}");
                        report.embedding_failure = Some(message);
                    }
                }
                None
            }
            Stage::Extract => {
                let summary = extract::extract_pages(
                    db,
                    fetcher,
                    sources,
                    &config.pipeline,
                    (env.clock)(),
                    env.cancel,
                )
                .await?;
                tracing::info!(
                    extracted = summary.extracted,
                    failed = summary.failed,
                    gave_up = summary.gave_up,
                    "extract stage finished"
                );
                None
            }
        };
        if let Some(task) = task {
            block(report, env.backend(task), halted);
        }
    }
    report.cancelled = env.cancel.is_requested();
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum EmbedRunError {
    #[error(transparent)]
    Client(#[from] crate::embedding::EmbedError),
    #[error(transparent)]
    Stage(#[from] embed::EmbedStageError),
}

/// 設定の API で embed ステージを流す：要約のベクトルを作り、好みのベクトルを作って利用者を採点する。
/// 鍵は環境変数から読む。百分位の基準は一覧の期間（`list_days`）の要約。
async fn embed_stage(
    db: &Db,
    cfg: &crate::config::EmbeddingConfig,
    list_days: u32,
    cancel: &Cancel,
    clock: &dyn Fn() -> DateTime<Utc>,
) -> Result<(embed::EmbedSummary, embed_profiles::ProfileSummary), EmbedRunError> {
    let client = crate::embedding::Client::from_config(cfg, |name| std::env::var(name).ok())?;
    let articles = embed::embed_articles(db, &client, cfg, cancel, clock).await?;
    let profiles =
        embed_profiles::embed_profiles(db, &client, cfg, list_days, cancel, clock).await?;
    Ok((articles, profiles))
}

/// 指定したモデルで要約か和訳を作り直す。条件に合う記事のうち、そのモデル・プロンプト版の
/// 成果物がまだ無いものだけを処理するので、途中で止めても同じ呼び出しで続きから再開できる。
/// 新しい digest ができた記事は、次の crawl で自動的に採点し直される。
pub async fn redo<L: LlmSet>(
    mut env: RunEnv<'_, L>,
    config: &Config,
    kind: RedoKind,
    model: String,
    filter: RedoFilter,
    glossary: bool,
) -> Result<RunReport, RunError> {
    let db = env.db;
    let owner = db.owner_id()?;
    let target = Target::Redo(RedoSpec {
        filter,
        user_id: owner,
        profile_hash: db.profile_hash(owner)?,
        glossary,
    });
    let mut report = RunReport::default();
    let now = (env.clock)();
    match kind {
        RedoKind::Digest => {
            let cfg = LlmConfig {
                digest_model: model,
                // redo では採点しないので、採点のための回数は残さない
                score_reserved_calls: 0,
                ..config.llm.clone()
            };
            let summary = digest::digest_articles(
                env.stage(LlmTask::Digest),
                &cfg,
                &config.pipeline,
                &target,
                now,
            )
            .await?;
            tracing::info!(
                digested = summary.digested,
                failed = summary.tally.failed,
                calls = summary.tally.calls,
                "redo digest finished"
            );
            report_halt(summary.tally.halted, &mut report.llm_failure);
        }
        RedoKind::Translate => {
            let cfg = LlmConfig {
                translate_model: model,
                ..config.llm.clone()
            };
            let summary = translate::translate_articles(
                env.stage(LlmTask::Translate),
                &cfg,
                &config.pipeline,
                owner,
                &target,
                now,
            )
            .await?;
            tracing::info!(
                translated = summary.translated,
                failed = summary.tally.failed,
                calls = summary.tally.calls,
                "redo translate finished"
            );
            report_halt(summary.tally.halted, &mut report.llm_failure);
        }
    }
    report.cancelled = env.cancel.is_requested();
    Ok(report)
}

/// `profile suggest`：反応を根拠に、プロファイルの更新案を 1 回の呼び出しで作る。案は保存しない。
/// 上限などで呼べなかったら案は `None`。
pub async fn suggest_profile<L: LlmSet>(
    mut env: RunEnv<'_, L>,
    config: &Config,
    profile: &Profile,
    evidence: &[Evidence],
) -> Result<(RunReport, Suggested), RunError> {
    let mut report = RunReport::default();
    let summary =
        suggest::suggest_profile(env.stage(LlmTask::Score), &config.llm, profile, evidence).await?;
    // 呼ばなかった理由（上限の種類）を利用者に示す。LLM の失敗と中断は report で知らせる
    let reason = match &summary.tally.halted {
        Some(Halt::Quota(stop)) => stop.to_string(),
        Some(Halt::UsageLimit { .. }) => "the subscription usage limit was reached".into(),
        Some(Halt::LlmFailed(message)) => message.clone(),
        None => "interrupted".into(),
    };
    report_halt(summary.tally.halted, &mut report.llm_failure);
    report.cancelled = summary.tally.cancelled || env.cancel.is_requested();
    let suggested = match summary.suggestion {
        Some(s) => Suggested::Profile(s),
        None => Suggested::NotAsked(reason),
    };
    Ok((report, suggested))
}

/// `profile suggest` の結果。
#[derive(Debug)]
pub enum Suggested {
    Profile(crate::prompt::suggest::Suggestion),
    /// 上限などで呼ばなかった。利用者に見せる理由
    NotAsked(String),
}

/// `eval --profile`：候補のプロファイルで、指定した記事のうちまだ採点していないものを採点する。
/// 候補は保存しない。
pub async fn eval_profile<L: LlmSet>(
    mut env: RunEnv<'_, L>,
    config: &Config,
    profile: &Profile,
    articles: &[i64],
) -> Result<RunReport, RunError> {
    let owner = env.db.owner_id()?;
    let mut report = RunReport::default();
    let now = (env.clock)();
    let summary = score::score_articles(
        env.stage(LlmTask::Score),
        &config.llm,
        &config.pipeline,
        owner,
        score::ScoreTarget::Candidate { profile, articles },
        now,
    )
    .await?;
    tracing::info!(
        scored = summary.scored,
        failed = summary.tally.failed,
        calls = summary.tally.calls,
        "candidate profile scored"
    );
    report_halt(summary.tally.halted, &mut report.llm_failure);
    report.cancelled = env.cancel.is_requested();
    Ok(report)
}

/// 止めた理由をログに出し、同じ実行でそのバックエンドをもう使わないほうがよいなら、止まった
/// バックエンドに加える（ほかのバックエンドの工程は続ける）。
fn block(report: &mut RunReport, backend: &'static str, halt: Option<Halt>) {
    if report_halt(halt, &mut report.llm_failure) && !report.llm_blocked.contains(&backend) {
        report.llm_blocked.push(backend);
    }
}

/// 止めた理由をログに出し、同じ実行で LLM をもう使わないほうがよいなら true を返す。
/// 認証切れなど利用者が対処すべき失敗は `llm_failure` に残し、最後にエラーとして報告する。
fn report_halt(halt: Option<Halt>, llm_failure: &mut Option<String>) -> bool {
    match halt {
        Some(Halt::LlmFailed(message)) => {
            *llm_failure = Some(message);
            true
        }
        Some(Halt::UsageLimit { resets_at }) => {
            tracing::warn!(?resets_at, "stopped at the subscription usage limit");
            true
        }
        Some(Halt::Quota(stop)) => {
            tracing::info!("llm work deferred: {stop}");
            false
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HttpConfig, Lang};
    use crate::db::{ContentKind, ContentOrigin, NewArticle};
    use crate::llm::fake::FakeLlm;
    use crate::llm::{LlmError, LlmResponse};
    use crate::quota::QuotaConfig;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-28T02:00:00Z")
            .unwrap()
            .to_utc()
    }

    /// 本文つきの記事を登録して id を返す。`n` ごとに URL と公開日時を変える。
    fn article(db: &Db, n: u32) -> i64 {
        let id = db
            .insert_article(&NewArticle {
                source_id: "wnn",
                url: &format!("https://e.com/{n}"),
                title: &format!("Title {n}"),
                lang: Lang::En,
                published_at: Some(&format!("2026-09-27T{:02}:00:00.000Z", 20 - n)),
            })
            .unwrap()
            .unwrap();
        db.insert_content(id, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        id
    }

    fn save_profile(db: &Db) {
        let profile = crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        db.save_profile(db.owner_id().unwrap(), &profile, now())
            .unwrap();
    }

    fn digest_ok(id: i64) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"items": [{
                "id": id, "title_ja": "題", "summary_ja": "要約", "points_ja": ["点"],
                "implications_ja": "", "lwr_relevant": true,
                "topics": ["規制・審査"], "new_topics": [],
            }]}),
            usage: None,
        })
    }

    fn score_ok(id: i64) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"items": [{
                "id": id, "score": 80, "reason": "理由", "matched": ["燃料"], "excluded": [],
            }]}),
            usage: None,
        })
    }

    fn not_logged_in() -> Result<LlmResponse, LlmError> {
        Err(LlmError::Reported {
            subtype: "error".into(),
            message: "Not logged in".into(),
        })
    }

    async fn crawl_with(
        db: &Db,
        llm: &FakeLlm,
        max_calls: u32,
        cancel: &Cancel,
        stages: &[Stage],
    ) -> RunReport {
        let mut quota = Quota::new(QuotaConfig::default(), None, Some(max_calls));
        let mut report = RunReport::default();
        crawl_part(db, llm, &mut quota, cancel, stages, &mut report).await;
        report
    }

    /// crawl はロックの単位ごとに分けて呼ぶ。クォータと報告（LLM が使えないことを含む）は
    /// 呼び出し側が持ち、単位をまたいで引き継ぐ。
    async fn crawl_part(
        db: &Db,
        llm: &FakeLlm,
        quota: &mut Quota,
        cancel: &Cancel,
        stages: &[Stage],
        report: &mut RunReport,
    ) {
        crawl(
            RunEnv {
                db,
                llm,
                quota,
                cancel,
                clock: &now,
            },
            stages,
            CrawlOptions::default(),
            &Config::default(),
            &[],
            &Fetcher::from_config(&HttpConfig::default()).unwrap(),
            report,
        )
        .await
        .unwrap();
    }

    /// crawl の見直しのステージは、評価の増えた利用者のプロファイルの案を作って保存する。
    #[tokio::test]
    async fn crawl_reviews_profiles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let early = DateTime::parse_from_rfc3339("2026-09-27T00:00:00Z")
            .unwrap()
            .to_utc();
        let profile = crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        db.save_profile(owner, &profile, early).unwrap();
        let mut responses = Vec::new();
        for n in 0..10 {
            let id = article(&db, n);
            responses.push(digest_ok(id));
            db.rate(
                owner,
                id,
                crate::db::Rating::new(if n < 5 { 5 } else { 1 }),
                now(),
            )
            .unwrap();
        }
        let llm = FakeLlm::new(responses);
        crawl_with(&db, &llm, 10, &Cancel::default(), &[Stage::Digest]).await;
        // 案が今と同じなら採点せずに残す（LLM の呼び出しは案の 1 回だけ）
        let same = serde_json::json!({
            "interests": profile.interests.iter().map(|i| serde_json::json!({
                "topic": i.topic, "weight": i.weight, "note": i.note.clone().unwrap_or_default(),
            })).collect::<Vec<_>>(),
            "exclude": profile.exclude,
            "reasons": [],
        });
        let llm = FakeLlm::new([Ok(LlmResponse {
            output: same,
            usage: None,
        })]);
        let review = Stage::from_name("review").expect("review stage");
        let report = crawl_with(&db, &llm, 10, &Cancel::default(), &[review]).await;
        assert_eq!(report, RunReport::default());
        assert_eq!(llm.requests().len(), 1);
        assert_eq!(
            db.profile_suggestions(owner).unwrap()[0].status,
            crate::db::SuggestionStatus::Unchanged
        );
    }

    /// 要約済みで採点を待つ記事と、要約を待つ記事を 1 件ずつ用意し、採点を待つ記事の id を返す。
    async fn digested_and_pending(db: &Db) -> i64 {
        save_profile(db);
        let id = article(db, 0);
        let llm = FakeLlm::new([digest_ok(id)]);
        let report = crawl_with(db, &llm, 1, &Cancel::default(), &[Stage::Digest]).await;
        assert_eq!(report, RunReport::default());
        article(db, 1);
        id
    }

    /// 上限で呼べなかったときは、どの上限で止まったかを返す（利用者に正しい理由を示すため）。
    #[tokio::test]
    async fn suggest_reports_why_it_did_not_ask() {
        let db = Db::open_in_memory().unwrap();
        let llm = FakeLlm::new([]);
        let mut quota = Quota::new(QuotaConfig::default(), None, Some(0));
        let profile = crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        let (report, suggested) = suggest_profile(
            RunEnv {
                db: &db,
                llm: &llm,
                quota: &mut quota,
                cancel: &Cancel::default(),
                clock: &now,
            },
            &Config::default(),
            &profile,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(report, RunReport::default());
        let stop = crate::quota::Stop::MaxCalls { limit: 0 }.to_string();
        assert!(
            matches!(&suggested, Suggested::NotAsked(reason) if *reason == stop),
            "{suggested:?}"
        );
    }

    /// 認証切れなどで LLM が失敗したら、同じ実行の後続の LLM ステージは呼ばず、最後に報告する。
    #[tokio::test]
    async fn llm_failure_skips_later_llm_stages() {
        let db = Db::open_in_memory().unwrap();
        digested_and_pending(&db).await;
        // 採点を待つ記事があるので、飛ばさなければ採点が呼び、用意した応答が尽きて panic する
        let llm = FakeLlm::new([not_logged_in()]);
        let report = crawl_with(
            &db,
            &llm,
            10,
            &Cancel::default(),
            &[Stage::Digest, Stage::Score, Stage::Translate, Stage::Tidy],
        )
        .await;
        assert_eq!(llm.requests().len(), 1);
        let failure = report.llm_failure.expect("the failure is reported");
        assert!(failure.contains("Not logged in"), "{failure}");
        assert!(!report.cancelled);
    }

    /// 要約だけ別のバックエンドにする。
    struct DigestApart {
        digest: FakeLlm,
        others: FakeLlm,
    }

    impl LlmSet for DigestApart {
        type Llm = FakeLlm;

        fn for_task(&self, task: LlmTask) -> &FakeLlm {
            if task == LlmTask::Digest {
                &self.digest
            } else {
                &self.others
            }
        }
    }

    /// `config` で `stages` を流す。
    async fn crawl_config(db: &Db, llm: &FakeLlm, stages: &[Stage], config: &Config) -> RunReport {
        let mut quota = Quota::new(QuotaConfig::default(), None, Some(10));
        let mut report = RunReport::default();
        crawl(
            RunEnv {
                db,
                llm,
                quota: &mut quota,
                cancel: &Cancel::default(),
                clock: &now,
            },
            stages,
            CrawlOptions::default(),
            config,
            &[],
            &Fetcher::from_config(&HttpConfig::default()).unwrap(),
            &mut report,
        )
        .await
        .unwrap();
        report
    }

    fn embedding_config(url: &str) -> Config {
        Config {
            embedding: Some(crate::embedding::fake::cfg(url)),
            ..Config::default()
        }
    }

    fn embedded(db: &Db) -> i64 {
        db.query_i64("SELECT count(*) FROM article_embeddings")
            .unwrap()
    }

    /// embed は LLM を使わないので、LLM が失敗して後続の LLM ステージを飛ばす実行でも、要約のベクトルを作る。
    #[tokio::test]
    async fn embed_runs_even_when_the_llm_is_blocked() {
        let db = Db::open_in_memory().unwrap();
        digested_and_pending(&db).await;
        let server = crate::embedding::fake::echo_server();
        let llm = FakeLlm::new([not_logged_in()]);
        let report = crawl_config(
            &db,
            &llm,
            &[Stage::Digest, Stage::Embed],
            &embedding_config(&server.url("/v1/embeddings")),
        )
        .await;
        assert!(report.llm_failure.is_some());
        assert_eq!(report.embedding_failure, None);
        assert_eq!(embedded(&db), 1);
    }

    /// embed ステージは、要約のベクトルを作った後、プロファイルのある利用者を embedding で採点する。
    #[tokio::test]
    async fn embed_scores_users_after_embedding_digests() {
        let db = Db::open_in_memory().unwrap();
        digested_and_pending(&db).await;
        let server = crate::embedding::fake::echo_server();
        let report = crawl_config(
            &db,
            &FakeLlm::new([]),
            &[Stage::Embed],
            &embedding_config(&server.url("/v1/embeddings")),
        )
        .await;
        assert_eq!(report, RunReport::default());
        assert_eq!(
            db.query_i64("SELECT count(*) FROM scores WHERE backend = 'embedding'")
                .unwrap(),
            1
        );
    }

    /// 設定が無ければ embed は何もしない。embedding の失敗は報告し、後続のステージは続ける。
    #[tokio::test]
    async fn reports_embedding_failures_and_continues() {
        let db = Db::open_in_memory().unwrap();
        let digested = digested_and_pending(&db).await;
        let report =
            crawl_config(&db, &FakeLlm::new([]), &[Stage::Embed], &Config::default()).await;
        assert_eq!(report, RunReport::default());
        let server = crate::testutil::Server::start_with(|_| crate::testutil::Route::status(503));
        let llm = FakeLlm::new([score_ok(digested)]);
        let report = crawl_config(
            &db,
            &llm,
            &[Stage::Embed, Stage::Score],
            &embedding_config(&server.url("/v1/embeddings")),
        )
        .await;
        let failure = report.embedding_failure.expect("the failure is reported");
        assert!(failure.contains("503"), "{failure}");
        assert_eq!(llm.requests().len(), 1, "score still runs");
        assert_eq!(embedded(&db), 0);
    }

    /// 工程ごとにバックエンドを変えるとき、あるバックエンドが失敗しても、ほかのバックエンドの工程は続ける。
    #[tokio::test]
    async fn a_failing_backend_does_not_stop_the_others() {
        let db = Db::open_in_memory().unwrap();
        let digested = digested_and_pending(&db).await;
        let llms = DigestApart {
            digest: FakeLlm::new([not_logged_in()]).named("claude-cli"),
            others: FakeLlm::new([score_ok(digested)]).named("copilot-cli"),
        };
        let mut quota = Quota::new(QuotaConfig::default(), None, Some(10));
        let mut report = RunReport::default();
        crawl(
            RunEnv {
                db: &db,
                llm: &llms,
                quota: &mut quota,
                cancel: &Cancel::default(),
                clock: &now,
            },
            &[Stage::Digest, Stage::Score],
            CrawlOptions::default(),
            &Config::default(),
            &[],
            &Fetcher::from_config(&HttpConfig::default()).unwrap(),
            &mut report,
        )
        .await
        .unwrap();
        assert_eq!(report.llm_blocked, ["claude-cli"]);
        assert!(report.llm_failure.is_some());
        assert_eq!(llms.others.requests().len(), 1, "score still runs");
    }

    /// ロックの単位に分けて呼んでも、前の単位で LLM が失敗していれば後の単位の LLM ステージは呼ばない。
    #[tokio::test]
    async fn llm_failure_carries_over_to_later_lock_groups() {
        let db = Db::open_in_memory().unwrap();
        digested_and_pending(&db).await;
        // 採点を待つ記事があるので、飛ばさなければ採点が呼び、用意した応答が尽きて panic する
        let llm = FakeLlm::new([not_logged_in()]);
        let mut quota = Quota::new(QuotaConfig::default(), None, Some(10));
        let mut report = RunReport::default();
        let cancel = Cancel::default();
        crawl_part(
            &db,
            &llm,
            &mut quota,
            &cancel,
            &[Stage::Digest],
            &mut report,
        )
        .await;
        crawl_part(&db, &llm, &mut quota, &cancel, &[Stage::Score], &mut report).await;
        assert_eq!(llm.requests().len(), 1);
        assert!(report.llm_failure.is_some());
    }

    /// 呼び出し回数の上限は、ロックの単位に分けて呼んでも実行全体に効く。
    #[tokio::test]
    async fn call_limit_spans_lock_groups() {
        let db = Db::open_in_memory().unwrap();
        digested_and_pending(&db).await;
        let pending = article(&db, 2);
        let llm = FakeLlm::new([digest_ok(pending)]);
        let mut quota = Quota::new(QuotaConfig::default(), None, Some(1));
        let mut report = RunReport::default();
        let cancel = Cancel::default();
        crawl_part(
            &db,
            &llm,
            &mut quota,
            &cancel,
            &[Stage::Digest],
            &mut report,
        )
        .await;
        crawl_part(&db, &llm, &mut quota, &cancel, &[Stage::Score], &mut report).await;
        assert_eq!(llm.requests().len(), 1);
    }

    /// クォータで止まるのは正常な先送りなので、後続の LLM ステージは実行する。要約を待つ記事が
    /// あっても、要約は採点のための 1 回を残して止まり、残した 1 回で採点する。
    #[tokio::test]
    async fn quota_stop_leaves_later_llm_stages_running() {
        let db = Db::open_in_memory().unwrap();
        let id = digested_and_pending(&db).await;
        let llm = FakeLlm::new([score_ok(id)]);
        let report = crawl_with(
            &db,
            &llm,
            1,
            &Cancel::default(),
            &[Stage::Digest, Stage::Score],
        )
        .await;
        let reqs = llm.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].schema,
            crate::prompt::score::schema(
                &crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap()
            )
        );
        assert_eq!(report, RunReport::default());
    }

    /// プロンプトの `<{tag} id="N"` の N を順に返す。
    fn tagged_ids(text: &str, tag: &str) -> Vec<i64> {
        let open = format!("<{tag} id=\"");
        text.split(&open)
            .skip(1)
            .filter_map(|rest| rest.split('"').next()?.parse().ok())
            .collect()
    }

    /// 依頼の内容だけから決まる応答。止めて再開しても、同じ依頼には同じ応答を返す。
    fn deterministic(req: &crate::llm::LlmRequest<'_>) -> Result<LlmResponse, LlmError> {
        let item = &req.schema["properties"]["items"]["items"]["properties"];
        let output = if req.schema["properties"].get("body_ja").is_some() {
            serde_json::json!({"body_ja": "訳文"})
        } else if item.get("summary_ja").is_some() {
            let items: Vec<_> = tagged_ids(req.prompt, "article")
                .into_iter()
                .map(|id| {
                    serde_json::json!({
                        "id": id, "title_ja": format!("規制の見出し {}", id % 3),
                        "summary_ja": "原子力規制の要約", "points_ja": ["点"],
                        "implications_ja": "", "lwr_relevant": true,
                        "topics": ["規制・審査"], "new_topics": [],
                    })
                })
                .collect();
            serde_json::json!({ "items": items })
        } else if item.get("score").is_some() {
            let items: Vec<_> = tagged_ids(req.prompt, "article")
                .into_iter()
                .map(|id| {
                    serde_json::json!({
                        "id": id, "score": 60 + id * 13 % 40, "reason": "理由",
                        "matched": [], "excluded": [],
                    })
                })
                .collect();
            serde_json::json!({ "items": items })
        } else if item.get("same").is_some() {
            // 対象ごとの候補：既存のグループ（story）とその外の記事。関係は ID の和で決める
            let items: Vec<_> = req
                .prompt
                .split("<candidates for=\"")
                .skip(1)
                .map(|block| {
                    let target: i64 = block.split('"').next().unwrap().parse().unwrap();
                    let block = block.split("</candidates>").next().unwrap();
                    let mut candidates = tagged_ids(block, "story");
                    let mut rest = block.to_string();
                    while let (Some(i), Some(j)) = (rest.find("<story id="), rest.find("</story>"))
                    {
                        rest.replace_range(i..j + "</story>".len(), "");
                    }
                    candidates.extend(tagged_ids(&rest, "article"));
                    let pick = |r| -> Vec<i64> {
                        candidates
                            .iter()
                            .copied()
                            .filter(|c| (target + c) % 3 == r)
                            .collect()
                    };
                    serde_json::json!({"id": target, "same": pick(0), "related": pick(1)})
                })
                .collect();
            serde_json::json!({ "items": items })
        } else if item.get("title_ja").is_some() {
            let items: Vec<_> = tagged_ids(req.prompt, "article")
                .into_iter()
                .map(|id| serde_json::json!({"id": id, "title_ja": format!("見出し {id}")}))
                .collect();
            serde_json::json!({ "items": items })
        } else {
            panic!("unexpected request: {}", req.schema);
        };
        Ok(LlmResponse {
            output,
            usage: None,
        })
    }

    /// 処理の結果として残るもの（ID と時刻を除く）。止めて再開した結果と比べる。
    fn outcome(db: &Db) -> Vec<String> {
        let mut rows = Vec::new();
        for (name, sql) in [
            (
                "artifact",
                "SELECT article_id || ' ' || kind || ' ' || model || ' ' || prompt_version || ' '
                        || payload FROM artifacts",
            ),
            (
                "score",
                "SELECT r.article_id || ' ' || s.score || ' ' || s.reason FROM scores AS s
                 JOIN artifacts AS r ON r.id = s.artifact_id",
            ),
            (
                "story_link",
                "SELECT r.article_id || ' ' || l.other_id || ' ' || l.relation FROM story_links AS l
                 JOIN artifacts AS r ON r.id = l.artifact_id",
            ),
            (
                "story",
                "SELECT article_id || ' ' || story_id FROM article_stories",
            ),
            (
                "stage_error",
                "SELECT article_id || ' ' || stage || ' ' || attempts || ' ' || last_error
                 FROM stage_errors",
            ),
            ("claim", "SELECT article_id || ' ' || stage FROM work_claims"),
        ] {
            let mut got = db.query_strings(sql).unwrap();
            got.sort();
            rows.extend(got.into_iter().map(|r| format!("{name} {r}")));
        }
        rows
    }

    /// 要約から同じ報道の判定までを止めずに走らせた結果と、k 回目の LLM の呼び出しで止める指示を
    /// 出してから次の実行で再開した結果が、どの k でも、応答の前後どちらで止まっても同じになる。
    #[tokio::test]
    async fn resuming_after_a_stop_matches_an_uninterrupted_run() {
        const STAGES: &[Stage] = &[
            Stage::Digest,
            Stage::Score,
            Stage::Translate,
            Stage::Title,
            Stage::Story,
        ];
        let setup = || {
            let db = Db::open_in_memory().unwrap();
            save_profile(&db);
            for n in 0..8 {
                article(&db, n);
            }
            // 本文の無い英語の記事は、見出しだけ訳す
            for n in 8..10 {
                db.insert_article(&NewArticle {
                    source_id: "ans",
                    url: &format!("https://e.com/{n}"),
                    title: &format!("Title {n}"),
                    lang: Lang::En,
                    published_at: Some(&format!("2026-09-27T{:02}:00:00.000Z", 20 - n)),
                })
                .unwrap()
                .unwrap();
            }
            db
        };

        let db = setup();
        let llm = FakeLlm::responding(std::time::Duration::ZERO, deterministic);
        let report = crawl_with(&db, &llm, 100, &Cancel::default(), STAGES).await;
        assert!(!report.cancelled, "{report:?}");
        let calls = llm.requests().len();
        assert!(calls >= STAGES.len(), "{calls} calls");
        let expected = outcome(&db);

        // 応答が届いてから止める指示に気づく場合（遅延なし）と、応答を待つ間に止めて呼び出しを捨てる場合
        for (delay, when) in [
            (std::time::Duration::ZERO, "after the response"),
            (std::time::Duration::from_millis(5), "during the call"),
        ] {
            for k in 0..calls {
                let db = setup();
                let cancel = Cancel::default();
                let stop = cancel.clone();
                let llm = FakeLlm::responding(delay, deterministic).hooked(move |n| {
                    if n == k {
                        stop.request();
                    }
                });
                let report = crawl_with(&db, &llm, 100, &cancel, STAGES).await;
                assert!(report.cancelled, "stop at call {k} {when}: {report:?}");
                let llm = FakeLlm::responding(std::time::Duration::ZERO, deterministic);
                crawl_with(&db, &llm, 100, &Cancel::default(), STAGES).await;
                assert_eq!(outcome(&db), expected, "stop at call {k} {when}");
            }
        }
    }

    /// 止める指示が出ていれば、ステージを始めずに中断として報告する。
    #[tokio::test]
    async fn cancel_stops_before_any_stage() {
        let db = Db::open_in_memory().unwrap();
        article(&db, 0);
        let cancel = Cancel::default();
        cancel.request();
        let llm = FakeLlm::new([]);
        let report = crawl_with(&db, &llm, 10, &cancel, &[Stage::Digest, Stage::Score]).await;
        assert!(llm.requests().is_empty());
        assert!(report.cancelled);
    }

    /// redo は採点しないので、採点のための回数を残さず、指定したモデルで要約する。
    #[tokio::test]
    async fn redo_digests_with_the_given_model_without_score_reserve() {
        let db = Db::open_in_memory().unwrap();
        save_profile(&db);
        let id = article(&db, 0);
        let llm = FakeLlm::new([digest_ok(id)]);
        // 呼べるのは 1 回だけ。採点のための回数（既定で 1）を残せば要約できない
        let mut quota = Quota::new(QuotaConfig::default(), None, Some(1));
        let report = redo(
            RunEnv {
                db: &db,
                llm: &llm,
                quota: &mut quota,
                cancel: &Cancel::default(),
                clock: &now,
            },
            &Config::default(),
            RedoKind::Digest,
            "opus".into(),
            RedoFilter::default(),
            false,
        )
        .await
        .unwrap();
        let reqs = llm.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].model, "opus");
        assert_eq!(report, RunReport::default());
    }
}
