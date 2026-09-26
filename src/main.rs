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
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
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
