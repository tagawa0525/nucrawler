//! 取得ステージ：有効なソースを取得・解析・絞り込みし、新しい記事を登録する。
//! フィードに概要や本文があれば、テキストにして本文の部分（contents）として保存する。

use super::Cancel;
use crate::config::Source;
use crate::db::{Db, DbError};
use crate::http::Fetcher;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("database error")]
    Db(#[from] DbError),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FetchSummary {
    /// 新しく登録した記事の数
    pub new_articles: usize,
    /// 取得・解析に失敗したソースの id
    pub failed_sources: Vec<String>,
    /// 中断要求で止まったか
    pub cancelled: bool,
}

/// ソース単位の失敗は `source_state` に記録して次のソースへ進む。DB のエラーは即座に返す。
pub async fn fetch_sources(
    _db: &Db,
    _fetcher: &Fetcher,
    _sources: &[Source],
    _cancel: &Cancel,
) -> Result<FetchSummary, FetchError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{Category, Filter, Lang, SourceKind};
    use crate::testutil::{Route, Server, fixture};

    fn src(id: &str, url: String, enabled: bool, filter: Filter) -> Source {
        Source {
            id: id.into(),
            name: id.into(),
            kind: SourceKind::Feed,
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
                ("/atom", Route::ok(fixture("atom.xml"))),
                ("/blocked", Route::status(403)),
            ]
            .into(),
        );
        let sources = vec![
            src(
                "reg",
                server.url("/rss"),
                true,
                Filter {
                    keywords: vec!["Power Reactor".into()],
                    url_contains: vec![],
                },
            ),
            src("blocked", server.url("/blocked"), true, Filter::default()),
            src("utility", server.url("/atom"), true, Filter::default()),
            src("off", server.url("/atom"), false, Filter::default()),
        ];
        (server, sources)
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.query_i64(sql).unwrap()
    }

    #[tokio::test]
    async fn stores_new_matching_articles_and_records_failures() {
        let (_server, sources) = setup();
        let db = Db::open_in_memory().unwrap();
        let summary = fetch_sources(&db, &fetcher(), &sources, &Cancel::default())
            .await
            .unwrap();
        // reg は絞り込みで 1 件、utility は 2 件。blocked は失敗し、off は無効
        assert_eq!(
            summary,
            FetchSummary {
                new_articles: 3,
                failed_sources: vec!["blocked".into()],
                cancelled: false,
            }
        );
        assert_eq!(count(&db, "SELECT count(*) FROM articles"), 3);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM articles WHERE source_id = 'reg' AND title LIKE 'Power Reactor%'"
            ),
            1
        );

        let blocked = db.source_state("blocked").unwrap().unwrap();
        assert!(blocked.last_error.as_deref().unwrap().contains("403"));
        assert!(blocked.last_success_at.is_none());
        let reg = db.source_state("reg").unwrap().unwrap();
        assert!(reg.last_success_at.is_some());
        assert!(reg.last_error.is_none());
        assert!(db.source_state("off").unwrap().is_none());
    }

    #[tokio::test]
    async fn rerun_adds_nothing_new() {
        let (_server, sources) = setup();
        let db = Db::open_in_memory().unwrap();
        fetch_sources(&db, &fetcher(), &sources, &Cancel::default())
            .await
            .unwrap();
        let again = fetch_sources(&db, &fetcher(), &sources, &Cancel::default())
            .await
            .unwrap();
        assert_eq!(again.new_articles, 0);
        assert_eq!(count(&db, "SELECT count(*) FROM articles"), 3);
    }

    #[tokio::test]
    async fn stores_feed_summary_and_content_as_text() {
        let (_server, sources) = setup();
        let db = Db::open_in_memory().unwrap();
        fetch_sources(&db, &fetcher(), &sources[..1], &Cancel::default())
            .await
            .unwrap();
        let rows = db
            .query_strings(
                "SELECT kind || '|' || origin || '|' || coalesce(access_membership_id, 'public')
                        || '|' || text
                 FROM contents ORDER BY kind",
            )
            .unwrap();
        assert_eq!(
            rows,
            [
                "body|feed|public|Full text of the event.",
                "lead|feed|public|Power Reactor event notification for Unit 2.",
            ]
        );
    }

    #[tokio::test]
    async fn stops_between_sources_when_cancelled() {
        let (_server, sources) = setup();
        let db = Db::open_in_memory().unwrap();
        let cancel = Cancel::default();
        cancel.request();
        let summary = fetch_sources(&db, &fetcher(), &sources, &cancel)
            .await
            .unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.new_articles, 0);
        assert_eq!(count(&db, "SELECT count(*) FROM articles"), 0);
    }
}
