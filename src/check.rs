//! `sources check`：各ソースを実際に取得して解析し、件数と先頭の数件を表示する。DB には書かない。

use std::collections::HashSet;
use std::fmt::Write as _;

use url::Url;

use crate::config::{HtmlList, Source, SourceKind};
use crate::errors::error_chain;
use crate::http::{Fetcher, HttpError};
use crate::source::{self, Candidate, SourceError, html_list};
use crate::text;

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("unknown source id: {0}")]
    UnknownSource(String),
}

/// 1 つのソースの取得失敗。他のソースの確認は続ける。
#[derive(Debug, thiserror::Error)]
pub enum SourceFailure {
    #[error("invalid source url {url:?}")]
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
        let outcome = fetch_source(fetcher, s).await;
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

/// 1 つのソースを取得・解析し、絞り込み条件に一致した候補を返す。
pub async fn fetch_source(fetcher: &Fetcher, s: &Source) -> Result<Stats, SourceFailure> {
    let url = Url::parse(&s.url).map_err(|source| SourceFailure::InvalidUrl {
        url: s.url.clone(),
        source,
    })?;
    let candidates = match (s.kind, &s.list) {
        (SourceKind::HtmlList, Some(list)) => fetch_html_list(fetcher, &url, list).await?,
        _ => {
            let fetched = fetcher.get(&url).await?;
            source::parse(s.kind, &fetched.body, &fetched.url)?
        }
    };
    let total = candidates.len();
    let matched = candidates
        .into_iter()
        .filter(|c| source::matches(&s.filter, c))
        .collect();
    Ok(Stats { total, matched })
}

/// 一覧ページは記事ページと同じく robots.txt に従って取得する。`also` のページは一覧に続けて
/// 同じ読み方で読み、同じ URL の記事は最初の 1 件だけにする。
async fn fetch_html_list(
    fetcher: &Fetcher,
    url: &Url,
    list: &HtmlList,
) -> Result<Vec<Candidate>, SourceFailure> {
    let mut page = fetcher.get_page(url).await?;
    if let Some(follow) = &list.follow {
        let html = text::decode_html(&page.body, page.content_type.as_deref());
        let next = html_list::follow(follow, &html, &page.url)?;
        page = fetcher.get_page(&next).await?;
    }
    let html = text::decode_html(&page.body, page.content_type.as_deref());
    let mut items = html_list::parse(list, &html, &page.url)?;
    for also in &list.also {
        let url = Url::parse(also).map_err(|source| SourceFailure::InvalidUrl {
            url: also.clone(),
            source,
        })?;
        let page = fetcher.get_page(&url).await?;
        let html = text::decode_html(&page.body, page.content_type.as_deref());
        items.extend(html_list::parse(list, &html, &page.url)?);
    }
    let mut seen = HashSet::new();
    items.retain(|c| seen.insert(c.url.clone()));
    Ok(items)
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
                        |d| {
                            d.with_timezone(&crate::jst::offset())
                                .format("%Y-%m-%d")
                                .to_string()
                        },
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
            label: None,
            kind,
            url,
            lang: Lang::En,
            category: Category::Regulator,
            enabled,
            filter,
            body_selector: None,
            list: None,
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

    /// html_list は入口のページから一覧をたどり、robots.txt に従う。
    #[tokio::test]
    async fn html_list_follows_and_respects_robots() {
        let html = |body: &'static str| Route {
            content_type: "text/html; charset=utf-8",
            ..Route::ok(body)
        };
        let server = Server::start(
            [
                (
                    "/robots.txt",
                    Route::ok("User-agent: *\nDisallow: /private/\n"),
                ),
                (
                    "/news/",
                    html(r#"<h3><a href="/press/2026/">プレスリリース</a></h3>"#),
                ),
                (
                    "/press/2026/",
                    html(
                        r#"<dl><dd><a href="pdf/20260925.pdf">原子炉の停止</a></dd>
                           <dd><a href="pdf/20260924.pdf">役員人事</a></dd></dl>"#,
                    ),
                ),
                ("/private/news/", html(r#"<dd><a href="/x.pdf">x</a></dd>"#)),
            ]
            .into(),
        );
        let list = HtmlList {
            link: "dd a".into(),
            date_in_url: Some(crate::config::UrlDate::Yyyymmdd),
            title_skip: None,
            follow: Some("h3 a".into()),
            date: None,
            also: vec![],
        };
        let source = |path: &str| Source {
            list: Some(list.clone()),
            ..src(
                "japc",
                SourceKind::HtmlList,
                server.url(path),
                true,
                Filter {
                    keywords: vec!["原子炉".into()],
                    url_contains: vec![],
                },
            )
        };
        let stats = fetch_source(&fetcher(), &source("/news/")).await.unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.matched.len(), 1);
        assert_eq!(
            stats.matched[0].url,
            server.url("/press/2026/pdf/20260925.pdf")
        );

        let err = fetch_source(&fetcher(), &source("/private/news/"))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                SourceFailure::Http(HttpError::DisallowedByRobots { .. })
            ),
            "{err}"
        );
    }

