//! ステージを順に流す：`crawl` は計画したステージを実行し、`redo` は要約か和訳を作り直す。
//! LLM が使えなくなった後の扱いと、止めた理由の報告をここで決める。ロック・シグナル・終了コードは
//! 呼び出し側（バイナリ）で扱う。

use chrono::{DateTime, Utc};

use super::llm_call::LlmStage;
use super::{Cancel, Halt, RedoSpec, Stage, Target};
use super::{digest, extract, fetch, score, tidy, translate};
use crate::cli::RedoKind;
use crate::config::{Config, LlmConfig, Source};
use crate::db::{Db, DbError, RedoFilter};
use crate::http::Fetcher;
use crate::llm::Llm;
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
                let summary = fetch::fetch_sources(db, fetcher, sources, env.cancel).await?;
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
