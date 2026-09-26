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
    fetcher: &Fetcher,
    sources: &[Source],
    only: Option<&str>,
) -> Result<Vec<Report>, CheckError> {
    let targets: Vec<&Source> = match only {
        Some(id) => vec![
            sources
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| CheckError::UnknownSource(id.to_string()))?,
        ],
        None => sources.iter().filter(|s| s.enabled).collect(),
    };
    let mut reports = Vec::with_capacity(targets.len());
    for s in targets {
        let outcome = check_one(fetcher, s).await;
        if let Err(e) = &outcome {
            tracing::warn!(source = %s.id, "{}", error_chain(e));
        }
        reports.push(Report {
            id: s.id.clone(),
            outcome,
        });
    }
    Ok(reports)
}

async fn check_one(fetcher: &Fetcher, s: &Source) -> Result<Stats, SourceFailure> {
    let url = Url::parse(&s.url).map_err(|source| SourceFailure::InvalidUrl {
        url: s.url.clone(),
        source,
    })?;
    let fetched = fetcher.get(&url).await?;
    let candidates = source::parse(s.kind, &fetched.body, &fetched.url)?;
    let total = candidates.len();
    let matched = candidates
        .into_iter()
        .filter(|c| source::matches(&s.filter, c))
        .collect();
    Ok(Stats { total, matched })
}

/// 表示は日本時間で行う。
fn jst() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(9 * 3600).expect("valid offset")
}

/// エラーと、その原因（`source()`）を ": " でつないだ文字列。
pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        let _ = write!(out, ": {c}");
        cause = c.source();
    }
    out
}

/// 各ソースの結果と、一致した記事の先頭 `samples` 件を表示用に整形する。
pub fn render(reports: &[Report], samples: usize) -> String {
    let width = reports.iter().map(|r| r.id.len()).max().unwrap_or(0);
    let mut out = String::new();
    for r in reports {
        match &r.outcome {
            Ok(stats) => {
                let _ = writeln!(
                    out,
                    "ok    {:width$}  {} items, {} matched",
                    r.id,
                    stats.total,
                    stats.matched.len()
                );
                for c in stats.matched.iter().take(samples) {
                    let date = c.published_at.map_or_else(
                        || "----------".to_string(),
                        |d| d.with_timezone(&jst()).format("%Y-%m-%d").to_string(),
                    );
                    let _ = writeln!(out, "      {:width$}  {date}  {}", "", c.title);
                }
            }
            Err(e) => {
                let _ = writeln!(out, "FAIL  {:width$}  {}", r.id, error_chain(e));
            }
        }
    }
    out
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
        Fetcher::new("t", Duration::from_secs(2), Duration::ZERO, 1 << 20).unwrap()
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

    /// 有効なソースが失敗しても、後続のソースの確認を続ける。
    #[tokio::test]
    async fn continues_after_an_enabled_source_fails() {
        let (server, _) = setup();
        let sources = vec![
            src(
                "blocked",
                SourceKind::Feed,
                server.url("/blocked"),
                true,
                Filter::default(),
            ),
            src(
                "fepc",
                SourceKind::FepcJson,
                server.url("/fepc.json"),
                true,
                Filter::default(),
            ),
        ];
        let reports = check(&fetcher(), &sources, None).await.unwrap();
        let ids: Vec<_> = reports.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["blocked", "fepc"]);
        assert!(reports[0].outcome.is_err());
        assert!(reports[1].outcome.is_ok());
    }

    /// 転送された場合、相対リンクは実際にフィードを返した URL を基準に解決する。
    #[tokio::test]
    async fn resolves_links_against_final_redirected_url() {
        let feed = br#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>t</title><link>https://e/</link><description>d</description>
  <item><title>rel</title><link>article/1</link></item>
</channel></rss>"#;
        let server = Server::start(
            [
                ("/feed", Route::redirect("/feeds/rss")),
                ("/feeds/rss", Route::ok(feed.to_vec())),
            ]
            .into(),
        );
        let sources = vec![src(
            "moved",
            SourceKind::Feed,
            server.url("/feed"),
            true,
            Filter::default(),
        )];
        let reports = check(&fetcher(), &sources, None).await.unwrap();
        let stats = reports[0].outcome.as_ref().unwrap();
        assert_eq!(stats.matched[0].url, server.url("/feeds/article/1"));
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
        // 2026-09-25T18:30Z は JST では 9/26
        assert!(out.contains("2026-09-26"), "{out}");
        assert!(out.contains("first") && out.contains("second"), "{out}");
        assert!(!out.contains("third"), "samples are limited: {out}");
        assert!(out.contains("FAIL") && out.contains("bad"), "{out}");
    }

    /// 通信エラーなどは原因のエラーまで表示しないと、何が起きたか分からない。
    #[test]
    fn renders_error_causes() {
        #[derive(Debug, thiserror::Error)]
        #[error("connection reset by peer")]
        struct Root;
        #[derive(Debug, thiserror::Error)]
        #[error("error sending request")]
        struct Outer(#[source] Root);

        let reports = vec![Report {
            id: "x".into(),
            outcome: Err(SourceFailure::Parse(SourceError::Json(
                serde_json::from_str::<u8>("\"a\"").unwrap_err(),
            ))),
        }];
        let out = render(&reports, 0);
        assert!(out.contains("failed to parse json"), "{out}");

        assert_eq!(
            error_chain(&Outer(Root)),
            "error sending request: connection reset by peer"
        );
    }
}
