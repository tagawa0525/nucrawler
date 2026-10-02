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
    #[error(
        "usage: nucrawler profile import FILE | nucrawler profile export | \
         nucrawler profile suggest --out FILE [--max-llm-calls N]"
    )]
    ProfileUsage,
    #[error("usage: nucrawler topics import FILE | nucrawler topics export")]
    TopicsUsage,
    #[error(
        "usage: nucrawler search [--since D] [--until D] [--topic T]... [--source ID]... \
         [--lang en|ja] [--translated] [--min-rating 1-5] [--read | --unread] [--bookmarked | --unbookmarked] [--unrated] [--min-score N] [--sort newest|score] \
         [--limit N] [WORD]...  (D: YYYY, YYYY-MM or YYYY-MM-DD)"
    )]
    SearchUsage,
    #[error(
        "usage: nucrawler redo digest|translate --model M [--source ID] [--since YYYY-MM-DD] \
         [--min-score N] [--ids 1,2,3] [--glossary] [--max-llm-calls N]"
    )]
    RedoUsage,
    #[error(
        "usage: nucrawler crawl [--until STAGE | --only STAGE | --requests-only] [--max-llm-calls N] [--wait-lock]  (stages: {stages})"
    )]
    CrawlUsage { stages: String },
    #[error("usage: nucrawler serve [--addr IP:PORT]")]
    ServeUsage,
    #[error("usage: nucrawler embed rebuild")]
    EmbedUsage,
    #[error("usage: nucrawler eval [--all] [--profile FILE [--max-llm-calls N]]")]
    EvalUsage,
    #[error(
        "usage: nucrawler user add LOGIN NAME | nucrawler user reset-password LOGIN | \
         nucrawler user disable LOGIN | nucrawler user rename LOGIN NEW_LOGIN | nucrawler user list  \
         (LOGIN: 1-254 bytes)"
    )]
    UserUsage,
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
    Profile,
    Topics,
    Search,
    Eval,
    User,
    Embed,
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
  status    ソースごとの取得状況と、ステージごとの失敗中・断念した記事の数を表示
  sources   ソースの取得確認（sources check [ID]）
  serve     Web UI を起動（serve [--addr IP:PORT]、既定は設定の web.bind）
  mcp       MCP stdio サーバを起動
  profile   関心プロファイルの取り込み・書き出し・更新案（profile import FILE / profile export / profile suggest --out FILE）
  topics    トピックの語彙の取り込み・書き出し（topics import FILE / topics export）
  search    記事を検索（search [--since D] [--topic T] ... 語...、条件は Web の検索画面と同じ）
  eval      採点が記事に付けた評価（★1〜5）とどれだけ合っているかを表示（eval [--all] [--profile FILE [--max-llm-calls N]]）
  embed     embedding を作り直す（embed rebuild：モデルや設定を替えた後、次の crawl で全件を作り直す）
  user      Web UI の利用者の管理（user add LOGIN NAME / reset-password LOGIN / disable LOGIN / rename LOGIN NEW_LOGIN / list）
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
        Some("profile") => Command::Profile,
        Some("topics") => Command::Topics,
        Some("search") => Command::Search,
        Some("eval") => Command::Eval,
        Some("user") => Command::User,
        Some("embed") => Command::Embed,
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
    /// 取得（fetch・extract）や語彙の整理を別の実行が行っていれば、終わるのを待ってから始める（timer 用。指定しなければ
    /// 終了コード 75 で終わる）。LLM のステージはロックを取らないので待たない
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
    /// 訳語集が変わった後に作られていない版だけを作り直す
    pub glossary: bool,
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
    let mut glossary = false;
    let mut it = rest.iter();
    while let Some(opt) = it.next() {
        // 値を取らないオプション
        if opt == "--glossary" {
            glossary = true;
            continue;
        }
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
        glossary,
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

/// `eval` サブコマンドの引数。
#[derive(Debug, PartialEq, Eq, Default)]
pub struct EvalArgs {
    /// 現行のキーだけでなく、過去のプロファイル・プロンプトの版の採点も並べる
    pub all: bool,
    /// 候補のプロファイル。ラベルの付いた記事をこれで採点してから、現行と並べる
    pub profile: Option<PathBuf>,
    /// 候補で採点するときの LLM の呼び出しの上限
    pub max_llm_calls: Option<u32>,
}

pub fn parse_eval_args(args: &[String]) -> Result<EvalArgs, ParseError> {
    let mut it = args.iter();
    let mut parsed = EvalArgs::default();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--all" => parsed.all = true,
            "--profile" => {
                let file = option_value(&mut it).ok_or(ParseError::EvalUsage)?;
                parsed.profile = Some(PathBuf::from(file));
            }
            "--max-llm-calls" => {
                let n = option_value(&mut it).ok_or(ParseError::EvalUsage)?;
                parsed.max_llm_calls = Some(n.parse().map_err(|_| ParseError::EvalUsage)?);
            }
            _ => return Err(ParseError::EvalUsage),
        }
    }
    // 上限は候補で採点するときだけ意味がある
    if parsed.max_llm_calls.is_some() && parsed.profile.is_none() {
        return Err(ParseError::EvalUsage);
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
    /// `profile suggest --out FILE [--max-llm-calls N]`：反応を根拠に更新案を作り、`out` に書く
    Suggest {
        out: PathBuf,
        max_llm_calls: Option<u32>,
    },
}

