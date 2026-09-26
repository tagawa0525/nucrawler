use std::path::PathBuf;

use crate::pipeline::Stage;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unknown command: {0}\n\n{USAGE}")]
    UnknownCommand(String),
    #[error("option {0} requires a value")]
    MissingValue(&'static str),
    #[error("usage: nucrawler sources check [ID]")]
    SourcesUsage,
    #[error("usage: nucrawler profile import FILE | nucrawler profile export")]
    ProfileUsage,
    #[error(
        "usage: nucrawler crawl [--until STAGE | --only STAGE] [--max-llm-calls N]  (stages: {stages})"
    )]
    CrawlUsage { stages: String },
}

/// トップレベルのサブコマンド。各サブコマンド固有の引数は `args` に残し、
/// そのサブコマンドの実装側で解釈する。
#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    /// `--config-dir DIR`（サブコマンドより前に置く共通オプション）
    pub config_dir: Option<PathBuf>,
    /// `--data-dir DIR`（DB とロックファイルの置き場所）
    pub data_dir: Option<PathBuf>,
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
usage: nucrawler [--config-dir DIR] [--data-dir DIR] <command> [args]

options:
  --config-dir DIR  設定ディレクトリ（既定 $XDG_CONFIG_HOME/nucrawler）
  --data-dir DIR    DB などの置き場所（既定 $XDG_DATA_HOME/nucrawler）

commands:
  crawl     巡回・抽出・要約・採点のパイプラインを実行（中断しても次回再開）
  redo      指定モデルで要約・和訳をやり直す
  status    ステージごとの未処理件数などを表示
  sources   ソースの取得確認（sources check [ID]）
  serve     Web UI / RSS / JSON API を起動
  mcp       MCP stdio サーバを起動
  rescore   記事を再採点
  profile   関心プロファイルの取り込み・書き出し（profile import FILE / profile export）
  help      このヘルプを表示
";

/// `args` はプログラム名を除いたコマンドライン引数。
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Invocation, ParseError> {
    let mut args = args.into_iter().peekable();
    let mut config_dir = None;
    let mut data_dir = None;
    loop {
        let (slot, name) = match args.peek().map(String::as_str) {
            Some("--config-dir") => (&mut config_dir, "--config-dir"),
            Some("--data-dir") => (&mut data_dir, "--data-dir"),
            _ => break,
        };
        args.next();
        let dir = args.next().ok_or(ParseError::MissingValue(name))?;
        *slot = Some(PathBuf::from(dir));
    }
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
        config_dir,
        data_dir,
        command,
        args: args.collect(),
    })
}

/// `crawl` サブコマンドの引数。`until` と `only` は同時に指定できない。
#[derive(Debug, PartialEq, Eq, Default)]
pub struct CrawlArgs {
    pub until: Option<Stage>,
    pub only: Option<Stage>,
    /// この実行で LLM を呼んでよい回数（設定の `quota.max_calls_per_run` より優先）
    pub max_llm_calls: Option<u32>,
}

pub fn parse_crawl_args(args: &[String]) -> Result<CrawlArgs, ParseError> {
    let usage = || ParseError::CrawlUsage {
        stages: Stage::ALL
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(", "),
    };
    let mut parsed = CrawlArgs::default();
    let mut it = args.iter();
    while let Some(opt) = it.next() {
        let slot = match opt.as_str() {
            "--until" => &mut parsed.until,
            "--only" => &mut parsed.only,
            "--max-llm-calls" => {
                let n = it.next().and_then(|n| n.parse().ok()).ok_or_else(usage)?;
                parsed.max_llm_calls = Some(n);
                continue;
            }
            _ => return Err(usage()),
        };
        let stage = it
            .next()
            .and_then(|name| Stage::from_name(name))
            .ok_or_else(usage)?;
        *slot = Some(stage);
    }
    if parsed.until.is_some() && parsed.only.is_some() {
        return Err(usage());
    }
    Ok(parsed)
}

/// `profile` サブコマンドの引数。
#[derive(Debug, PartialEq, Eq)]
pub enum ProfileArgs {
    /// `profile import FILE`
    Import { file: PathBuf },
    /// `profile export`（標準出力へ）
    Export,
}

pub fn parse_profile_args(args: &[String]) -> Result<ProfileArgs, ParseError> {
    match args {
        [cmd, file] if cmd == "import" => Ok(ProfileArgs::Import {
            file: PathBuf::from(file),
        }),
        [cmd] if cmd == "export" => Ok(ProfileArgs::Export),
        _ => Err(ParseError::ProfileUsage),
    }
}

