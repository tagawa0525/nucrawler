//! ステージを順に流す：`crawl` は計画したステージを実行し、`redo` は要約か和訳を作り直す。
//! LLM が使えなくなった後の扱いと、止めた理由の報告をここで決める。ロック・シグナル・終了コードは
//! 呼び出し側（バイナリ）で扱う。

use chrono::{DateTime, Utc};

use super::llm_call::LlmStage;
use super::{Cancel, Halt, RedoSpec, Stage, Target};
use super::{digest, extract, fetch, score, suggest, tidy, translate};
use crate::cli::RedoKind;
use crate::config::{Config, LlmConfig, Source};
use crate::db::{Db, DbError, Evidence, RedoFilter};
use crate::http::Fetcher;
use crate::llm::Llm;
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
    Suggest(#[from] suggest::SuggestStageError),
}

/// 1 回の実行で共有する環境。クォータは実行全体に効くので、ステージ間で引き継ぐ。
pub struct RunEnv<'a, L> {
    pub db: &'a Db,
    pub llm: &'a L,
    pub quota: &'a mut Quota,
    pub cancel: &'a Cancel,
    /// ステージを始めるたびに今の時刻を読む（テストでは固定する）
    pub clock: &'a dyn Fn() -> DateTime<Utc>,
}

impl<L> RunEnv<'_, L> {
    fn stage(&mut self) -> LlmStage<'_, L> {
        LlmStage {
            db: self.db,
            llm: self.llm,
            quota: self.quota,
            cancel: self.cancel,
        }
    }
}

