//! `redo`: 要約か和訳を指定したモデルで作り直す。

use std::path::PathBuf;

use nucrawler::cli::RedoArgs;
use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::llm::Backend;
use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::lock::{self, LockKind};
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
    let _lock = lock::acquire(&data, LockKind::Llm)?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let llm = Backend::from_config(&config.llm, &data);
    let mut quota = Quota::from_config(&config, args.max_llm_calls);
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
