//! `sources check`：各ソースを実際に取得して解析し、件数と先頭の数件を表示する。DB には書かない。

use std::fmt::Write as _;

use url::Url;

use crate::config::Source;
use crate::http::{Fetcher, HttpError};
use crate::source::{self, Candidate, SourceError};

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("unknown source id: {0}")]
    UnknownSource(String),
}

/// 1 つのソースの取得失敗。他のソースの確認は続ける。
#[derive(Debug, thiserror::Error)]
pub enum SourceFailure {
    #[error("invalid source url {url:?}: {source}")]
    InvalidUrl {
        url: String,
        source: url::ParseError,
    },
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Parse(#[from] SourceError),
}

#[derive(Debug)]
pub struct Report {
    pub id: String,
    pub outcome: Result<Stats, SourceFailure>,
}

#[derive(Debug)]
pub struct Stats {
    /// フィードに含まれていた件数
    pub total: usize,
    /// 絞り込み条件に一致したもの（フィードの順）
    pub matched: Vec<Candidate>,
}

/// `only` を指定したときは、無効化されたソースでもそれだけを確認する。
/// 指定しないときは有効なソースをすべて確認する。
pub async fn check(
    _fetcher: &Fetcher,
    _sources: &[Source],
    _only: Option<&str>,
) -> Result<Vec<Report>, CheckError> {
    todo!()
}

/// 各ソースの結果と、一致した記事の先頭 `samples` 件を表示用に整形する。
pub fn render(_reports: &[Report], _samples: usize) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{Category, Filter, Lang, SourceKind};
    use crate::testutil::{Route, Server, fixture};

    fn src(id: &str, kind: SourceKind, url: String, enabled: bool, filter: Filter) -> Source {
        Source {
            id: id.into(),
            name: id.into(),
            kind,
            url,
            lang: Lang::En,
            category: Category::Regulator,
            enabled,
            filter,
        }
    }

    fn fetcher() -> Fetcher {
        Fetcher::new("t", Duration::from_secs(2), Duration::ZERO).unwrap()
    }

    fn setup() -> (Server, Vec<Source>) {
        let server = Server::start(
            [
                ("/rss", Route::ok(fixture("rss2.xml"))),
                ("/fepc.json", Route::ok(fixture("fepc.json"))),
                ("/blocked", Route::status(403)),
            ]
            .into(),
        );
        let sources = vec![
            src(
                "reg",
                SourceKind::Feed,
                server.url("/rss"),
                true,
                Filter {
                    keywords: vec!["Power Reactor".into()],
                    url_contains: vec![],
                },
            ),
            src(
                "fepc",
                SourceKind::FepcJson,
                server.url("/fepc.json"),
                true,
                Filter::default(),
            ),
            src(
                "blocked",
                SourceKind::Feed,
                server.url("/blocked"),
                false,
                Filter::default(),
            ),
        ];
        (server, sources)
    }

    #[tokio::test]
    async fn checks_enabled_sources_and_applies_filters() {
        let (server, sources) = setup();
        let reports = check(&fetcher(), &sources, None).await.unwrap();
        let ids: Vec<_> = reports.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["reg", "fepc"]);

        let reg = reports[0].outcome.as_ref().unwrap();
        assert_eq!(reg.total, 2);
        assert_eq!(reg.matched.len(), 1);
        assert_eq!(
            reg.matched[0].title,
            "Power Reactor Event: Unit 2 automatic scram"
        );

        let fepc = reports[1].outcome.as_ref().unwrap();
        assert_eq!((fepc.total, fepc.matched.len()), (2, 2));
        assert!(fepc.matched[0].url.starts_with(&server.base));
    }

    #[tokio::test]
    async fn only_checks_named_source_even_if_disabled() {
        let (_server, sources) = setup();
        let reports = check(&fetcher(), &sources, Some("blocked")).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert!(matches!(
            reports[0].outcome,
            Err(SourceFailure::Http(HttpError::Status { .. }))
        ));
    }

    #[tokio::test]
    async fn unknown_source_is_error() {
        let (_server, sources) = setup();
        let err = check(&fetcher(), &sources, Some("nope")).await.unwrap_err();
        assert!(matches!(err, CheckError::UnknownSource(ref id) if id == "nope"));
    }

    #[test]
    fn renders_success_samples_and_failures() {
        let c = |title: &str| Candidate {
            url: format!("https://e/{title}"),
            title: title.into(),
            published_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-09-25T18:30:00Z")
                    .unwrap()
                    .to_utc(),
            ),
            summary: None,
            content: None,
        };
        let reports = vec![
            Report {
                id: "reg".into(),
                outcome: Ok(Stats {
                    total: 5,
                    matched: vec![c("first"), c("second"), c("third")],
                }),
            },
            Report {
                id: "bad".into(),
                outcome: Err(SourceFailure::InvalidUrl {
                    url: "::".into(),
                    source: url::ParseError::RelativeUrlWithoutBase,
                }),
            },
        ];
        let out = render(&reports, 2);
        assert!(out.contains("ok"), "{out}");
        assert!(out.contains("reg"), "{out}");
        assert!(out.contains("5 items, 3 matched"), "{out}");
        assert!(out.contains("2026-09-25"), "{out}");
        assert!(out.contains("first") && out.contains("second"), "{out}");
        assert!(!out.contains("third"), "samples are limited: {out}");
        assert!(out.contains("FAIL") && out.contains("bad"), "{out}");
    }
}
