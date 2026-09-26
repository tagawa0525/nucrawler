mod cli;

use anyhow::{Result, bail};
use cli::Command;

fn main() -> Result<()> {
    let inv = cli::parse(std::env::args().skip(1))?;
    match inv.command {
        Command::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        cmd => bail!("{cmd:?} is not implemented yet"),
    }
}
