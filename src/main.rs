mod cli;

use std::process::ExitCode;

use cli::Command;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Parse(#[from] cli::ParseError),
    #[error("{0:?} is not implemented yet")]
    NotImplemented(Command),
}

fn main() -> ExitCode {
    init_tracing();
    match run() {
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

fn run() -> Result<(), Error> {
    let inv = cli::parse(std::env::args().skip(1))?;
    match inv.command {
        Command::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        cmd => Err(Error::NotImplemented(cmd)),
    }
}
