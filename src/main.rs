use std::path::{Path, PathBuf};
use std::process::ExitCode;

use nucrawler::auth::{self, AuthError};
use nucrawler::check::{self, CheckError};
use nucrawler::cli::{self, Command, ProfileArgs, SearchArgs, SourcesArgs, TopicsArgs, UserArgs};
use nucrawler::config::{self, ConfigError};
use nucrawler::db::{Db, DbError};
use nucrawler::errors;
use nucrawler::filelock::LockError;
use nucrawler::http::{Fetcher, HttpError};
use nucrawler::mcp::{self, McpError};
use nucrawler::pipeline::run::RunError;
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
    Run(#[from] RunError),
    #[error("llm call failed: {0}")]
    LlmFailed(String),
    #[error("embedding failed: {0}")]
    EmbeddingFailed(String),
    #[error(transparent)]
    Embedding(#[from] nucrawler::embedding::EmbedError),
    #[error(transparent)]
    EmbedStage(#[from] nucrawler::pipeline::embed::EmbedStageError),
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Topics(#[from] TopicsError),
    #[error(transparent)]
    Search(#[from] nucrawler::search::SearchError),
    #[error(transparent)]
    Serve(#[from] ServeError),
    #[error(transparent)]
    Mcp(#[from] McpError),
    #[error(transparent)]
    NoReasons(#[from] nucrawler::suggest::NoReasons),
    #[error("failed to read {path}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no profile yet; run `nucrawler profile import FILE` first")]
    NoProfile,
    /// `eval --profile` は候補を embedding で計算する
    #[error("eval --profile scores the candidate by embedding; add [embedding] to config.toml")]
    CandidateNeedsEmbedding,
    #[error("no embeddings yet; run `nucrawler crawl --only embed` first")]
    NoEmbeddings,
    #[error("no ratings yet; rate articles (★1-5) in the web UI first")]
    NoLabels,
    #[error(
        "no rated article has a digest you can view, so there is nothing to base a suggestion on"
    )]
    NoEvidence,
    #[error("{0} already exists; choose a new file for the suggestion")]
    OutputExists(PathBuf),
    #[error("failed to write {path}")]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to create data directory {path}")]
    DataDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0} source(s) failed")]
    SourcesFailed(usize),
    #[error("interrupted; the next run resumes from where this one stopped")]
    Interrupted,
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
        Command::Profile => {
            profile(
                inv.config_dir,
                inv.data_dir,
                cli::parse_profile_args(&inv.args)?,
            )
            .await
        }
        Command::Topics => topics(inv.data_dir, cli::parse_topics_args(&inv.args)?),
        Command::User => user(inv.data_dir, cli::parse_user_args(&inv.args)?),
        Command::Embed => embed(inv.data_dir, cli::parse_embed_args(&inv.args)?),
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
        Command::Eval => {
            cmd::eval(
                inv.config_dir,
                inv.data_dir,
                cli::parse_eval_args(&inv.args)?,
            )
            .await
        }
        Command::Mcp => mcp(inv.config_dir, inv.data_dir).await,
        Command::Sources => match cli::parse_sources_args(&inv.args)? {
            SourcesArgs::Check { id } => sources_check(inv.config_dir, id.as_deref()).await,
        },
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

/// データディレクトリ `data` の DB を開く。
fn open_db(data: &Path) -> Result<Db, Error> {
    Ok(Db::open(&data.join("nucrawler.db"))?)
}

/// 検索して 1 行 1 件で出す。オーナーとして閲覧判定し、閲覧としては記録しない。
fn search(config: Option<PathBuf>, data: Option<PathBuf>, args: SearchArgs) -> Result<(), Error> {
    let (config, _) = config::load(&config_dir(config)?)?;
    let db = open_db(&data_dir(data)?)?.with_prior_strength(config.recommend.prior_strength);
    let owner = db.owner_id()?;
    let hash = db.profile_hash(owner)?;
    let limit = args.limit.unwrap_or(config.web.list_limit);
    let query = args.params.to_query(owner, hash.as_deref(), limit)?;
    for item in db.search_articles(&query)? {
        println!("{}", nucrawler::search::result_line(&item));
    }
    Ok(())
}

/// Web UI の利用者を管理する（CLI を使えるのは稼働ホストに入れる管理者だけ）。パスワードは CLI が作って 1 回だけ表示する。
fn user(data: Option<PathBuf>, args: UserArgs) -> Result<(), Error> {
    let db = open_db(&data_dir(data)?)?;
    let new_password = || -> Result<(String, String), Error> {
        let password = auth::initial_password()?;
        let hash = auth::hash_password(&password)?;
        Ok((password, hash))
    };
    match args {
        UserArgs::Add {
            login,
            display_name,
        } => {
            let (password, hash) = new_password()?;
            db.add_user(&login, &display_name, &hash)?;
            println!("added {login}; initial password: {password}");
        }
        UserArgs::ResetPassword { login } => {
            let (password, hash) = new_password()?;
            db.reset_password(&login, &hash)?;
            println!("reset {login} (sessions and feed URL revoked); new password: {password}");
        }
        UserArgs::Disable { login } => {
            db.disable_user(&login)?;
            println!(
                "disabled {login}; run `nucrawler user reset-password {login}` to enable again"
            );
        }
        UserArgs::Rename { login, new_login } => {
            db.rename_user(&login, &new_login)?;
            println!("renamed {login} to {new_login}");
        }
        UserArgs::List => {
            for u in db.users()? {
                println!(
                    "{}\t{}\t{}\t{}",
                    u.login,
                    u.display_name,
                    if u.is_admin { "admin" } else { "user" },
                    if u.has_password {
                        "password set"
                    } else {
                        "no password"
                    }
                );
            }
        }
    }
    Ok(())
}

fn topics(data: Option<PathBuf>, args: TopicsArgs) -> Result<(), Error> {
    let db = open_db(&data_dir(data)?)?;
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

/// 実行中の crawl が古い空間に保存しようとしても、空間の世代が変わっているので保存されない。
fn embed(data: Option<PathBuf>, args: cli::EmbedArgs) -> Result<(), Error> {
    let db = open_db(&data_dir(data)?)?;
    match args {
        cli::EmbedArgs::Rebuild => {
            db.rebuild_embeddings()?;
            tracing::info!("embeddings cleared; the next crawl embeds every digest again");
        }
    }
    Ok(())
}

/// プロファイルはオーナー（このマシンの利用者）のものを扱う。
async fn profile(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    args: ProfileArgs,
) -> Result<(), Error> {
    let open = |data| -> Result<(Db, i64), Error> {
        let db = open_db(&data_dir(data)?)?;
        let owner = db.owner_id()?;
        Ok((db, owner))
    };
    match args {
        ProfileArgs::Import { file } => {
            let text = std::fs::read_to_string(&file).map_err(|source| Error::ReadFile {
                path: file.clone(),
                source,
            })?;
            let parsed = profile::parse(&text)?;
            let (db, owner) = open(data)?;
            db.save_profile(owner, &parsed, chrono::Utc::now())?;
            tracing::info!(
                interests = parsed.interests.len(),
                hash = %profile::hash(&parsed),
                "profile imported"
            );
            Ok(())
        }
        ProfileArgs::Export => {
            let (db, owner) = open(data)?;
            let (saved, _) = db.load_profile(owner)?.ok_or(Error::NoProfile)?;
            print!("{}", profile::to_toml(&saved));
            Ok(())
        }
        ProfileArgs::Suggest { out, max_llm_calls } => {
            cmd::suggest(config, data, &out, max_llm_calls).await
        }
        ProfileArgs::History => {
            let (db, owner) = open(data)?;
            print!("{}", profile::render_versions(&db.profile_versions(owner)?));
            Ok(())
        }
        ProfileArgs::Revert { version } => {
            let (db, owner) = open(data)?;
            if db.revert_profile(owner, version, chrono::Utc::now())? {
                tracing::info!(version, "profile reverted");
            } else {
                tracing::info!(version, "the profile already has this content");
            }
            Ok(())
        }
    }
}

async fn serve(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    args: cli::ServeArgs,
) -> Result<(), Error> {
    let (config, sources) = config::load(&config_dir(config)?)?;
    let db = open_db(&data_dir(data)?)?.with_prior_strength(config.recommend.prior_strength);
    let addr = args.addr.unwrap_or(config.web.bind);
    let state = server::AppState::new(db, config.web, sources.labels());
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
    let db = open_db(&data_dir(data)?)?.with_prior_strength(config.recommend.prior_strength);
    mcp::run(mcp::Server::new(db, config.web, sources.labels())).await?;
    Ok(())
}

fn status(config: Option<PathBuf>, data: Option<PathBuf>) -> Result<(), Error> {
    let (_, sources) = config::load(&config_dir(config)?)?;
    let db = open_db(&data_dir(data)?)?;
    print!(
        "{}",
        status::render(&sources.sources, &db.source_overview()?)
    );
    print!("{}", status::render_failures(&db.stage_failures()?));
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
