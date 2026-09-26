use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{path}: duplicate source id: {id}")]
    DuplicateSourceId { path: PathBuf, id: String },
    #[error("cannot determine config directory: neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigDir,
}

#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub http: HttpConfig,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub user_agent: String,
    pub per_host_delay_secs: u64,
    pub timeout_secs: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        todo!()
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

pub fn parse_config(_text: &str, _path: &Path) -> Result<Config, ConfigError> {
    todo!()
}

pub fn parse_sources(_text: &str, _path: &Path) -> Result<Sources, ConfigError> {
    todo!()
}

/// `dir` の config.toml（無ければ既定値）と sources.toml（必須）を読む。
pub fn load(_dir: &Path) -> Result<(Config, Sources), ConfigError> {
    todo!()
}

/// `$XDG_CONFIG_HOME/nucrawler`、無ければ `$HOME/.config/nucrawler`。
pub fn default_dir(_env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, ConfigError> {
    todo!()
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
    fn default_dir_without_env_is_error() {
        let err = default_dir(|_| None).unwrap_err();
        assert!(matches!(err, ConfigError::NoConfigDir), "{err}");
    }
}
