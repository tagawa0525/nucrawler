use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error("{path}: duplicate source id: {id}")]
    DuplicateSourceId { path: PathBuf, id: String },
    #[error("cannot determine config directory: neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigDir,
    #[error("cannot determine data directory: neither XDG_DATA_HOME nor HOME is set")]
    NoDataDir,
}

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub http: HttpConfig,
    pub pipeline: PipelineConfig,
    pub llm: LlmConfig,
    pub quota: crate::quota::QuotaConfig,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    /// `claude` の実行ファイル（PATH から探す）
    pub command: String,
    /// 1 回の呼び出しのタイムアウト
    pub timeout_secs: u64,
    /// 要約に使うモデル
    pub digest_model: String,
    /// 1 回の呼び出しで要約する記事数
    pub digest_batch_size: usize,
    /// プロンプトに入れる本文の部分ごとの最大文字数
    pub max_input_chars: usize,
    /// 採点に使うモデル
    pub score_model: String,
    /// 1 回の呼び出しで採点する記事数
    pub score_batch_size: usize,
}

impl LlmConfig {
    /// 0 だと処理が黙って何もしなくなる値を拒否する。
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("digest_batch_size", self.digest_batch_size as u64),
            ("max_input_chars", self.max_input_chars as u64),
            ("score_batch_size", self.score_batch_size as u64),
            ("timeout_secs", self.timeout_secs),
        ] {
            if value == 0 {
                return Err(format!("llm.{name} must be at least 1"));
            }
        }
        Ok(())
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            command: "claude".into(),
            timeout_secs: 300,
            digest_model: "sonnet".into(),
            digest_batch_size: 5,
            max_input_chars: 6000,
            score_model: "sonnet".into(),
            score_batch_size: 20,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PipelineConfig {
    /// これより古い記事は、本文の抽出や LLM の処理の対象にしない（取り込み済みの過去記事を
    /// 一度に処理しないため）。公開日時が無い記事は取得日時で判断する。
    pub backlog_days: u32,
    /// 1 回の実行で本文を抽出する記事数の上限
    pub extract_max_per_run: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            backlog_days: 14,
            extract_max_per_run: 100,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub user_agent: String,
    pub per_host_delay_secs: u64,
    pub timeout_secs: u64,
    /// 応答本文の上限。これを超える応答は読み込まずにエラーにする。
    pub max_body_bytes: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            user_agent: concat!(
                "nucrawler/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/tagawa0525/nucrawler)"
            )
            .to_string(),
            per_host_delay_secs: 5,
            timeout_secs: 30,
            max_body_bytes: 20 * 1024 * 1024,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    #[serde(rename = "source", default)]
    pub sources: Vec<Source>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    pub url: String,
    pub lang: Lang,
    pub category: Category,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub filter: Filter,
    /// 記事ページの本文を示す CSS セレクタ。無ければ readability で推定する。
    #[serde(default)]
    pub body_selector: Option<String>,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// RSS / Atom / RDF
    Feed,
    /// 電事連のニュース一覧 JSON
    FepcJson,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    En,
    Ja,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Regulator,
    Industry,
    Research,
    Utility,
    Vendor,
    Paper,
}

/// 原子力関連に絞り込む条件。いずれかに一致したものを取り込む。空なら全件。
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    pub keywords: Vec<String>,
    pub url_contains: Vec<String>,
}

pub fn parse_config(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let config: Config = parse_toml(text, path)?;
    config
        .quota
        .validate()
        .and_then(|()| config.llm.validate())
        .map_err(|reason| ConfigError::Invalid {
            path: path.to_path_buf(),
            reason,
        })?;
    Ok(config)
}

pub fn parse_sources(text: &str, path: &Path) -> Result<Sources, ConfigError> {
    let sources: Sources = parse_toml(text, path)?;
    let mut seen = HashSet::new();
    for s in &sources.sources {
        if !seen.insert(s.id.as_str()) {
            return Err(ConfigError::DuplicateSourceId {
                path: path.to_path_buf(),
                id: s.id.clone(),
            });
        }
    }
    Ok(sources)
}

