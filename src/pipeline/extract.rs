//! 抽出ステージ：本文の無い記事のページを取得し、本文のテキストを保存する。
//! 失敗は `stage_errors` に記録し、一時的なものは間隔を空けて再試行、恒久的なものは断念する。

use chrono::{DateTime, Utc};

use url::Url;

use super::Cancel;
use crate::config::{PipelineConfig, Source};
use crate::db::{ContentKind, ContentOrigin, Db, DbError, StageKey};
use crate::extract::{self, ExtractError};
use crate::http::{Fetcher, HttpError};
use crate::{errors, text};

pub const STAGE: &str = "extract";

#[derive(Debug, thiserror::Error)]
pub enum ExtractStageError {
    #[error("database error")]
    Db(#[from] DbError),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExtractSummary {
    /// 本文を保存した記事の数
    pub extracted: usize,
    /// 一時的に失敗し、後で再試行する記事の数
    pub failed: usize,
    /// 恒久的に失敗し、断念した記事の数（robots.txt、404/410、PDF など）
    pub gave_up: usize,
    pub cancelled: bool,
}

/// 抽出待ちの記事を新しい順に最大 `extract_max_per_run` 件処理する。
pub async fn extract_pages(
    db: &Db,
    fetcher: &Fetcher,
    sources: &[Source],
    cfg: &PipelineConfig,
    now: DateTime<Utc>,
    cancel: &Cancel,
) -> Result<ExtractSummary, ExtractStageError> {
    let cutoff = now - chrono::Duration::days(i64::from(cfg.backlog_days));
    let pending = db.pending_extract(cutoff, now, cfg.extract_max_per_run)?;
    let mut summary = ExtractSummary::default();
    for page in pending {
        if cancel.is_requested() {
            summary.cancelled = true;
            break;
        }
        let selector = sources
            .iter()
            .find(|s| s.id == page.source_id)
            .and_then(|s| s.body_selector.as_deref());
        let key = StageKey {
            article_id: page.article_id,
            stage: STAGE,
            backend: "",
            model: "",
        };
        match extract_one(fetcher, &page.url, selector).await {
            Ok(text) => {
                db.insert_content(
                    page.article_id,
                    ContentKind::Body,
                    ContentOrigin::Page,
                    &text,
                )?;
                db.clear_stage_failure(key)?;
                summary.extracted += 1;
            }
            Err(failure) => {
                let message = errors::error_chain(&failure);
                let permanent = failure.is_permanent();
                tracing::warn!(url = %page.url, permanent, "extract failed: {message}");
                db.record_stage_failure(key, &message, now, permanent)?;
                if permanent {
                    summary.gave_up += 1;
                } else {
                    summary.failed += 1;
                }
            }
        }
    }
    Ok(summary)
}

/// 1 つの記事ページの失敗。
#[derive(Debug, thiserror::Error)]
enum PageFailure {
    #[error("invalid article url {url:?}")]
    InvalidUrl {
        url: String,
        source: url::ParseError,
    },
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("PDF is not supported yet ({content_type})")]
    Pdf { content_type: String },
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error("no article text found")]
    NoText,
}

impl PageFailure {
    /// 再試行しても結果が変わらない失敗。
    fn is_permanent(&self) -> bool {
        match self {
            Self::InvalidUrl { .. } | Self::Pdf { .. } => true,
            Self::Http(HttpError::DisallowedByRobots { .. }) => true,
            // 401/403 は多くが bot 対策で、待っても変わらない（回避はしない方針）。
            Self::Http(HttpError::Status { status, .. }) => {
                matches!(status.as_u16(), 401 | 403 | 404 | 410)
            }
            // robots.txt の障害、セレクタの誤り（設定を直せば解決する）などは再試行に回す。
            Self::Http(_) | Self::Extract(_) | Self::NoText => false,
        }
    }
}

