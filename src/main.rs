use std::path::PathBuf;
use std::process::ExitCode;

use nucrawler::check::{self, CheckError};
use nucrawler::cli::{self, Command, SourcesArgs};
use nucrawler::config::{self, ConfigError};
use nucrawler::http::{Fetcher, HttpError};

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
    #[error("{0} source(s) failed")]
    SourcesFailed(usize),
    #[error("{0:?} is not implemented yet")]
    NotImplemented(Command),
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
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
