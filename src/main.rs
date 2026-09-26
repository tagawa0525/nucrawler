use std::path::PathBuf;
use std::process::ExitCode;

use nucrawler::check::{self, CheckError};
use nucrawler::cli::{self, Command, ProfileArgs, RedoArgs, RedoKind, SourcesArgs};
use nucrawler::config::{self, ConfigError, LlmConfig};
use nucrawler::db::{Db, DbError};
use nucrawler::errors;
use nucrawler::http::{Fetcher, HttpError};
use nucrawler::llm::claude_cli::ClaudeCli;
use nucrawler::pipeline::digest::{self, DigestStageError};
use nucrawler::pipeline::extract::{self, ExtractStageError};
use nucrawler::pipeline::fetch::{self, FetchError};
use nucrawler::pipeline::lock::{self, LockError};
use nucrawler::pipeline::score::{self, ScoreStageError};
use nucrawler::pipeline::translate::{self, TranslateStageError};
use nucrawler::pipeline::{self, Cancel, Halt, Stage, Target};
use nucrawler::profile::{self, ProfileError};
use nucrawler::quota::Quota;
use nucrawler::status;
use nucrawler::web::server::{self, ServeError};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Parse(#[from] cli::ParseError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Check(#[from] CheckError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Extract(#[from] ExtractStageError),
    #[error(transparent)]
    Digest(#[from] DigestStageError),
    #[error(transparent)]
    Score(#[from] ScoreStageError),
    #[error(transparent)]
    Translate(#[from] TranslateStageError),
    #[error("llm call failed: {0}")]
    LlmFailed(String),
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error(transparent)]
    Serve(#[from] ServeError),
    #[error("failed to read {path}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no profile yet; run `nucrawler profile import FILE` first")]
    NoProfile,
    #[error("failed to create data directory {path}")]
    DataDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0} source(s) failed")]
    SourcesFailed(usize),
    #[error("interrupted; the next run resumes from where this one stopped")]
    Interrupted,
    #[error("{0:?} is not implemented yet")]
    NotImplemented(Command),
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let code = exit_code(&e);
            if code == 1 {
                tracing::error!("{}", errors::error_chain(&e));
            } else {
                tracing::warn!("{e}");
            }
            ExitCode::from(code)
        }
    }
}

/// 失敗の終了コード。中断は 130、別の crawl が実行中なら EX_TEMPFAIL（75。systemd の unit では
/// `SuccessExitStatus` で失敗扱いにしない）、それ以外は 1。
fn exit_code(e: &Error) -> u8 {
    match e {
        Error::Interrupted => 130,
        Error::Lock(LockError::Held { .. }) => 75,
        _ => 1,
    }
}

/// ログは stderr に出す（stdout は help 出力や将来の MCP の JSON-RPC 用）。
/// 詳細度は RUST_LOG で変えられ、既定は info。readability が HTML を書き出すときの
/// html5ever の警告（"weird namespace" など）は利用者が対処できないので、既定では出さない。
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,html5ever=error")),
        )
        .init();
}

async fn run() -> Result<(), Error> {
    let inv = cli::parse(std::env::args().skip(1))?;
    match inv.command {
        Command::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Command::Crawl => {
            let args = cli::parse_crawl_args(&inv.args)?;
            crawl(inv.config_dir, inv.data_dir, &args).await
        }
        Command::Status => status(inv.config_dir, inv.data_dir),
        Command::Redo => {
            redo(
                inv.config_dir,
                inv.data_dir,
                cli::parse_redo_args(&inv.args)?,
            )
            .await
        }
        Command::Profile => profile(inv.data_dir, cli::parse_profile_args(&inv.args)?),
        Command::Serve => {
            serve(
                inv.config_dir,
                inv.data_dir,
                cli::parse_serve_args(&inv.args)?,
            )
            .await
        }
        Command::Sources => match cli::parse_sources_args(&inv.args)? {
            SourcesArgs::Check { id } => sources_check(inv.config_dir, id.as_deref()).await,
        },
        cmd => Err(Error::NotImplemented(cmd)),
    }
}

fn config_dir(explicit: Option<PathBuf>) -> Result<PathBuf, ConfigError> {
    match explicit {
        Some(dir) => Ok(dir),
        None => config::default_dir(|k| std::env::var_os(k)),
    }
}

