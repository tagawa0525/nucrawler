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
    #[error("usage: nucrawler topics import FILE | nucrawler topics export")]
    TopicsUsage,
    #[error(
        "usage: nucrawler redo digest|translate --model M [--source ID] [--since YYYY-MM-DD] \
         [--min-score N] [--ids 1,2,3] [--max-llm-calls N]"
    )]
    RedoUsage,
    #[error(
        "usage: nucrawler crawl [--until STAGE | --only STAGE | --requests-only] [--max-llm-calls N] [--wait-lock]  (stages: {stages})"
    )]
    CrawlUsage { stages: String },
    #[error("usage: nucrawler serve [--addr IP:PORT]")]
    ServeUsage,
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
    Topics,
    Help,
}

pub const USAGE: &str = "\
usage: nucrawler [--config-dir DIR] [--data-dir DIR] <command> [args]

options:
  --config-dir DIR  設定ディレクトリ（既定 $XDG_CONFIG_HOME/nucrawler）
  --data-dir DIR    DB などの置き場所（既定 $XDG_DATA_HOME/nucrawler）

commands:
  crawl     巡回・抽出・要約・採点のパイプラインを実行（中断しても次回再開）
  redo      指定モデルで要約・和訳をやり直す（redo digest|translate --model M ...）
  status    ステージごとの未処理件数などを表示
  sources   ソースの取得確認（sources check [ID]）
  serve     Web UI を起動（serve [--addr IP:PORT]、既定は設定の web.bind）
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
    /// 和訳の依頼だけを処理する（15 分ごとの timer 用）
    pub requests_only: bool,
    /// 別の crawl が実行中なら、終わるのを待ってから始める（timer 用。指定しなければ終了コード 75 で終わる）
    pub wait_lock: bool,
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
                let n = option_value(&mut it)
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(usage)?;
                parsed.max_llm_calls = Some(n);
                continue;
            }
            "--requests-only" => {
                parsed.requests_only = true;
                continue;
            }
            "--wait-lock" => {
                parsed.wait_lock = true;
                continue;
            }
            _ => return Err(usage()),
        };
        let stage = option_value(&mut it)
            .and_then(|name| Stage::from_name(name))
            .ok_or_else(usage)?;
        *slot = Some(stage);
    }
    if parsed.until.is_some() && parsed.only.is_some() {
        return Err(usage());
    }
    // 依頼の処理は和訳ステージだけで行うので、ステージの指定とは併用できない
    if parsed.requests_only && (parsed.until.is_some() || parsed.only.is_some()) {
        return Err(usage());
    }
    Ok(parsed)
}

/// `redo` で作り直す成果物。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedoKind {
    Digest,
    Translate,
}

/// `redo` サブコマンドの引数。
#[derive(Debug, PartialEq)]
pub struct RedoArgs {
    pub kind: RedoKind,
    pub model: String,
    pub filter: crate::db::RedoFilter,
    pub max_llm_calls: Option<u32>,
}

pub fn parse_redo_args(args: &[String]) -> Result<RedoArgs, ParseError> {
    let usage = || ParseError::RedoUsage;
    let (kind, rest) = match args.split_first() {
        Some((k, rest)) if k == "digest" => (RedoKind::Digest, rest),
        Some((k, rest)) if k == "translate" => (RedoKind::Translate, rest),
        _ => return Err(usage()),
    };
    let mut model = None;
    let mut filter = crate::db::RedoFilter::default();
    let mut max_llm_calls = None;
    let mut it = rest.iter();
    while let Some(opt) = it.next() {
        let value = option_value(&mut it).ok_or_else(usage)?;
        match opt.as_str() {
            "--model" => model = Some(value.clone()),
            "--source" => filter.source_id = Some(value.clone()),
            "--since" => filter.since = Some(jst_midnight(value).ok_or_else(usage)?),
            "--min-score" => {
                let score: u8 = value.parse().map_err(|_| usage())?;
                if score > 100 {
                    return Err(usage());
                }
                filter.min_score = Some(score);
            }
            "--ids" => {
                filter.ids = value
                    .split(',')
                    .map(|id| id.trim().parse().map_err(|_| usage()))
                    .collect::<Result<_, _>>()?;
            }
            "--max-llm-calls" => max_llm_calls = Some(value.parse().map_err(|_| usage())?),
            _ => return Err(usage()),
        }
    }
    Ok(RedoArgs {
        kind,
        model: model.ok_or_else(usage)?,
        filter,
        max_llm_calls,
    })
}

/// オプションの値を取り出す。値の書き忘れで次のオプションを値として読まないよう、
/// 空の値と `--` で始まる値は受け付けない。
fn option_value<'a>(it: &mut impl Iterator<Item = &'a String>) -> Option<&'a String> {
    it.next().filter(|v| !v.is_empty() && !v.starts_with("--"))
}

/// "YYYY-MM-DD" を日本時間のその日の 0 時（UTC）にする。
fn jst_midnight(date: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    crate::jst::midnight(chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?)
}

/// `serve` サブコマンドの引数。
#[derive(Debug, PartialEq, Eq)]
pub struct ServeArgs {
    /// 待ち受けるアドレス（設定の `web.bind` より優先）
    pub addr: Option<std::net::SocketAddr>,
}

