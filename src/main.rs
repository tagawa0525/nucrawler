use std::path::PathBuf;
use std::process::ExitCode;

use nucrawler::check::{self, CheckError};
use nucrawler::cli::{self, Command, ProfileArgs, SearchArgs, SourcesArgs, TopicsArgs};
use nucrawler::config::{self, ConfigError};
use nucrawler::db::{Db, DbError};
use nucrawler::errors;
use nucrawler::http::{Fetcher, HttpError};
use nucrawler::mcp::{self, McpError};
use nucrawler::pipeline::digest::DigestStageError;
use nucrawler::pipeline::extract::ExtractStageError;
use nucrawler::pipeline::fetch::FetchError;
use nucrawler::pipeline::lock::LockError;
use nucrawler::pipeline::score::ScoreStageError;
use nucrawler::pipeline::tidy::TidyStageError;
use nucrawler::pipeline::translate::TranslateStageError;
use nucrawler::profile::{self, ProfileError};
use nucrawler::status;
use nucrawler::topics::{self, TopicsError};
use nucrawler::web::server::{self, ServeError};

mod cmd;

use cmd::{crawl, redo};

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
    #[error(transparent)]
    Tidy(#[from] TidyStageError),
    #[error("llm call failed: {0}")]
    LlmFailed(String),
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error(transparent)]
    Topics(#[from] TopicsError),
    #[error(transparent)]
    Search(#[from] nucrawler::search::SearchError),
    #[error(transparent)]
    Serve(#[from] ServeError),
    #[error(transparent)]
    Mcp(#[from] McpError),
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

/// ログは stderr に出す（stdout は help 出力や MCP の JSON-RPC 用）。
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
        Command::Topics => topics(inv.data_dir, cli::parse_topics_args(&inv.args)?),
        Command::Search => search(
            inv.config_dir,
            inv.data_dir,
            cli::parse_search_args(&inv.args)?,
        ),
        Command::Serve => {
            serve(
                inv.config_dir,
                inv.data_dir,
                cli::parse_serve_args(&inv.args)?,
            )
            .await
        }
        Command::Mcp => mcp(inv.config_dir, inv.data_dir).await,
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

/// 検索して 1 行 1 件で出す。オーナーとして閲覧判定し、閲覧としては記録しない。
fn search(config: Option<PathBuf>, data: Option<PathBuf>, args: SearchArgs) -> Result<(), Error> {
    let (config, _) = config::load(&config_dir(config)?)?;
    let db = Db::open(&data_dir(data)?.join("nucrawler.db"))?;
    let owner = db.owner_id()?;
    let hash = db.load_profile(owner)?.map(|(_, hash)| hash);
    let limit = args.limit.unwrap_or(config.web.list_limit);
    let query = args.params.to_query(owner, hash.as_deref(), limit)?;
    for item in db.search_articles(&query)? {
        println!("{}", nucrawler::search::result_line(&item));
    }
    Ok(())
}

fn topics(data: Option<PathBuf>, args: TopicsArgs) -> Result<(), Error> {
    let db = Db::open(&data_dir(data)?.join("nucrawler.db"))?;
    match args {
        TopicsArgs::Import { file } => {
            let text = std::fs::read_to_string(&file).map_err(|source| Error::ReadFile {
                path: file.clone(),
                source,
            })?;
            let parsed = topics::parse(&text)?;
            db.replace_topics(&parsed)?;
            tracing::info!(topics = parsed.len(), "topics imported");
        }
        TopicsArgs::Export => print!("{}", topics::to_toml(&db.vocabulary()?)),
    }
    Ok(())
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
    let (config, sources) = config::load(&config_dir(config)?)?;
    let db = Db::open(&data_dir(data)?.join("nucrawler.db"))?;
    let addr = args.addr.unwrap_or(config.web.bind);
    let labels = sources
        .sources
        .iter()
        .map(|s| (s.id.clone(), s.display_name().to_string()))
        .collect();
    let state = server::AppState::new(db, config.web, labels);
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

/// MCP の stdio サーバー。一覧の既定値とソースの表示名は Web UI と同じ設定を使う。
async fn mcp(config: Option<PathBuf>, data: Option<PathBuf>) -> Result<(), Error> {
    let (config, sources) = config::load(&config_dir(config)?)?;
    let db = Db::open(&data_dir(data)?.join("nucrawler.db"))?;
    let labels = sources
        .sources
        .iter()
        .map(|s| (s.id.clone(), s.display_name().to_string()))
        .collect();
    mcp::run(mcp::Server::new(db, config.web, labels)).await?;
    Ok(())
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