fn parse_toml<T: serde::de::DeserializeOwned>(text: &str, path: &Path) -> Result<T, ConfigError> {
    toml::from_str(text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// `dir` の config.toml（無ければ既定値）と sources.toml（必須）を読む。
pub fn load(dir: &Path) -> Result<(Config, Sources), ConfigError> {
    let config_path = dir.join("config.toml");
    let config = match std::fs::read_to_string(&config_path) {
        Ok(text) => parse_config(&text, &config_path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(source) => {
            return Err(ConfigError::Read {
                path: config_path,
                source,
            });
        }
    };
    let sources_path = dir.join("sources.toml");
    let text = std::fs::read_to_string(&sources_path).map_err(|source| ConfigError::Read {
        path: sources_path.clone(),
        source,
    })?;
    let sources = parse_sources(&text, &sources_path)?;
    Ok((config, sources))
}

/// `$XDG_CONFIG_HOME/nucrawler`、無ければ `$HOME/.config/nucrawler`。
pub fn default_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, ConfigError> {
    let non_empty = |k| env(k).filter(|v: &OsString| !v.is_empty());
    let base = match (non_empty("XDG_CONFIG_HOME"), non_empty("HOME")) {
        (Some(xdg), _) => PathBuf::from(xdg),
        (None, Some(home)) => PathBuf::from(home).join(".config"),
        (None, None) => return Err(ConfigError::NoConfigDir),
    };
    Ok(base.join("nucrawler"))
}

/// `$XDG_DATA_HOME/nucrawler`、無ければ `$HOME/.local/share/nucrawler`（DB やロックファイルを置く）。
pub fn default_data_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, ConfigError> {
    let non_empty = |k| env(k).filter(|v: &OsString| !v.is_empty());
    let base = match (non_empty("XDG_DATA_HOME"), non_empty("HOME")) {
        (Some(xdg), _) => PathBuf::from(xdg),
        (None, Some(home)) => PathBuf::from(home).join(".local/share"),
        (None, None) => return Err(ConfigError::NoDataDir),
    };
    Ok(base.join("nucrawler"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> &'static Path {
        Path::new("test.toml")
    }

    #[test]
    fn empty_config_uses_defaults() {
        let c = parse_config("", p()).unwrap();
        assert_eq!(c, Config::default());
        assert!(c.http.user_agent.starts_with("nucrawler/"));
        assert!(c.http.per_host_delay_secs > 0);
        assert!(c.http.timeout_secs > 0);
        assert!(c.http.max_body_bytes > 0);
    }

    #[test]
    fn pipeline_defaults_and_overrides() {
        let d = PipelineConfig::default();
        assert_eq!((d.backlog_days, d.extract_max_per_run), (14, 100));
        let c = parse_config("[pipeline]\nbacklog_days = 3\n", p()).unwrap();
        assert_eq!(c.pipeline.backlog_days, 3);
        assert_eq!(c.pipeline.extract_max_per_run, 100);
    }

    #[test]
    fn rejects_invalid_quota_values() {
        for (toml, needle) in [
            ("[quota]\nweekly_max = 2.0\n", "weekly_max"),
            (
                "[quota]\ndefault_max_five_hour = -0.1\n",
                "default_max_five_hour",
            ),
            (
                "[quota]\ndefault_max_five_hour = nan\n",
                "default_max_five_hour",
            ),
            ("[quota]\npace_ahead_days = -1.0\n", "pace_ahead_days"),
            (
                "[quota]\ntimezone_offset_hours = 24\n",
                "timezone_offset_hours",
            ),
            (
                "[quota]\nslots = [{ start = 10, end = 10, max_five_hour = 0.5 }]\n",
                "slot",
            ),
            (
                "[quota]\nslots = [{ start = 20, end = 25, max_five_hour = 0.5 }]\n",
                "slot",
            ),
            (
                "[quota]\nslots = [{ start = 1, end = 2, max_five_hour = 1.5 }]\n",
                "slot",
            ),
        ] {
            let err = parse_config(toml, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains(needle)),
                "{toml}: {err}"
            );
        }
    }

    #[test]
    fn rejects_non_positive_llm_settings() {
        for (toml, needle) in [
            ("[llm]\ndigest_batch_size = 0\n", "digest_batch_size"),
            ("[llm]\nmax_input_chars = 0\n", "max_input_chars"),
            ("[llm]\ntimeout_secs = 0\n", "timeout_secs"),
            ("[llm]\nscore_batch_size = 0\n", "score_batch_size"),
        ] {
            let err = parse_config(toml, p()).unwrap_err();
            assert!(
                matches!(&err, ConfigError::Invalid { reason, .. } if reason.contains(needle)),
                "{toml}: {err}"
            );
        }
    }

    #[test]
    fn llm_defaults() {
        let d = LlmConfig::default();
        assert_eq!(d.command, "claude");
        assert_eq!(d.timeout_secs, 300);
        assert_eq!(d.digest_model, "sonnet");
        assert_eq!(d.digest_batch_size, 5);
        assert_eq!(d.max_input_chars, 6000);
        assert_eq!(d.score_model, "sonnet");
        assert_eq!(d.score_batch_size, 20);
    }

    #[test]
    fn parses_body_selector() {
        let s = parse_sources(
            r#"
            [[source]]
            id = "a"
            name = "A"
            kind = "feed"
            url = "https://example.com/rss"
            lang = "ja"
            category = "utility"
            body_selector = "div#contents"
            "#,
            p(),
        )
        .unwrap();
        assert_eq!(s.sources[0].body_selector.as_deref(), Some("div#contents"));
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let c = parse_config("[http]\nuser_agent = \"x\"\n", p()).unwrap();
        assert_eq!(c.http.user_agent, "x");
        assert_eq!(
            c.http.per_host_delay_secs,
            HttpConfig::default().per_host_delay_secs
        );
    }

    #[test]
    fn unknown_config_field_is_error() {
        let err = parse_config("[http]\nuser_agnet = \"x\"\n", p()).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    }

    #[test]
    fn parses_source_with_defaults_and_filter() {
        let s = parse_sources(
            r#"
            [[source]]
            id = "a"
            name = "A"
            kind = "feed"
            url = "https://example.com/rss"
            lang = "ja"
            category = "utility"
            filter = { keywords = ["原子力"] }

            [[source]]
            id = "b"
            name = "B"
            kind = "fepc_json"
            url = "https://example.com/index.json"
            lang = "en"
            category = "industry"
            enabled = false
            "#,
            p(),
        )
        .unwrap();
        assert_eq!(
            s.sources[0],
            Source {
                id: "a".into(),
                name: "A".into(),
                kind: SourceKind::Feed,
                url: "https://example.com/rss".into(),
                lang: Lang::Ja,
                category: Category::Utility,
                enabled: true,
                filter: Filter {
                    keywords: vec!["原子力".into()],
                    url_contains: vec![],
                },
                body_selector: None,
            }
        );
        assert_eq!(s.sources[1].kind, SourceKind::FepcJson);
        assert!(!s.sources[1].enabled);
        assert_eq!(s.sources[1].filter, Filter::default());
    }

    #[test]
    fn duplicate_source_id_is_error() {
        let one = r#"
            [[source]]
            id = "a"
            name = "A"
            kind = "feed"
            url = "https://example.com/rss"
            lang = "en"
            category = "industry"
        "#;
        let err = parse_sources(&format!("{one}\n{one}"), p()).unwrap_err();
        assert!(
            matches!(&err, ConfigError::DuplicateSourceId { id, .. } if id == "a"),
            "{err}"
        );
    }

    #[test]
    fn unknown_kind_is_error() {
        let err = parse_sources(
            r#"
            [[source]]
            id = "a"
            name = "A"
            kind = "gopher"
            url = "gopher://example.com"
            lang = "en"
            category = "industry"
            "#,
            p(),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    }

    #[test]
    fn examples_are_valid() {
        parse_config(include_str!("../examples/config.toml"), p()).unwrap();
        let s = parse_sources(include_str!("../examples/sources.toml"), p()).unwrap();
        assert!(!s.sources.is_empty());
    }

    /// テストごとに独立した一時ディレクトリ。
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nucrawler-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_without_config_toml_uses_defaults() {
        let dir = temp_dir("load-defaults");
        std::fs::write(dir.join("sources.toml"), "").unwrap();
        let (c, s) = load(&dir).unwrap();
        assert_eq!(c, Config::default());
        assert!(s.sources.is_empty());
    }

    #[test]
    fn load_without_sources_toml_is_error() {
        let dir = temp_dir("load-no-sources");
        let err = load(&dir).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Read { path, .. } if path.ends_with("sources.toml")),
            "{err}"
        );
    }

    #[test]
    fn default_dir_prefers_xdg() {
        let dir = default_dir(|k| match k {
            "XDG_CONFIG_HOME" => Some("/xdg".into()),
            "HOME" => Some("/home/u".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(dir, PathBuf::from("/xdg/nucrawler"));
    }

    #[test]
    fn default_dir_falls_back_to_home() {
        let dir = default_dir(|k| (k == "HOME").then(|| "/home/u".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.config/nucrawler"));
    }

    #[test]
    fn default_data_dir_prefers_xdg_then_home() {
        let xdg = default_data_dir(|k| match k {
            "XDG_DATA_HOME" => Some("/xdg".into()),
            "HOME" => Some("/home/u".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(xdg, PathBuf::from("/xdg/nucrawler"));
        let home = default_data_dir(|k| (k == "HOME").then(|| "/home/u".into())).unwrap();
        assert_eq!(home, PathBuf::from("/home/u/.local/share/nucrawler"));
        let err = default_data_dir(|_| None).unwrap_err();
        assert!(matches!(err, ConfigError::NoDataDir), "{err}");
    }

    #[test]
    fn default_dir_without_env_is_error() {
        let err = default_dir(|_| None).unwrap_err();
        assert!(matches!(err, ConfigError::NoConfigDir), "{err}");
    }
}