/// DB とロックファイルの置き場所。無ければ作る。
fn data_dir(explicit: Option<PathBuf>) -> Result<PathBuf, Error> {
    let dir = match explicit {
        Some(dir) => dir,
        None => config::default_data_dir(|k| std::env::var_os(k))?,
    };
    std::fs::create_dir_all(&dir).map_err(|source| Error::DataDir {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

async fn crawl(
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
    // 認証切れなど、利用者が対処すべき LLM の失敗（最後にエラーとして報告する）
    let mut llm_failure = None;
    // LLM のステージで共有する。呼び出し回数や時間帯の上限は、この実行全体に効く。
    let llm = ClaudeCli {
        command: config.llm.command.clone().into(),
        cwd: data.join("llm-cwd"),
        timeout: std::time::Duration::from_secs(config.llm.timeout_secs),
    };
    let mut quota = Quota::new(
        config.quota.clone(),
        db.latest_rate_limit()?,
        args.max_llm_calls,
    );
    // 上限到達や LLM の失敗の後は、同じ実行の中で後続の LLM ステージを試さない
    let mut llm_blocked = false;

    let mut failed_sources = 0;
    for &stage in &stages {
        if cancel.is_requested() {
            break;
        }
        match stage {
            Stage::Fetch => {
                let summary =
                    fetch::fetch_sources(&db, &fetcher, &sources.sources, &cancel).await?;
                tracing::info!(
                    new_articles = summary.new_articles,
                    failed_sources = summary.failed_sources.len(),
                    "fetch stage finished"
                );
                failed_sources += summary.failed_sources.len();
            }
            Stage::Digest | Stage::Score | Stage::Translate if llm_blocked => {
                tracing::warn!(
                    stage = stage.name(),
                    "skipped: the llm is unavailable in this run"
                );
            }
            Stage::Digest => {
                // 採点が計画に無ければ、採点のための予約はしない
                let digest_cfg = LlmConfig {
                    score_reserved_calls: pipeline::score_reserve(
                        &stages,
                        &config.llm,
                        db.load_profile(db.owner_id()?)?.is_some(),
                    ),
                    ..config.llm.clone()
                };
                let summary = digest::digest_articles(
                    &db,
                    &llm,
                    &mut quota,
                    &digest_cfg,
                    &config.pipeline,
                    &Target::Pending {
                        requests_only: false,
                    },
                    chrono::Utc::now(),
                    &cancel,
                )
                .await?;
                tracing::info!(
                    digested = summary.digested,
                    failed = summary.failed,
                    calls = summary.calls,
                    "digest stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut llm_failure);
            }
            Stage::Score => {
                let summary = score::score_articles(
                    &db,
                    &llm,
                    &mut quota,
                    &config.llm,
                    &config.pipeline,
                    db.owner_id()?,
                    chrono::Utc::now(),
                    &cancel,
                )
                .await?;
                tracing::info!(
                    scored = summary.scored,
                    failed = summary.failed,
                    calls = summary.calls,
                    "score stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut llm_failure);
            }
            Stage::Translate => {
                let summary = translate::translate_articles(
                    &db,
                    &llm,
                    &mut quota,
                    &config.llm,
                    &config.pipeline,
                    db.owner_id()?,
                    &Target::Pending {
                        requests_only: args.requests_only,
                    },
                    chrono::Utc::now(),
                    &cancel,
                )
                .await?;
                tracing::info!(
                    translated = summary.translated,
                    failed = summary.failed,
                    calls = summary.calls,
                    "translate stage finished"
                );
                llm_blocked = report_halt(summary.halted, &mut llm_failure);
            }
            Stage::Extract => {
                let summary = extract::extract_pages(
                    &db,
                    &fetcher,
                    &sources.sources,
                    &config.pipeline,
                    chrono::Utc::now(),
                    &cancel,
                )
                .await?;
                tracing::info!(
                    extracted = summary.extracted,
                    failed = summary.failed,
                    gave_up = summary.gave_up,
                    "extract stage finished"
                );
            }
        }
    }
    if cancel.is_requested() {
        return Err(Error::Interrupted);
    }
    if let Some(message) = llm_failure {
        return Err(Error::LlmFailed(message));
    }
    if failed_sources > 0 {
        return Err(Error::SourcesFailed(failed_sources));
    }
    Ok(())
}

/// 指定したモデルで要約か和訳を作り直す。条件に合う記事のうち、そのモデル・プロンプト版の
/// 成果物がまだ無いものだけを処理するので、途中で止めても同じコマンドで続きから再開できる。
/// 新しい digest ができた記事は、次の crawl で自動的に採点し直される。
async fn redo(config: Option<PathBuf>, data: Option<PathBuf>, args: RedoArgs) -> Result<(), Error> {
    let (config, _) = config::load(&config_dir(config)?)?;
    let data = data_dir(data)?;
    let _lock = lock::acquire(&data)?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let llm = ClaudeCli {
        command: config.llm.command.clone().into(),
        cwd: data.join("llm-cwd"),
        timeout: std::time::Duration::from_secs(config.llm.timeout_secs),
    };
    let mut quota = Quota::new(
        config.quota.clone(),
        db.latest_rate_limit()?,
        args.max_llm_calls,
    );
    let owner = db.owner_id()?;
    let target = Target::Redo(pipeline::RedoSpec {
        filter: args.filter,
        user_id: owner,
        profile_hash: db.load_profile(owner)?.map(|(_, hash)| hash),
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
                &db,
                &llm,
                &mut quota,
                &cfg,
                &config.pipeline,
                &target,
                chrono::Utc::now(),
                &cancel,
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
                &db,
                &llm,
                &mut quota,
                &cfg,
                &config.pipeline,
                owner,
                &target,
                chrono::Utc::now(),
                &cancel,
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

/// 止めた理由をログに出し、同じ実行で LLM をもう使わないほうがよいなら true を返す。
/// 認証切れなど利用者が対処すべき失敗は `llm_failure` に残し、最後にエラーとして報告する。
fn report_halt(halt: Option<Halt>, llm_failure: &mut Option<String>) -> bool {
    match halt {
        Some(Halt::LlmFailed(message)) => {
            *llm_failure = Some(message);
            true
        }
        Some(Halt::UsageLimit { resets_at }) => {
            tracing::warn!(?resets_at, "stopped at the subscription usage limit");
            true
        }
        Some(Halt::Quota(stop)) => {
            tracing::info!("llm work deferred: {stop}");
            false
        }
        None => false,
    }
}

/// 1 回目の SIGINT/SIGTERM では処理中の 1 件を終えてから止め、2 回目で即座に終了する。
fn spawn_signal_handler(cancel: Cancel) {
    tokio::spawn(async move {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(term) => term,
                Err(e) => {
                    tracing::error!("cannot listen for SIGTERM: {e}");
                    return;
                }
            };
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            if cancel.is_requested() {
                tracing::warn!("second signal; exiting immediately");
                std::process::exit(130);
            }
            tracing::warn!("stopping after the current item (signal again to exit immediately)");
            cancel.request();
        }
    });
}

/// プロファイルはオーナー（このマシンの利用者）のものを扱う。
fn profile(data: Option<PathBuf>, args: ProfileArgs) -> Result<(), Error> {
    let db = Db::open(&data_dir(data)?.join("nucrawler.db"))?;
    let owner = db.owner_id()?;
    match args {
        ProfileArgs::Import { file } => {
            let text = std::fs::read_to_string(&file).map_err(|source| Error::ReadFile {
                path: file.clone(),
                source,
            })?;
            let parsed = profile::parse(&text)?;
            db.save_profile(owner, &parsed, chrono::Utc::now())?;
            tracing::info!(
                interests = parsed.interests.len(),
                hash = %profile::hash(&parsed),
                "profile imported"
            );
        }
        ProfileArgs::Export => {
            let (saved, _) = db.load_profile(owner)?.ok_or(Error::NoProfile)?;
            print!("{}", profile::to_toml(&saved));
        }
    }
    Ok(())
}

async fn serve(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    args: cli::ServeArgs,
) -> Result<(), Error> {
    let (config, _) = config::load(&config_dir(config)?)?;
    let db = Db::open(&data_dir(data)?.join("nucrawler.db"))?;
    let addr = args.addr.unwrap_or(config.web.bind);
    let state = server::AppState::new(db, config.web);
    server::run(addr, state, shutdown_signal()).await?;
    Ok(())
}

/// Ctrl-C か SIGTERM（systemd の停止）で、処理中の応答を終えてから止める。
async fn shutdown_signal() {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            Err(e) => {
                tracing::error!("cannot listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = term => {}
    }
    tracing::info!("shutting down the web ui");
}

fn status(config: Option<PathBuf>, data: Option<PathBuf>) -> Result<(), Error> {
    let (_, sources) = config::load(&config_dir(config)?)?;
    let db = Db::open(&data_dir(data)?.join("nucrawler.db"))?;
    print!(
        "{}",
        status::render(&sources.sources, &db.source_overview()?)
    );
    Ok(())
}

async fn sources_check(dir: Option<PathBuf>, id: Option<&str>) -> Result<(), Error> {
    let (config, sources) = config::load(&config_dir(dir)?)?;
    let fetcher = Fetcher::from_config(&config.http)?;
    let reports = check::check(&fetcher, &sources.sources, id).await?;
    print!("{}", check::render(&reports, 3));
    let failed = reports.iter().filter(|r| r.outcome.is_err()).count();
    if failed > 0 {
        return Err(Error::SourcesFailed(failed));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// timer から起動した実行が、実行中の別の crawl とぶつかったときは「一時的に実行できない」
    /// （EX_TEMPFAIL）にし、systemd の unit で失敗扱いにしないようにする。
    #[test]
    fn lock_held_is_temporary_failure() {
        let held = Error::Lock(LockError::Held {
            path: PathBuf::from("/tmp/crawl.lock"),
        });
        assert_eq!(exit_code(&held), 75);
        assert_eq!(exit_code(&Error::Interrupted), 130);
        assert_eq!(exit_code(&Error::NoProfile), 1);
    }
}
