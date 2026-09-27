//! `eval`：採点が利用者の反応とどれだけ合っているかを表示する。`--profile` なら、候補の
//! プロファイルでラベルの付いた記事を採点してから、現行と並べる。

use std::path::PathBuf;

use nucrawler::cli::EvalArgs;
use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::eval;
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::lock;
use nucrawler::pipeline::run::{self, RunEnv};
use nucrawler::profile;
use nucrawler::prompt;
use nucrawler::quota::Quota;

use crate::{Error, config_dir, data_dir};

use super::{finish, spawn_signal_handler};

pub(crate) async fn eval(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    args: EvalArgs,
) -> Result<(), Error> {
    let data = data_dir(data)?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let owner = db.owner_id()?;
    let candidate = match &args.profile {
        Some(file) => {
            let text = std::fs::read_to_string(file).map_err(|source| Error::ReadFile {
                path: file.clone(),
                source,
            })?;
            let candidate = profile::parse(&text)?;
            score_candidate(config, &data, &db, &candidate, args.max_llm_calls).await?;
            Some(profile::hash(&candidate))
        }
        None => None,
    };
    let current = db.profile_hash(owner)?;
    print!(
        "{}",
        eval::render(
            &db.eval_labels(owner)?,
            &db.eval_scores(owner)?,
            current.as_deref(),
            candidate.as_deref(),
            prompt::score::PROMPT_VERSION,
            args.all,
        )
    );
    Ok(())
}

/// ラベルの付いた記事を候補のプロファイルで採点する。ロック・クォータ・シグナルは `redo` と同じ。
async fn score_candidate(
    config: Option<PathBuf>,
    data: &std::path::Path,
    db: &Db,
    candidate: &profile::Profile,
    max_llm_calls: Option<u32>,
) -> Result<(), Error> {
    let (config, _) = config::load(&config_dir(config)?)?;
    let _lock = lock::acquire(data)?;
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let llm = ClaudeCli::from_config(&config.llm, data.join("llm-cwd"));
    let mut quota = Quota::new(config.quota.clone(), db.latest_rate_limit()?, max_llm_calls);
    let articles: Vec<i64> = db
        .eval_labels(db.owner_id()?)?
        .iter()
        .map(|l| l.article_id)
        .collect();
    let report = run::eval_profile(
        RunEnv {
            db,
            llm: &llm,
            quota: &mut quota,
            cancel: &cancel,
            clock: &chrono::Utc::now,
        },
        &config,
        candidate,
        &articles,
    )
    .await?;
    finish(report)
}
