//! `crawl`: 取得から採点までのパイプラインを流す。

use std::path::{Path, PathBuf};

use nucrawler::cli;
use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::http::Fetcher;
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::lock::{self, Lock, LockError, LockKind};
use nucrawler::pipeline::run::{self, CrawlOptions, RunEnv, RunReport};
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
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let fetcher = Fetcher::from_config(&config.http)?;
    let llm = ClaudeCli::from_config(&config.llm, data.join("llm-cwd"), data.clone());
    let mut report = RunReport::default();
    // 取得と LLM のステージはロックが別なので、取得を終えてから LLM のロックを取る。
    // 両方を同時には持たないので、他の実行と互いに待ち合って止まることはない
    for (kind, group) in pipeline::lock_groups(&stages) {
        let Some(_lock) = acquire(&data, kind, args.wait_lock, &cancel).await? else {
            report.cancelled = true;
            break;
        };
        // DB はロックを取ってから開く。更新前の版の実行が使っている間にマイグレーションを当てないため
        let db = Db::open(&data.join("nucrawler.db"))?;
        // 使用率はロックを取ってから読む。待っている間に他の実行が呼んだ分も判定に入れるため。
        // LLM のステージは 1 つの単位にまとまるので、呼び出し回数の上限はこの実行全体に効く
        let mut quota = Quota::new(
            config.quota.clone(),
            db.latest_rate_limit()?,
            args.max_llm_calls,
        );
        let part = run::crawl(
            RunEnv {
                db: &db,
                llm: &llm,
                quota: &mut quota,
                cancel: &cancel,
                clock: &chrono::Utc::now,
            },
            &group,
            CrawlOptions {
                requests_only: args.requests_only,
                force_tidy: args.only == Some(Stage::Tidy),
            },
            &config,
            &sources.sources,
            &fetcher,
        )
        .await?;
        report.failed_sources += part.failed_sources;
        report.llm_failure = report.llm_failure.or(part.llm_failure);
        report.cancelled = part.cancelled;
        if report.cancelled {
            break;
        }
    }
    finish(report)
}

/// 他の実行が持っていれば、`wait` なら終わるまで待つ。待っている間に止められたら `None`。
async fn acquire(
    data: &Path,
    kind: LockKind,
    wait: bool,
    cancel: &Cancel,
) -> Result<Option<Lock>, LockError> {
    match lock::acquire(data, kind) {
        Err(LockError::Held { .. }) if wait => {
            tracing::info!(?kind, "waiting for another run to release the lock");
            lock::acquire_waiting(data, kind, cancel).await
        }
        lock => lock.map(Some),
    }
}