pub fn parse_profile_args(args: &[String]) -> Result<ProfileArgs, ParseError> {
    match args {
        [cmd, file] if cmd == "import" => Ok(ProfileArgs::Import {
            file: PathBuf::from(file),
        }),
        [cmd] if cmd == "export" => Ok(ProfileArgs::Export),
        [cmd, rest @ ..] if cmd == "suggest" => parse_suggest_args(rest),
        _ => Err(ParseError::ProfileUsage),
    }
}

fn parse_suggest_args(args: &[String]) -> Result<ProfileArgs, ParseError> {
    let mut it = args.iter();
    let (mut out, mut max_llm_calls) = (None, None);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => {
                out = Some(PathBuf::from(
                    option_value(&mut it).ok_or(ParseError::ProfileUsage)?,
                ));
            }
            "--max-llm-calls" => {
                let n = option_value(&mut it).ok_or(ParseError::ProfileUsage)?;
                max_llm_calls = Some(n.parse().map_err(|_| ParseError::ProfileUsage)?);
            }
            _ => return Err(ParseError::ProfileUsage),
        }
    }
    Ok(ProfileArgs::Suggest {
        out: out.ok_or(ParseError::ProfileUsage)?,
        max_llm_calls,
    })
}

/// `search` サブコマンドの引数。条件は Web の検索画面と同じ（`search::Params`）。
#[derive(Debug, PartialEq, Eq)]
pub struct SearchArgs {
    pub params: crate::search::Params,
    /// 最大件数（既定は設定の `web.list_limit`）
    pub limit: Option<usize>,
}

/// 印で絞る条件を付ける。逆の指定が既にあれば誤り（`--read` と `--unread` など）。
fn set_mark(mark: &mut Option<bool>, on: bool) -> Result<(), ParseError> {
    if *mark == Some(!on) {
        return Err(ParseError::SearchUsage);
    }
    *mark = Some(on);
    Ok(())
}

/// オプション以外の引数は検索語として空白でつなぐ。
pub fn parse_search_args(args: &[String]) -> Result<SearchArgs, ParseError> {
    fn value<'a>(it: &mut impl Iterator<Item = &'a String>) -> Result<String, ParseError> {
        option_value(it).cloned().ok_or(ParseError::SearchUsage)
    }
    let mut params = crate::search::Params::default();
    let mut words = Vec::new();
    let mut limit = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--since" => params.since = value(&mut it)?,
            "--until" => params.until = value(&mut it)?,
            "--topic" => params.topics.push(value(&mut it)?),
            "--source" => params.sources.push(value(&mut it)?),
            "--lang" => params.lang = value(&mut it)?,
            "--min-rating" => params.min_rating = value(&mut it)?,
            "--min-score" => params.min_score = value(&mut it)?,
            "--sort" => params.sort = value(&mut it)?,
            "--translated" => params.translated = true,
            // あり・なしは片方だけ（両方の指定は誤り）
            "--unread" => set_mark(&mut params.read, false)?,
            "--read" => set_mark(&mut params.read, true)?,
            "--bookmarked" => set_mark(&mut params.bookmarked, true)?,
            "--unbookmarked" => set_mark(&mut params.bookmarked, false)?,
            "--unrated" => params.unrated = true,
            "--limit" => {
                let n = value(&mut it)?
                    .parse()
                    .ok()
                    .filter(|&n: &usize| n > 0)
                    .ok_or(ParseError::SearchUsage)?;
                limit = Some(n);
            }
            other if other.starts_with("--") => return Err(ParseError::SearchUsage),
            word => words.push(word.to_string()),
        }
    }
    params.q = words.join(" ");
    Ok(SearchArgs { params, limit })
}

/// `topics` サブコマンドの引数。
#[derive(Debug, PartialEq, Eq)]
pub enum TopicsArgs {
    /// `topics import FILE`
    Import { file: PathBuf },
    /// `topics export`（標準出力へ）
    Export,
}

