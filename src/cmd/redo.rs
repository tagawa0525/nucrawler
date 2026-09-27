//! `redo`: 要約か和訳を指定したモデルで作り直す。

use std::path::PathBuf;

use nucrawler::cli::RedoArgs;
use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::lock;
use nucrawler::pipeline::run::{self, RunEnv};
use nucrawler::quota::Quota;

use crate::{Error, config_dir, data_dir};

use super::{finish, spawn_signal_handler};

/// 途中で止めても、同じコマンドで続きから再開できる（`pipeline::run::redo`）。
pub(crate) async fn redo(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    args: RedoArgs,
) -> Result<(), Error> {
    let (config, _) = config::load(&config_dir(config)?)?;
    let data = data_dir(data)?;
    let _lock = lock::acquire(&data)?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let llm = ClaudeCli::from_config(&config.llm, data.join("llm-cwd"));
    let mut quota = Quota::new(
        config.quota.clone(),
        db.latest_rate_limit()?,
        args.max_llm_calls,
    );
    let report = run::redo(
        RunEnv {
            db: &db,
            llm: &llm,
            quota: &mut quota,
            cancel: &cancel,
            clock: &chrono::Utc::now,
        },
        &config,
        args.kind,
        args.model,
        args.filter,
        args.glossary,
    )
    .await?;
    finish(report)
}