async fn extract_one(
    fetcher: &Fetcher,
    url: &str,
    selector: Option<&str>,
) -> Result<String, PageFailure> {
    let parsed = Url::parse(url).map_err(|source| PageFailure::InvalidUrl {
        url: url.to_string(),
        source,
    })?;
    let page = fetcher.get_page(&parsed).await?;
    let content_type = page.content_type.as_deref().unwrap_or_default();
    // Content-Type が当てにならないサーバもあるので、中身の署名でも判定する。
    if content_type.to_ascii_lowercase().contains("pdf") || page.body.starts_with(b"%PDF-") {
        return Err(PageFailure::Pdf {
            content_type: content_type.to_string(),
        });
    }
    let html = text::decode_html(&page.body, page.content_type.as_deref());
    extract::extract_text(&html, page.url.as_str(), selector)?.ok_or(PageFailure::NoText)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{Category, Filter, Lang, SourceKind};
    use crate::db::NewArticle;
    use crate::testutil::{Route, Server, fixture};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-27T00:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn cfg(max: usize) -> PipelineConfig {
        PipelineConfig {
            backlog_days: 14,
            extract_max_per_run: max,
        }
    }

    fn source(id: &str, body_selector: Option<&str>) -> Source {
        Source {
            id: id.into(),
            name: id.into(),
            kind: SourceKind::Feed,
            url: "https://unused.example/rss".into(),
            lang: Lang::En,
            category: Category::Utility,
            enabled: true,
            filter: Filter::default(),
            body_selector: body_selector.map(Into::into),
        }
    }

    fn html(body: Vec<u8>) -> Route {
        Route {
            content_type: "text/html; charset=utf-8",
            ..Route::ok(body)
        }
    }

    fn server() -> Server {
        Server::start(
            [
                (
                    "/robots.txt",
                    Route::ok("User-agent: *\nDisallow: /private/\n"),
                ),
                ("/news/1", html(fixture("article.html"))),
                ("/news/2", html(fixture("article.html"))),
                (
                    "/doc.pdf",
                    Route {
                        content_type: "application/pdf",
                        ..Route::ok(b"%PDF-1.7".to_vec())
                    },
                ),
                ("/down", Route::status(500)),
                ("/blocked", Route::status(403)),
                ("/private/x", html(fixture("article.html"))),
            ]
            .into(),
        )
    }

    fn fetcher() -> Fetcher {
        Fetcher::new("t", Duration::from_secs(2), Duration::ZERO, 1 << 20).unwrap()
    }

    /// 公開日時が新しい順に `paths` の記事を登録する。
    fn add(db: &Db, server: &Server, source_id: &str, paths: &[&str]) -> Vec<i64> {
        paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let published = format!("2026-09-26T{:02}:00:00.000Z", 23 - i);
                db.insert_article(&NewArticle {
                    source_id,
                    url: &server.url(path),
                    title: path,
                    lang: Lang::En,
                    published_at: Some(&published),
                })
                .unwrap()
                .unwrap()
            })
            .collect()
    }

    fn bodies(db: &Db) -> Vec<String> {
        db.query_strings(
            "SELECT a.title || '|' || c.kind || '|' || c.origin || '|' || substr(c.text, 1, 20)
             FROM contents c JOIN articles a ON a.id = c.article_id ORDER BY a.title",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn extracts_bodies_and_classifies_failures() {
        let server = server();
        let db = Db::open_in_memory().unwrap();
        add(
            &db,
            &server,
            "u",
            // PDF、404、403（bot 対策）、robots.txt の禁止は断念し、500 は再試行に回す
            &[
                "/news/1",
                "/doc.pdf",
                "/missing",
                "/down",
                "/blocked",
                "/private/x",
            ],
        );
        let sources = [source("u", None)];
        let summary = extract_pages(
            &db,
            &fetcher(),
            &sources,
            &cfg(100),
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            summary,
            ExtractSummary {
                extracted: 1,
                failed: 1,
                gave_up: 4,
                cancelled: false,
            }
        );
        assert_eq!(bodies(&db), ["/news/1|body|page|Unit 2 of the exampl"]);

        // 同じ時刻に再実行しても、断念したものと再試行待ちのものは処理しない
        let again = extract_pages(
            &db,
            &fetcher(),
            &sources,
            &cfg(100),
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert_eq!(again, ExtractSummary::default());
    }

    /// robots.txt の一時的な障害では断念せず、後で再試行する。
    #[tokio::test]
    async fn robots_outage_is_retried_later() {
        let server = Server::start(
            [
                ("/robots.txt", Route::status(503)),
                ("/news/1", html(fixture("article.html"))),
            ]
            .into(),
        );
        let db = Db::open_in_memory().unwrap();
        add(&db, &server, "u", &["/news/1"]);
        let summary = extract_pages(
            &db,
            &fetcher(),
            &[source("u", None)],
            &cfg(100),
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert_eq!((summary.failed, summary.gave_up), (1, 0));
    }

    /// 一時的な失敗でも、再試行を使い切ったら断念した数に数える。
    #[tokio::test]
    async fn exhausted_retries_count_as_given_up() {
        let server = server();
        let db = Db::open_in_memory().unwrap();
        let ids = add(&db, &server, "u", &["/down"]);
        let key = StageKey {
            article_id: ids[0],
            stage: STAGE,
            backend: "",
            model: "",
        };
        let long_ago = now() - chrono::Duration::days(30);
        for _ in 1..crate::db::MAX_ATTEMPTS {
            db.record_stage_failure(key, "HTTP 500", long_ago, false)
                .unwrap();
        }
        let summary = extract_pages(
            &db,
            &fetcher(),
            &[source("u", None)],
            &cfg(100),
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert_eq!((summary.failed, summary.gave_up), (0, 1));
    }

    /// Content-Type が PDF でなくても、中身が PDF なら断念する。
    #[tokio::test]
    async fn detects_pdf_by_signature() {
        let server = Server::start(
            [(
                "/file",
                Route {
                    content_type: "application/octet-stream",
                    ..Route::ok(b"%PDF-1.7 binary".to_vec())
                },
            )]
            .into(),
        );
        let db = Db::open_in_memory().unwrap();
        add(&db, &server, "u", &["/file"]);
        let summary = extract_pages(
            &db,
            &fetcher(),
            &[source("u", None)],
            &cfg(100),
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert_eq!((summary.failed, summary.gave_up), (0, 1));
    }

    #[tokio::test]
    async fn uses_source_body_selector() {
        let server = server();
        let db = Db::open_in_memory().unwrap();
        add(&db, &server, "sel", &["/news/1"]);
        let sources = [source("sel", Some("aside.related a"))];
        extract_pages(
            &db,
            &fetcher(),
            &sources,
            &cfg(100),
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert_eq!(bodies(&db), ["/news/1|body|page|Related 1\nRelated 2"]);
    }

    #[tokio::test]
    async fn respects_per_run_limit_newest_first() {
        let server = server();
        let db = Db::open_in_memory().unwrap();
        add(&db, &server, "u", &["/news/2", "/news/1"]);
        let summary = extract_pages(
            &db,
            &fetcher(),
            &[source("u", None)],
            &cfg(1),
            now(),
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert_eq!(summary.extracted, 1);
        assert_eq!(bodies(&db).len(), 1);
        assert!(bodies(&db)[0].starts_with("/news/2|"));
    }

    #[tokio::test]
    async fn stops_when_cancelled() {
        let server = server();
        let db = Db::open_in_memory().unwrap();
        add(&db, &server, "u", &["/news/1"]);
        let cancel = Cancel::default();
        cancel.request();
        let summary = extract_pages(
            &db,
            &fetcher(),
            &[source("u", None)],
            &cfg(100),
            now(),
            &cancel,
        )
        .await
        .unwrap();
        assert!(summary.cancelled);
        assert!(bodies(&db).is_empty());
    }
}