pub fn parse_topics_args(args: &[String]) -> Result<TopicsArgs, ParseError> {
    match args {
        [cmd, file] if cmd == "import" => Ok(TopicsArgs::Import {
            file: PathBuf::from(file),
        }),
        [cmd] if cmd == "export" => Ok(TopicsArgs::Export),
        _ => Err(ParseError::TopicsUsage),
    }
}

/// `embed` サブコマンドの引数。
#[derive(Debug, PartialEq, Eq)]
pub enum EmbedArgs {
    /// `embed rebuild`：ベクトルの空間とベクトルをすべて消す（次の crawl が今の設定とモデルで作り直す）
    Rebuild,
}

pub fn parse_embed_args(args: &[String]) -> Result<EmbedArgs, ParseError> {
    match args {
        [cmd] if cmd == "rebuild" => Ok(EmbedArgs::Rebuild),
        _ => Err(ParseError::EmbedUsage),
    }
}

/// `user` サブコマンドの引数。パスワードは引数で受け取らず、CLI が作って表示する（シェルの履歴に残さないため）。
#[derive(Debug, PartialEq, Eq)]
pub enum UserArgs {
    /// `user add LOGIN NAME`：利用者を作り、初期パスワードを表示する
    Add { login: String, display_name: String },
    /// `user reset-password LOGIN`：資格をすべて失効させ、新しいパスワードを表示する
    ResetPassword { login: String },
    /// `user disable LOGIN`：資格をすべて失効させる（戻すときは reset-password）
    Disable { login: String },
    /// `user rename LOGIN NEW_LOGIN`
    Rename { login: String, new_login: String },
    /// `user list`
    List,
}

