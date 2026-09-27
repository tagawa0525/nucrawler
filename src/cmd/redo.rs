//! `redo`: 要約か和訳を指定したモデルで作り直す。

use std::path::PathBuf;

use nucrawler::cli::{RedoArgs, RedoKind};
use nucrawler::config::{self, LlmConfig};
use nucrawler::db::Db;
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::digest;
use nucrawler::pipeline::llm_call::LlmStage;
use nucrawler::pipeline::lock;
use nucrawler::pipeline::translate;
use nucrawler::pipeline::{self, Cancel, Target};
use nucrawler::quota::Quota;

use crate::{Error, config_dir, data_dir};

use super::{report_halt, spawn_signal_handler};

/// 指定したモデルで要約か和訳を作り直す。条件に合う記事のうち、そのモデル・プロンプト版の
/// 成果物がまだ無いものだけを処理するので、途中で止めても同じコマンドで続きから再開できる。
/// 新しい digest ができた記事は、次の crawl で自動的に採点し直される。
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
    let owner = db.owner_id()?;
    let target = Target::Redo(pipeline::RedoSpec {
        filter: args.filter,
        user_id: owner,
        profile_hash: db.profile_hash(owner)?,
        glossary: args.glossary,
    });
    let mut llm_failure = None;
    match args.kind {
        RedoKind::Digest => {
            let cfg = LlmConfig {
                digest_model: args.model,
                // redo では採点しないので、採点のための回数は残さない
                score_reserved_calls: 0,
                ..config.llm.clone()
            };
            let summary = digest::digest_articles(
                LlmStage {
                    db: &db,
                    llm: &llm,
                    quota: &mut quota,
                    cancel: &cancel,
                },
                &cfg,
                &config.pipeline,
                &target,
                chrono::Utc::now(),
            )
            .await?;
            tracing::info!(
                digested = summary.digested,
                failed = summary.failed,
                calls = summary.calls,
                "redo digest finished"
            );
            report_halt(summary.halted, &mut llm_failure);
        }
        RedoKind::Translate => {
            let cfg = LlmConfig {
                translate_model: args.model,
                ..config.llm.clone()
            };
            let summary = translate::translate_articles(
                LlmStage {
                    db: &db,
                    llm: &llm,
                    quota: &mut quota,
                    cancel: &cancel,
                },
                &cfg,
                &config.pipeline,
                owner,
                &target,
                chrono::Utc::now(),
            )
            .await?;
            tracing::info!(
                translated = summary.translated,
                failed = summary.failed,
                calls = summary.calls,
                "redo translate finished"
            );
            report_halt(summary.halted, &mut llm_failure);
        }
    }
    if cancel.is_requested() {
        return Err(Error::Interrupted);
    }
    if let Some(message) = llm_failure {
        return Err(Error::LlmFailed(message));
    }
    Ok(())
}
