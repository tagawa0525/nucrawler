//! `eval`：採点が利用者の反応とどれだけ合っているかを表示する。`--profile` なら、候補の
//! プロファイルでラベルの付いた記事を採点してから、現行と並べる。

use std::path::PathBuf;

use nucrawler::cli::EvalArgs;
use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::eval;
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::lock::{self, LockKind};
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
    // 候補のファイルと設定の誤りは、ロックを取る前に知らせる
    let candidate = args.profile.as_deref().map(read_profile).transpose()?;
    let (config, _) = config::load(&config_dir(config)?)?;
    let data = data_dir(data)?;
    // 候補で採点するときは LLM を呼んで DB に書くので、redo と同じく DB を開く前にロックを取る
    let _lock = candidate
        .is_some()
        .then(|| lock::acquire(&data, LockKind::Llm))
        .transpose()?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let owner = db.owner_id()?;
    if let Some(candidate) = &candidate {
        score_candidate(&config, &data, &db, candidate, args.max_llm_calls).await?;
    }
    let candidate = candidate.as_ref().map(profile::hash);
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
            config.recommend.prior_strength,
        )
    );
    print!("{}", eval::render_explore(db.explore_stats(owner)?));
    Ok(())
}

fn read_profile(file: &std::path::Path) -> Result<profile::Profile, Error> {
    let text = std::fs::read_to_string(file).map_err(|source| Error::ReadFile {
        path: file.to_path_buf(),
        source,
    })?;
    Ok(profile::parse(&text)?)
}

/// ラベルの付いた記事を候補のプロファイルで採点する。クォータ・シグナルは `redo` と同じ。
/// ロックは呼び出し側が取る。
async fn score_candidate(
    config: &config::Config,
    data: &std::path::Path,
    db: &Db,
    candidate: &profile::Profile,
    max_llm_calls: Option<u32>,
) -> Result<(), Error> {
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let llm = ClaudeCli::from_config(&config.llm, data.join("llm-cwd"), data.to_path_buf());
    let mut quota = Quota::new(config.quota.clone(), None, max_llm_calls);
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
        config,
        candidate,
        &articles,
    )
    .await?;
    finish(report)
}
