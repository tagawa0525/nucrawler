//! `crawl`: 取得から採点までのパイプラインを流す。

use std::path::PathBuf;

use nucrawler::cli;
use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::http::Fetcher;
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::lock::{self, LockError};
use nucrawler::pipeline::run::{self, CrawlOptions, RunEnv};
use nucrawler::pipeline::{self, Cancel, Stage};
use nucrawler::quota::Quota;

use crate::{Error, config_dir, data_dir};

use super::{finish, spawn_signal_handler};

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
    // LLM のステージで共有する。呼び出し回数や時間帯の上限は、この実行全体に効く。
    let llm = ClaudeCli::from_config(&config.llm, data.join("llm-cwd"));
    let mut quota = Quota::new(
        config.quota.clone(),
        db.latest_rate_limit()?,
        args.max_llm_calls,
    );
    let report = run::crawl(
        RunEnv {
            db: &db,
            llm: &llm,
            quota: &mut quota,
            cancel: &cancel,
            clock: &chrono::Utc::now,
        },
        &stages,
        CrawlOptions {
            requests_only: args.requests_only,
            force_tidy: args.only == Some(Stage::Tidy),
        },
        &config,
        &sources.sources,
        &fetcher,
    )
    .await?;
    finish(report)
}
