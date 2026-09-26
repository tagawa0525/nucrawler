use anyhow::Result;

/// トップレベルのサブコマンド。各サブコマンド固有の引数は `args` に残し、
/// そのサブコマンドの実装側で解釈する。
#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Command {
    Crawl,
    Redo,
    Status,
    Sources,
    Serve,
    Mcp,
    Rescore,
    Profile,
    Help,
}

pub fn parse(_args: impl IntoIterator<Item = String>) -> Result<Invocation> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_subcommand_and_keeps_rest() {
        let inv = parse(args(&["crawl", "--until", "digest"])).unwrap();
        assert_eq!(
            inv,
            Invocation {
                command: Command::Crawl,
                args: args(&["--until", "digest"]),
            }
        );
    }

    #[test]
    fn parses_every_subcommand_name() {
        for (name, cmd) in [
            ("crawl", Command::Crawl),
            ("redo", Command::Redo),
            ("status", Command::Status),
            ("sources", Command::Sources),
            ("serve", Command::Serve),
            ("mcp", Command::Mcp),
            ("rescore", Command::Rescore),
            ("profile", Command::Profile),
            ("help", Command::Help),
            ("--help", Command::Help),
            ("-h", Command::Help),
        ] {
            assert_eq!(parse(args(&[name])).unwrap().command, cmd, "{name}");
        }
    }

    #[test]
    fn no_subcommand_is_help() {
        assert_eq!(parse(args(&[])).unwrap().command, Command::Help);
    }

    #[test]
    fn unknown_subcommand_is_error() {
        let err = parse(args(&["frobnicate"])).unwrap_err();
        assert!(err.to_string().contains("frobnicate"), "{err}");
    }
}
