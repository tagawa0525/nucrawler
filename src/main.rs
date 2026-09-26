use std::path::PathBuf;
use std::process::ExitCode;

use nucrawler::check::{self, CheckError};
use nucrawler::cli::{self, Command, SourcesArgs};
use nucrawler::config::{self, ConfigError};
use nucrawler::db::{Db, DbError};
use nucrawler::errors;
use nucrawler::http::{Fetcher, HttpError};
use nucrawler::pipeline::fetch::{self, FetchError};
use nucrawler::pipeline::lock::{self, LockError};
use nucrawler::pipeline::{self, Cancel, Stage};
use nucrawler::status;

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
        Err(e @ Error::Interrupted) => {
            tracing::warn!("{e}");
            ExitCode::from(130)
        }
        Err(e) => {
            tracing::error!("{}", errors::error_chain(&e));
            ExitCode::FAILURE
        }
    }
}

/// ログは stderr に出す（stdout は help 出力や将来の MCP の JSON-RPC 用）。
/// 詳細度は RUST_LOG で変えられ、既定は info。
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
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
            let stages = pipeline::plan(args.until, args.only);
            crawl(inv.config_dir, inv.data_dir, &stages).await
        }
        Command::Status => status(inv.config_dir, inv.data_dir),
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
    stages: &[Stage],
) -> Result<(), Error> {
    let (config, sources) = config::load(&config_dir(config)?)?;
    let data = data_dir(data)?;
    let _lock = lock::acquire(&data)?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let fetcher = Fetcher::from_config(&config.http)?;
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());

    let mut failed_sources = 0;
    for &stage in stages {
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
            Stage::Extract => todo!("extract stage"),
        }
    }
    if cancel.is_requested() {
        return Err(Error::Interrupted);
    }
    if failed_sources > 0 {
        return Err(Error::SourcesFailed(failed_sources));
    }
    Ok(())
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
