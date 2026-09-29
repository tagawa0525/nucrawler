//! `profile suggest`：反応を根拠に LLM がプロファイルの更新案を作り、差分と根拠を表示して `--out` に書く。
//! 案は取り込まない（`eval --profile` で比べてから `profile import` する）。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use nucrawler::config;
use nucrawler::db::Db;
use nucrawler::llm::Backends;
use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::lock::{self, LockKind};
use nucrawler::pipeline::run::{self, RunEnv, Suggested};
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
    let _lock = lock::acquire(&data, LockKind::Llm)?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let owner = db.owner_id()?;
    let (current, _) = db.load_profile(owner)?.ok_or(Error::NoProfile)?;
    let evidence = db.label_evidence(owner)?;
    if evidence.is_empty() {
        // 評価はあっても、どの記事にも閲覧できる要約が無ければ根拠にできない
        return Err(if db.eval_labels(owner)?.is_empty() {
            Error::NoLabels
        } else {
            Error::NoEvidence
        });
    }
    let positive = evidence.iter().filter(|e| e.rating.is_positive()).count();
    let negative = evidence.iter().filter(|e| e.rating.is_negative()).count();
    if positive < FEW_LABELS || negative < FEW_LABELS {
        println!(
            "note: {positive} ratings of 4-5 and {negative} of 1-2; treat the suggestion as rough"
        );
    }
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let llm = Backends::from_config(&config.llm, &data);
    let mut quota = Quota::from_config(&config, max_llm_calls);
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
    let suggestion = match suggestion {
        Suggested::Profile(suggestion) => suggestion,
        Suggested::NotAsked(reason) => {
            println!("no suggestion: the llm was not asked ({reason})");
            return Ok(());
        }
    };
    let Some(text) = suggest::render(&current, &suggestion, &out.display().to_string())? else {
        println!("no changes suggested");
        return Ok(());
    };
    write_new(out, &profile::to_toml(&suggestion.profile)).map_err(|source| Error::WriteFile {
        path: out.to_path_buf(),
        source,
    })?;
    print!("{text}");
    Ok(())
}

/// 新しいファイルとして書く。書き込みに失敗したら、作ったファイルを消す（書きかけのファイルが
/// 残ると、同じパスで再実行できなくなるため）。
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    if written.is_err() {
        drop(file);
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!(path = %path.display(), "cannot remove the partial file: {e}");
        }
    }
    written
}