pub fn parse_user_args(args: &[String]) -> Result<UserArgs, ParseError> {
    let login = |s: &String| {
        crate::auth::valid_login(s)
            .then(|| s.clone())
            .ok_or(ParseError::UserUsage)
    };
    match args {
        [cmd, id, name] if cmd == "add" => Ok(UserArgs::Add {
            login: login(id)?,
            display_name: name.clone(),
        }),
        [cmd, id] if cmd == "reset-password" => Ok(UserArgs::ResetPassword { login: login(id)? }),
        [cmd, id] if cmd == "disable" => Ok(UserArgs::Disable { login: login(id)? }),
        [cmd, id, new] if cmd == "rename" => Ok(UserArgs::Rename {
            login: login(id)?,
            new_login: login(new)?,
        }),
        [cmd] if cmd == "list" => Ok(UserArgs::List),
        _ => Err(ParseError::UserUsage),
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
    fn parses_eval_args() {
        assert_eq!(parse_eval_args(&[]).unwrap(), EvalArgs::default());
        assert_eq!(
            parse_eval_args(&args(&["--all"])).unwrap(),
            EvalArgs {
                all: true,
                ..EvalArgs::default()
            }
        );
        assert_eq!(
            parse_eval_args(&args(&[
                "--profile",
                "p.toml",
                "--max-llm-calls",
                "3",
                "--all"
            ]))
            .unwrap(),
            EvalArgs {
                all: true,
                profile: Some("p.toml".into()),
                max_llm_calls: Some(3),
            }
        );
        for bad in [
            &["--bogus"][..],
            &["--profile"],
            &["--max-llm-calls", "x"],
            // 上限は候補で採点するときだけ意味がある
            &["--max-llm-calls", "3"],
        ] {
            assert!(
                matches!(parse_eval_args(&args(bad)), Err(ParseError::EvalUsage)),
                "{bad:?}"
            );
        }
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
            ("profile", Command::Profile),
            ("topics", Command::Topics),
            ("search", Command::Search),
            ("eval", Command::Eval),
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
        assert!(!minimal.glossary);
        // --glossary は値を取らない
        let glossary =
            parse_redo_args(&args(&["translate", "--glossary", "--model", "opus"])).unwrap();
        assert!(glossary.glossary);
        assert_eq!(glossary.model, "opus");
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
        assert_eq!(
            parse_profile_args(&args(&["suggest", "--out", "new.toml"])).unwrap(),
            ProfileArgs::Suggest {
                out: PathBuf::from("new.toml"),
                max_llm_calls: None,
            }
        );
        assert_eq!(
            parse_profile_args(&args(&[
                "suggest",
                "--max-llm-calls",
                "1",
                "--out",
                "n.toml"
            ]))
            .unwrap(),
            ProfileArgs::Suggest {
                out: PathBuf::from("n.toml"),
                max_llm_calls: Some(1),
            }
        );
        for bad in [
            &[][..],
            &["import"][..],
            &["export", "x"][..],
            // 案の書き出し先は必須
            &["suggest"][..],
            &["suggest", "--out"][..],
            &["suggest", "--out", "a", "--bogus"][..],
            &["show"][..],
        ] {
            let err = parse_profile_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::ProfileUsage), "{bad:?}");
        }
    }

    #[test]
    fn parses_embed_args() {
        assert_eq!(
            parse(args(&["embed", "rebuild"])).unwrap().command,
            Command::Embed
        );
        assert_eq!(
            parse_embed_args(&args(&["rebuild"])).unwrap(),
            EmbedArgs::Rebuild
        );
        for bad in [&[][..], &["rebuild", "x"][..], &["status"][..]] {
            let err = parse_embed_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::EmbedUsage), "{bad:?}");
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
    fn parses_search_args() {
        let parsed = parse_search_args(&args(&[
            "炉心",
            "--since",
            "2026-09",
            "--until",
            "2026-09-20",
            "--topic",
            "燃料",
            "--topic",
            "PWR",
            "--source",
            "nra",
            "--lang",
            "ja",
            "--translated",
            "--min-rating",
            "4",
            "--unread",
            "--bookmarked",
            "--unrated",
            "--min-score",
            "60",
            "--sort",
            "score",
            "--limit",
            "5",
            "NRC",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            SearchArgs {
                params: crate::search::Params {
                    q: "炉心 NRC".into(),
                    since: "2026-09".into(),
                    until: "2026-09-20".into(),
                    topics: vec!["燃料".into(), "PWR".into()],
                    sources: vec!["nra".into()],
                    lang: "ja".into(),
                    translated: true,
                    read: Some(false),
                    bookmarked: Some(true),
                    unrated: true,
                    hide_low: false,
                    min_rating: "4".into(),
                    min_score: "60".into(),
                    sort: "score".into(),
                },
                limit: Some(5),
            }
        );
        // 既読だけ・ブックマークしていない記事だけ
        let p = parse_search_args(&["--read".to_string(), "--unbookmarked".to_string()])
            .unwrap()
            .params;
        assert_eq!((p.read, p.bookmarked), (Some(true), Some(false)));
        // あり・なしを両方指定するのは誤り
        for pair in [["--read", "--unread"], ["--bookmarked", "--unbookmarked"]] {
            let args: Vec<String> = pair.iter().map(|a| a.to_string()).collect();
            assert!(parse_search_args(&args).is_err(), "{pair:?}");
        }
        assert_eq!(
            parse_search_args(&[]).unwrap(),
            SearchArgs {
                params: crate::search::Params::default(),
                limit: None
            }
        );
        for bad in [
            &["--since"][..],
            &["--limit", "x"][..],
            &["--limit", "0"][..],
            &["--bogus"][..],
        ] {
            let err = parse_search_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::SearchUsage), "{bad:?}");
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
    fn parses_user_args() {
        assert_eq!(
            parse(args(&["user", "list"])).unwrap().command,
            Command::User
        );
        assert_eq!(
            parse_user_args(&args(&["add", "a@example.com", "A さん"])).unwrap(),
            UserArgs::Add {
                login: "a@example.com".into(),
                display_name: "A さん".into()
            }
        );
        for (cmd, expected) in [
            (
                "reset-password",
                UserArgs::ResetPassword { login: "a".into() },
            ),
            ("disable", UserArgs::Disable { login: "a".into() }),
        ] {
            assert_eq!(parse_user_args(&args(&[cmd, "a"])).unwrap(), expected);
        }
        assert_eq!(
            parse_user_args(&args(&["rename", "owner", "me@example.com"])).unwrap(),
            UserArgs::Rename {
                login: "owner".into(),
                new_login: "me@example.com".into()
            }
        );
        assert_eq!(parse_user_args(&args(&["list"])).unwrap(), UserArgs::List);
    }

    #[test]
    fn rejects_bad_user_args() {
        for bad in [
            &[][..],
            &["add", "a"][..],
            &["disable"][..],
            &["remove", "a"][..],
            &["list", "x"][..],
            // ログイン ID は空でなく 254 バイト以下
            &["add", "", "A"][..],
            &["rename", "owner", " "][..],
            &["rename", "", "me@example.com"][..],
            &["reset-password", ""][..],
            &["disable", " "][..],
        ] {
            let err = parse_user_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::UserUsage), "{bad:?}: {err}");
        }
        let long = "a".repeat(255);
        for bad in [
            &["add", &long, "A"][..],
            &["reset-password", &long][..],
            &["disable", &long][..],
            &["rename", &long, "me@example.com"][..],
        ] {
            let err = parse_user_args(&args(bad)).unwrap_err();
            assert!(matches!(err, ParseError::UserUsage), "{bad:?}: {err}");
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
