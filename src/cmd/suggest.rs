//! `profile suggest`：反応を根拠に LLM がプロファイルの更新案を作り、差分と根拠を表示して `--out` に書く。
//! 案は取り込まない（`eval --profile` で比べてから `profile import` する）。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::lock;
use nucrawler::pipeline::run::{self, RunEnv};
use nucrawler::profile;
use nucrawler::quota::Quota;
use nucrawler::suggest;

use crate::{Error, config_dir, data_dir};

use super::{finish, spawn_signal_handler};

/// 正例・負例のどちらかがこれより少なければ、案は参考程度だと添える（`eval` と同じ）
const FEW_LABELS: usize = 5;

pub(crate) async fn suggest(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    out: &Path,
    max_llm_calls: Option<u32>,
) -> Result<(), Error> {
    // 手元の TOML を上書きしないよう、LLM を呼ぶ前に確かめる（書くときも新規作成に限る）
    if out.exists() {
        return Err(Error::OutputExists(out.to_path_buf()));
    }
    let (config, _) = config::load(&config_dir(config)?)?;
    let data = data_dir(data)?;
    let _lock = lock::acquire(&data)?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let owner = db.owner_id()?;
    let (current, _) = db.load_profile(owner)?.ok_or(Error::NoProfile)?;
    let evidence = db.label_evidence(owner)?;
    if evidence.is_empty() {
        return Err(Error::NoLabels);
    }
    let positive = evidence.iter().filter(|e| e.positive).count();
    if positive < FEW_LABELS || evidence.len() - positive < FEW_LABELS {
        println!(
            "note: {positive} positive and {} negative reactions; treat the suggestion as rough",
            evidence.len() - positive
        );
    }
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let llm = ClaudeCli::from_config(&config.llm, data.join("llm-cwd"));
    let mut quota = Quota::new(config.quota.clone(), db.latest_rate_limit()?, max_llm_calls);
    let (report, suggestion) = run::suggest_profile(
        RunEnv {
            db: &db,
            llm: &llm,
            quota: &mut quota,
            cancel: &cancel,
            clock: &chrono::Utc::now,
        },
        &config,
        &current,
        &evidence,
    )
    .await?;
    finish(report)?;
    let Some(suggestion) = suggestion else {
        println!("no suggestion: the llm call limit was reached");
        return Ok(());
    };
    let Some(text) = suggest::render(&current, &suggestion, &out.display().to_string()) else {
        println!("no changes suggested");
        return Ok(());
    };
    let write = |path: &Path| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(profile::to_toml(&suggestion.profile).as_bytes())
    };
    write(out).map_err(|source| Error::WriteFile {
        path: out.to_path_buf(),
        source,
    })?;
    print!("{text}");
    Ok(())
}
