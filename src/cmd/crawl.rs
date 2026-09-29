//! `crawl`: 取得から採点までのパイプラインを流す。

use std::path::{Path, PathBuf};

use nucrawler::cli;
use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::http::Fetcher;
use nucrawler::llm::Backend;
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
    let llm = Backend::from_config(&config.llm, &data);
    // クォータと報告（LLM が使えないことを含む）は実行全体で 1 つにし、ロックの単位をまたいで
    // 引き継ぐ（呼び出し回数の上限と、LLM の失敗の後に後続の LLM ステージを呼ばないことが効くように）。
    // 使用率は判定のたびに DB から読むので、ここでは読まない
    let mut quota = Quota::from_config(&config, args.max_llm_calls);
    let mut report = RunReport::default();
    // 単位ごとに、その単位のロックを取ってから実行する。複数のロックを同時には持たないので、
    // 他の実行と互いに待ち合って止まることはない
    for (kind, group) in pipeline::lock_groups(&stages) {
        let Some(_lock) = acquire(&data, kind, args.wait_lock, &cancel).await? else {
            report.cancelled = true;
            break;
        };
        // DB はロックを取ってから開く。更新前の版の実行が使っている間にマイグレーションを当てないため
        let db = Db::open(&data.join("nucrawler.db"))?;
        run::crawl(
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
            &mut report,
        )
        .await?;
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