    /// 規制委の新着履歴は月ごとで、月が替わると前の月の分は載らない。月の初めに新着履歴が
    /// 空でも、同じ読み方で読むトップの新着情報（月をまたいで最新の数件）から前の月の記事を拾う。
    #[tokio::test]
    async fn html_list_also_reads_other_pages_when_the_list_is_empty() {
        let html = |body: Vec<u8>| Route {
            content_type: "text/html; charset=utf-8",
            ..Route::ok(body)
        };
        let server = Server::start(
            [
                ("/news/index.html", html(fixture("nra_news_empty.html"))),
                ("/", html(fixture("nra_top.html"))),
            ]
            .into(),
        );
        let source = Source {
            list: Some(HtmlList {
                link: "dl.news__list dd.news__title a".into(),
                date_in_url: None,
                title_skip: None,
                follow: None,
                date: Some(".news__date".into()),
                also: vec![server.url("/")],
            }),
            ..src(
                "nra",
                SourceKind::HtmlList,
                server.url("/news/index.html"),
                true,
                Filter::default(),
            )
        };
        let stats = fetch_source(&fetcher(), &source).await.unwrap();
        assert_eq!(stats.total, 5);
        assert_eq!(
            stats.matched[1].url,
            server.url("/news_only/20260831_01.html")
        );
        assert_eq!(
            stats.matched[1].published_at,
            crate::jst::midnight(chrono::NaiveDate::from_ymd_opt(2026, 8, 31).unwrap())
        );
    }

    /// 一覧とほかのページの両方に載る記事は、一覧の側の 1 件だけにする。順は一覧、ほかのページの順。
    #[tokio::test]
    async fn html_list_also_pages_come_after_the_list_without_duplicates() {
        let html = |body: &'static str| Route {
            content_type: "text/html; charset=utf-8",
            ..Route::ok(body)
        };
        let server = Server::start(
            [
                (
                    "/news/",
                    html(r#"<dd><a href="/a.html">A</a></dd><dd><a href="/b.html">B</a></dd>"#),
                ),
                (
                    "/",
                    html(r#"<dd><a href="/b.html">B（トップ）</a></dd><dd><a href="/c.html">C</a></dd>"#),
                ),
            ]
            .into(),
        );
        let source = Source {
            list: Some(HtmlList {
                link: "dd a".into(),
                date_in_url: None,
                title_skip: None,
                follow: None,
                date: None,
                also: vec![server.url("/")],
            }),
            ..src(
                "x",
                SourceKind::HtmlList,
                server.url("/news/"),
                true,
                Filter::default(),
            )
        };
        let stats = fetch_source(&fetcher(), &source).await.unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(
            stats
                .matched
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
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

    /// 実サイトの確認。`cargo test -- --ignored jaif` で実行する。
    #[tokio::test]
    #[ignore = "uses the real network"]
    async fn jaif_example_source_fetches_full_text_from_the_real_feed() {
        let sources = crate::config::parse_sources(
            include_str!("../examples/sources.toml"),
            std::path::Path::new("examples/sources.toml"),
        )
        .unwrap();
        let jaif = sources.sources.iter().find(|s| s.id == "jaif").unwrap();
        let fetcher = Fetcher::new(
            "nucrawler-test",
            Duration::from_secs(30),
            Duration::ZERO,
            8 << 20,
        )
        .unwrap();
        let stats = fetch_source(&fetcher, jaif).await.unwrap();
        assert!(!stats.matched.is_empty());
        for c in &stats.matched {
            assert!(
                c.url.starts_with("https://www.jaif.or.jp/journal/"),
                "{}",
                c.url
            );
            assert!(c.published_at.is_some(), "{}", c.url);
            assert!(
                c.content.as_deref().is_some_and(|b| !b.is_empty()),
                "{}",
                c.url
            );
        }
    }

    /// 実サイトの確認。`cargo test -- --ignored nra` で実行する。新着履歴（当月分）に加えて
    /// トップの新着情報（月をまたいで最新の 5 件）を読むので、月の初めでも 0 件にならない。
    #[tokio::test]
    #[ignore = "uses the real network"]
    async fn nra_example_source_reads_the_month_list_and_the_top_page() {
        let sources = crate::config::parse_sources(
            include_str!("../examples/sources.toml"),
            std::path::Path::new("examples/sources.toml"),
        )
        .unwrap();
        let nra = sources.sources.iter().find(|s| s.id == "nra").unwrap();
        let fetcher = Fetcher::new(
            "nucrawler-test",
            Duration::from_secs(30),
            Duration::ZERO,
            8 << 20,
        )
        .unwrap();
        let stats = fetch_source(&fetcher, nra).await.unwrap();
        assert!(stats.total >= 5, "{:#?}", stats.matched);
        let mut urls = std::collections::HashSet::new();
        for c in &stats.matched {
            assert!(c.url.starts_with("https://www.nra.go.jp/"), "{}", c.url);
            assert!(c.published_at.is_some(), "{}", c.url);
            assert!(urls.insert(&c.url), "duplicate {}", c.url);
        }
    }
}
