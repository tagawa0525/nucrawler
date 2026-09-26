#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unknown command: {0}\n\n{USAGE}")]
    UnknownCommand(String),
}

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

pub const USAGE: &str = "\
usage: nucrawler <command> [args]

commands:
  crawl     巡回・抽出・要約・採点のパイプラインを実行（中断しても次回再開）
  redo      指定モデルで要約・和訳をやり直す
  status    ステージごとの未処理件数などを表示
  sources   ソースの取得確認
  serve     Web UI / RSS / JSON API を起動
  mcp       MCP stdio サーバを起動
  rescore   記事を再採点
  profile   プロファイル関連の操作
  help      このヘルプを表示
";

/// `args` はプログラム名を除いたコマンドライン引数。
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Invocation, ParseError> {
    let mut args = args.into_iter();
    let command = match args.next().as_deref() {
        None | Some("help" | "--help" | "-h") => Command::Help,
        Some("crawl") => Command::Crawl,
        Some("redo") => Command::Redo,
        Some("status") => Command::Status,
        Some("sources") => Command::Sources,
        Some("serve") => Command::Serve,
        Some("mcp") => Command::Mcp,
        Some("rescore") => Command::Rescore,
        Some("profile") => Command::Profile,
        Some(other) => return Err(ParseError::UnknownCommand(other.to_string())),
    };
    Ok(Invocation {
        command,
        args: args.collect(),
    })
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