/// `sources` サブコマンドの引数。
#[derive(Debug, PartialEq, Eq)]
pub enum SourcesArgs {
    /// `sources check [ID]`
    Check { id: Option<String> },
}

pub fn parse_sources_args(args: &[String]) -> Result<SourcesArgs, ParseError> {
    match args {
        [cmd] if cmd == "check" => Ok(SourcesArgs::Check { id: None }),
        [cmd, id] if cmd == "check" => Ok(SourcesArgs::Check {
            id: Some(id.clone()),
        }),
        _ => Err(ParseError::SourcesUsage),
    }
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
                config_dir: None,
                data_dir: None,
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
    fn parses_global_config_dir_before_subcommand() {
        let inv = parse(args(&[
            "--config-dir",
            "/etc/nc",
            "sources",
            "check",
            "nrc",
        ]))
        .unwrap();
        assert_eq!(
            inv,
            Invocation {
                config_dir: Some(PathBuf::from("/etc/nc")),
                data_dir: None,
                command: Command::Sources,
                args: args(&["check", "nrc"]),
            }
        );
    }

    #[test]
    fn usage_documents_global_options() {
        assert!(USAGE.contains("--config-dir"), "{USAGE}");
    }

    #[test]
    fn parses_global_data_dir_with_config_dir() {
        let inv = parse(args(&[
            "--data-dir",
            "/var/nc",
            "--config-dir",
            "/etc/nc",
            "status",
        ]))
        .unwrap();
        assert_eq!(inv.data_dir, Some(PathBuf::from("/var/nc")));
        assert_eq!(inv.config_dir, Some(PathBuf::from("/etc/nc")));
        assert_eq!(inv.command, Command::Status);
        assert!(USAGE.contains("--data-dir"), "{USAGE}");
    }

    #[test]
    fn parses_crawl_args() {
        assert_eq!(parse_crawl_args(&[]).unwrap(), CrawlArgs::default());
        assert_eq!(
            parse_crawl_args(&args(&["--until", "fetch"])).unwrap(),
            CrawlArgs {
                until: Some(Stage::Fetch),
                only: None,
                max_llm_calls: None,
            }
        );
        assert_eq!(
            parse_crawl_args(&args(&["--only", "fetch"])).unwrap(),
            CrawlArgs {
                until: None,
                only: Some(Stage::Fetch),
                max_llm_calls: None,
            }
        );
    }

    #[test]
    fn parses_max_llm_calls() {
        let args = parse_crawl_args(&args(&["--max-llm-calls", "3", "--only", "digest"])).unwrap();
        assert_eq!(args.max_llm_calls, Some(3));
        assert_eq!(args.only, Some(Stage::Digest));
        for bad in [&["--max-llm-calls"][..], &["--max-llm-calls", "x"][..]] {
            assert!(
                parse_crawl_args(&super::tests::args(bad)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_crawl_args() {
        for bad in [
            &["--until"][..],
            &["--until", "nope"][..],
            &["--until", "fetch", "--only", "fetch"][..],
            &["extra"][..],
        ] {
            let err = parse_crawl_args(&args(bad)).unwrap_err();
            assert!(
                matches!(err, ParseError::CrawlUsage { .. }),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn config_dir_without_value_is_error() {
        let err = parse(args(&["--config-dir"])).unwrap_err();
        assert!(
            matches!(err, ParseError::MissingValue("--config-dir")),
            "{err}"
        );
    }

    #[test]
    fn parses_sources_check() {
        assert_eq!(
            parse_sources_args(&args(&["check"])).unwrap(),
            SourcesArgs::Check { id: None }
        );
        assert_eq!(
            parse_sources_args(&args(&["check", "nrc-news"])).unwrap(),
            SourcesArgs::Check {
                id: Some("nrc-news".into())
            }
        );
    }

    #[test]
    fn parses_profile_args() {
        assert_eq!(
            parse_profile_args(&args(&["import", "p.toml"])).unwrap(),
            ProfileArgs::Import {
                file: PathBuf::from("p.toml")
            }
        );
        assert_eq!(
            parse_profile_args(&args(&["export"])).unwrap(),
            ProfileArgs::Export
        );
        for bad in [
            &[][..],
            &["import"][..],
            &["export", "x"][..],
            &["show"][..],
        ] {
            let err = parse_profile_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::ProfileUsage), "{bad:?}");
        }
    }

    #[test]
    fn rejects_bad_sources_args() {
        for bad in [&[][..], &["list"][..], &["check", "a", "b"][..]] {
            let err = parse_sources_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::SourcesUsage), "{bad:?}: {err}");
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
