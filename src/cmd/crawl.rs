//! `crawl`: 取得から採点までのパイプラインを流す。

use std::path::PathBuf;

use nucrawler::cli;
use nucrawler::config::{self, LlmConfig};
use nucrawler::db::Db;
use nucrawler::http::Fetcher;
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::digest;
use nucrawler::pipeline::extract;
use nucrawler::pipeline::fetch;
use nucrawler::pipeline::llm_call::LlmStage;
use nucrawler::pipeline::lock::{self, LockError};
use nucrawler::pipeline::score;
use nucrawler::pipeline::tidy;
use nucrawler::pipeline::translate;
use nucrawler::pipeline::{self, Cancel, Stage, Target};
use nucrawler::quota::Quota;

use crate::{Error, config_dir, data_dir};

use super::{report_halt, spawn_signal_handler};

pub(crate) async fn crawl(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    args: &cli::CrawlArgs,
) -> Result<(), Error> {
    let stages = if args.requests_only {
        vec![Stage::Translate]
    } else {
        pipeline::plan(args.until, args.only)
    };
    let (config, sources) = config::load(&config_dir(config)?)?;
    let data = data_dir(data)?;
    let _lock = match lock::acquire(&data) {
        Err(LockError::Held { .. }) if args.wait_lock => {
            tracing::info!("waiting for another crawl to finish");
            // まだ他のタスクを始めていないので、ここでスレッドをブロックしてよい
            lock::acquire_waiting(&data)?
        }
        lock => lock?,
    };
    let db = Db::open(&data.join("nucrawler.db"))?;
    let fetcher = Fetcher::from_config(&config.http)?;
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    // 認証切れなど、利用者が対処すべき LLM の失敗（最後にエラーとして報告する）
    let mut llm_failure = None;
    // LLM のステージで共有する。呼び出し回数や時間帯の上限は、この実行全体に効く。
    let llm = ClaudeCli {
        command: config.llm.command.clone().into(),
        cwd: data.join("llm-cwd"),
        timeout: std::time::Duration::from_secs(config.llm.timeout_secs),
    };
    let mut quota = Quota::new(
        config.quota.clone(),
        db.latest_rate_limit()?,
        args.max_llm_calls,
    );
    // 上限到達や LLM の失敗の後は、同じ実行の中で後続の LLM ステージを試さない
    let mut llm_blocked = false;

    let mut failed_sources = 0;
    for &stage in &stages {
        if cancel.is_requested() {
            break;
        }
        match stage {
            Stage::Fetch => {
                let summary =
                    fetch::fetch_sources(&db, &fetcher, &sources.sources, &cancel).await?;
                tracing::info!(
                    new_articles = summary.new_articles,
                    failed_sources = summary.failed_sources.len(),
                    "fetch stage finished"
                );
                failed_sources += summary.failed_sources.len();
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
                    score_reserved_calls: pipeline::score_reserve(
                        &stages,
                        &config.llm,
                        db.load_profile(db.owner_id()?)?.is_some(),
                    ),
                    ..config.llm.clone()
                };
                let summary = digest::digest_articles(
                    LlmStage {
                        db: &db,
                        llm: &llm,
                        quota: &mut quota,
                        cancel: &cancel,
                    },
                    &digest_cfg,
                    &config.pipeline,
                    &Target::Pending {
                        requests_only: false,
                    },
                    chrono::Utc::now(),
                )
                .await?;
                tracing::info!(
                    digested = summary.digested,
                    failed = summary.failed,
                    calls = summary.calls,
                    "digest stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut llm_failure);
            }
            Stage::Score => {
                let summary = score::score_articles(
                    LlmStage {
                        db: &db,
                        llm: &llm,
                        quota: &mut quota,
                        cancel: &cancel,
                    },
                    &config.llm,
                    &config.pipeline,
                    db.owner_id()?,
                    chrono::Utc::now(),
                )
                .await?;
                tracing::info!(
                    scored = summary.scored,
                    failed = summary.failed,
                    calls = summary.calls,
                    "score stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut llm_failure);
            }
            Stage::Translate => {
                let summary = translate::translate_articles(
                    LlmStage {
                        db: &db,
                        llm: &llm,
                        quota: &mut quota,
                        cancel: &cancel,
                    },
                    &config.llm,
                    &config.pipeline,
                    db.owner_id()?,
                    &Target::Pending {
                        requests_only: args.requests_only,
                    },
                    chrono::Utc::now(),
                )
                .await?;
                tracing::info!(
                    translated = summary.translated,
                    failed = summary.failed,
                    calls = summary.calls,
                    "translate stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut llm_failure);
            }
            Stage::Tidy => {
                let summary = tidy::tidy_topics(
                    LlmStage {
                        db: &db,
                        llm: &llm,
                        quota: &mut quota,
                        cancel: &cancel,
                    },
                    &config.llm,
                    // `--only tidy` なら、前回の整理からの間隔によらず整理する
                    args.only == Some(Stage::Tidy),
                    chrono::Utc::now(),
                )
                .await?;
                tracing::info!(
                    merged = summary.merged,
                    calls = summary.calls,
                    "tidy stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut llm_failure);
            }
            Stage::Extract => {
                let summary = extract::extract_pages(
                    &db,
                    &fetcher,
                    &sources.sources,
                    &config.pipeline,
                    chrono::Utc::now(),
                    &cancel,
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
    if cancel.is_requested() {
        return Err(Error::Interrupted);
    }
    if let Some(message) = llm_failure {
        return Err(Error::LlmFailed(message));
    }
    if failed_sources > 0 {
        return Err(Error::SourcesFailed(failed_sources));
    }
    Ok(())
}