/// 実行の結果。どれをエラーや終了コードにするかは呼び出し側が決める。
#[derive(Debug, Default, PartialEq)]
pub struct RunReport {
    pub failed_sources: usize,
    /// 認証切れなど、利用者が対処すべき LLM の失敗
    pub llm_failure: Option<String>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CrawlOptions {
    /// 和訳は依頼されたものだけを処理する
    pub requests_only: bool,
    /// 前回の整理からの間隔によらず、語彙を整理する（`--only tidy`）
    pub force_tidy: bool,
}

pub async fn crawl<L: Llm>(
    mut env: RunEnv<'_, L>,
    stages: &[Stage],
    opts: CrawlOptions,
    config: &Config,
    sources: &[Source],
    fetcher: &Fetcher,
) -> Result<RunReport, RunError> {
    let db = env.db;
    let mut report = RunReport::default();
    // 上限到達や LLM の失敗の後は、同じ実行の中で後続の LLM ステージを試さない
    let mut llm_blocked = false;
    for &stage in stages {
        if env.cancel.is_requested() {
            break;
        }
        match stage {
            Stage::Fetch => {
                let summary =
                    fetch::fetch_sources(db, fetcher, sources, env.cancel, (env.clock)()).await?;
                tracing::info!(
                    new_articles = summary.new_articles,
                    failed_sources = summary.failed_sources.len(),
                    "fetch stage finished"
                );
                report.failed_sources += summary.failed_sources.len();
            }
            Stage::Digest | Stage::Score | Stage::Translate | Stage::Tidy if llm_blocked => {
                tracing::warn!(
                    stage = stage.name(),
                    "skipped: the llm is unavailable in this run"
                );
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
                    env.stage(),
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
                    failed = summary.failed,
                    calls = summary.calls,
                    "digest stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut report.llm_failure);
            }
            Stage::Score => {
                let now = (env.clock)();
                let summary = score::score_articles(
                    env.stage(),
                    &config.llm,
                    &config.pipeline,
                    db.owner_id()?,
                    score::ScoreTarget::Saved,
                    now,
                )
                .await?;
                tracing::info!(
                    scored = summary.scored,
                    failed = summary.failed,
                    calls = summary.calls,
                    "score stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut report.llm_failure);
            }
            Stage::Translate => {
                let now = (env.clock)();
                let summary = translate::translate_articles(
                    env.stage(),
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
                    failed = summary.failed,
                    calls = summary.calls,
                    "translate stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut report.llm_failure);
            }
            Stage::Tidy => {
                let now = (env.clock)();
                let summary =
                    tidy::tidy_topics(env.stage(), &config.llm, opts.force_tidy, now).await?;
                tracing::info!(
                    merged = summary.merged,
                    calls = summary.calls,
                    "tidy stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut report.llm_failure);
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
            }
        }
    }
    report.cancelled = env.cancel.is_requested();
    Ok(report)
}

/// 指定したモデルで要約か和訳を作り直す。条件に合う記事のうち、そのモデル・プロンプト版の
/// 成果物がまだ無いものだけを処理するので、途中で止めても同じ呼び出しで続きから再開できる。
/// 新しい digest ができた記事は、次の crawl で自動的に採点し直される。
pub async fn redo<L: Llm>(
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
            let summary =
                digest::digest_articles(env.stage(), &cfg, &config.pipeline, &target, now).await?;
            tracing::info!(
                digested = summary.digested,
                failed = summary.failed,
                calls = summary.calls,
                "redo digest finished"
            );
            report_halt(summary.halted, &mut report.llm_failure);
        }
        RedoKind::Translate => {
            let cfg = LlmConfig {
                translate_model: model,
                ..config.llm.clone()
            };
            let summary = translate::translate_articles(
                env.stage(),
                &cfg,
                &config.pipeline,
                owner,
                &target,
                now,
            )
            .await?;
            tracing::info!(
                translated = summary.translated,
                failed = summary.failed,
                calls = summary.calls,
                "redo translate finished"
            );
            report_halt(summary.halted, &mut report.llm_failure);
        }
    }
    report.cancelled = env.cancel.is_requested();
    Ok(report)
}

/// `profile suggest`：反応を根拠に、プロファイルの更新案を 1 回の呼び出しで作る。案は保存しない。
/// 上限などで呼べなかったら案は `None`。
pub async fn suggest_profile<L: Llm>(
    mut env: RunEnv<'_, L>,
    config: &Config,
    profile: &Profile,
    evidence: &[Evidence],
) -> Result<(RunReport, Suggested), RunError> {
    let mut report = RunReport::default();
    let now = (env.clock)();
    let summary =
        suggest::suggest_profile(env.stage(), &config.llm, profile, evidence, now).await?;
    // 呼ばなかった理由（上限の種類）を利用者に示す。LLM の失敗と中断は report で知らせる
    let reason = match &summary.halted {
        Some(Halt::Quota(stop)) => stop.to_string(),
        Some(Halt::UsageLimit { .. }) => "the subscription usage limit was reached".into(),
        Some(Halt::LlmFailed(message)) => message.clone(),
        None => "interrupted".into(),
    };
    report_halt(summary.halted, &mut report.llm_failure);
    report.cancelled = summary.cancelled || env.cancel.is_requested();
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
pub async fn eval_profile<L: Llm>(
    mut env: RunEnv<'_, L>,
    config: &Config,
    profile: &Profile,
    articles: &[i64],
) -> Result<RunReport, RunError> {
    let owner = env.db.owner_id()?;
    let mut report = RunReport::default();
    let now = (env.clock)();
    let summary = score::score_articles(
        env.stage(),
        &config.llm,
        &config.pipeline,
        owner,
        score::ScoreTarget::Candidate { profile, articles },
        now,
    )
    .await?;
    tracing::info!(
        scored = summary.scored,
        failed = summary.failed,
        calls = summary.calls,
        "candidate profile scored"
    );
    report_halt(summary.halted, &mut report.llm_failure);
    report.cancelled = env.cancel.is_requested();
    Ok(report)
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
            rate_limit: None,
        })
    }

    fn score_ok(id: i64) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            output: serde_json::json!({"items": [{
                "id": id, "score": 80, "reason": "理由", "matched": ["燃料"], "excluded": [],
            }]}),
            rate_limit: None,
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
        crawl(
            RunEnv {
                db,
                llm,
                quota: &mut quota,
                cancel,
                clock: &now,
            },
            stages,
            CrawlOptions::default(),
            &Config::default(),
            &[],
            &Fetcher::from_config(&HttpConfig::default()).unwrap(),
        )
        .await
        .unwrap()
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
