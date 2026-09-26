//! 抽出ステージ：本文の無い記事のページを取得し、本文のテキストを保存する。
//! 失敗は `stage_errors` に記録し、一時的なものは間隔を空けて再試行、恒久的なものは断念する。

use chrono::{DateTime, Utc};

use super::Cancel;
use crate::config::{PipelineConfig, Source};
use crate::db::{Db, DbError};
use crate::http::Fetcher;

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
    _db: &Db,
    _fetcher: &Fetcher,
    _sources: &[Source],
    _cfg: &PipelineConfig,
    _now: DateTime<Utc>,
    _cancel: &Cancel,
) -> Result<ExtractSummary, ExtractStageError> {
    todo!()
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
            &["/news/1", "/doc.pdf", "/missing", "/down", "/private/x"],
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
                gave_up: 3,
                cancelled: false,
            }
        );
        assert_eq!(bodies(&db), ["/news/1|body|page|Unit 2 returns to se"]);

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