pub fn parse_serve_args(args: &[String]) -> Result<ServeArgs, ParseError> {
    let mut it = args.iter();
    let mut addr = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--addr" => {
                let value = option_value(&mut it).ok_or(ParseError::ServeUsage)?;
                addr = Some(value.parse().map_err(|_| ParseError::ServeUsage)?);
            }
            _ => return Err(ParseError::ServeUsage),
        }
    }
    Ok(ServeArgs { addr })
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

/// `topics` サブコマンドの引数。
#[derive(Debug, PartialEq, Eq)]
pub enum TopicsArgs {
    /// `topics import FILE`
    Import { file: PathBuf },
    /// `topics export`（標準出力へ）
    Export,
}

pub fn parse_topics_args(_args: &[String]) -> Result<TopicsArgs, ParseError> {
    Err(ParseError::TopicsUsage)
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
            ("topics", Command::Topics),
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
                requests_only: false,
                wait_lock: false,
            }
        );
        assert_eq!(
            parse_crawl_args(&args(&["--only", "fetch"])).unwrap(),
            CrawlArgs {
                until: None,
                only: Some(Stage::Fetch),
                max_llm_calls: None,
                requests_only: false,
                wait_lock: false,
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
    fn parses_wait_lock() {
        assert!(!parse_crawl_args(&[]).unwrap().wait_lock);
        let parsed = parse_crawl_args(&args(&["--wait-lock", "--requests-only"])).unwrap();
        assert!(parsed.wait_lock && parsed.requests_only);
    }

    #[test]
    fn parses_requests_only() {
        let parsed = parse_crawl_args(&args(&["--requests-only"])).unwrap();
        assert!(parsed.requests_only);
        // 依頼の処理は和訳ステージだけなので、ステージの指定とは併用できない
        for bad in [
            &["--requests-only", "--until", "digest"][..],
            &["--requests-only", "--only", "fetch"][..],
        ] {
            assert!(parse_crawl_args(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rejects_bad_crawl_args() {
        for bad in [
            &["--until"][..],
            &["--until", "nope"][..],
            &["--until", "fetch", "--only", "fetch"][..],
            &["--max-llm-calls", "--requests-only"][..],
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
    fn parses_redo_args() {
        let parsed = parse_redo_args(&args(&[
            "digest",
            "--model",
            "opus",
            "--source",
            "wnn",
            "--since",
            "2026-09-20",
            "--min-score",
            "70",
            "--ids",
            "3,5",
            "--max-llm-calls",
            "4",
        ]))
        .unwrap();
        assert_eq!(parsed.kind, RedoKind::Digest);
        assert_eq!(parsed.model, "opus");
        assert_eq!(parsed.filter.source_id.as_deref(), Some("wnn"));
        // 日付は JST の 0 時（UTC では前日 15 時）
        assert_eq!(
            parsed.filter.since.map(|t| t.to_rfc3339()),
            Some("2026-09-19T15:00:00+00:00".to_string())
        );
        assert_eq!(parsed.filter.min_score, Some(70));
        assert_eq!(parsed.filter.ids, [3, 5]);
        assert_eq!(parsed.max_llm_calls, Some(4));
        let minimal = parse_redo_args(&args(&["translate", "--model", "opus"])).unwrap();
        assert_eq!(minimal.kind, RedoKind::Translate);
        assert_eq!(minimal.filter, crate::db::RedoFilter::default());
    }

    #[test]
    fn rejects_bad_redo_args() {
        for bad in [
            &[][..],
            &["score", "--model", "opus"][..],
            &["digest"][..],
            &["digest", "--model"][..],
            &["digest", "--model", "opus", "--since", "2026/09/20"][..],
            &["digest", "--model", "opus", "--min-score", "101"][..],
            &["digest", "--model", "opus", "--ids", "a,b"][..],
            &["digest", "--model", "opus", "--bogus"][..],
            // 値を書き忘れて次のオプションを値として読まないこと、空の値を受け付けないこと
            &["digest", "--model", "--source", "wnn"][..],
            &["digest", "--model", ""][..],
            &["digest", "--model", "opus", "--source", "--ids", "1"][..],
        ] {
            let err = parse_redo_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::RedoUsage), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parses_serve_args() {
        assert_eq!(parse_serve_args(&[]).unwrap(), ServeArgs { addr: None });
        assert_eq!(
            parse_serve_args(&args(&["--addr", "100.64.0.1:8080"])).unwrap(),
            ServeArgs {
                addr: Some("100.64.0.1:8080".parse().unwrap())
            }
        );
        for bad in [
            &["--addr"][..],
            &["--addr", "localhost"],
            &["--addr", "--x"],
            &["extra"],
        ] {
            assert!(parse_serve_args(&args(bad)).is_err(), "{bad:?}");
        }
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
    fn parses_topics_args() {
        assert_eq!(
            parse_topics_args(&args(&["import", "t.toml"])).unwrap(),
            TopicsArgs::Import {
                file: PathBuf::from("t.toml")
            }
        );
        assert_eq!(
            parse_topics_args(&args(&["export"])).unwrap(),
            TopicsArgs::Export
        );
        for bad in [
            &[][..],
            &["import"][..],
            &["export", "x"][..],
            &["list"][..],
        ] {
            let err = parse_topics_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::TopicsUsage), "{bad:?}");
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
