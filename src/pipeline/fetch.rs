//! 取得ステージ：有効なソースを取得・解析・絞り込みし、新しい記事を登録する。
//! フィードに概要や本文があれば、テキストにして本文の部分（contents）として保存する。

use chrono::{DateTime, Utc};

use super::Cancel;
use crate::check;
use crate::config::Source;
use crate::db::{self, ContentKind, ContentOrigin, Db, DbError, FetchCounts, NewArticle};
use crate::errors;
use crate::http::Fetcher;
use crate::source::Candidate;
use crate::text;

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

/// ソース単位の失敗は `source_state` に記録して次のソースへ進む。成功した回は、その件数と一緒に
/// `now` の時刻で記録する。DB のエラーは即座に返す。
pub async fn fetch_sources(
    db: &Db,
    fetcher: &Fetcher,
    sources: &[Source],
    cancel: &Cancel,
    now: DateTime<Utc>,
) -> Result<FetchSummary, FetchError> {
    let mut summary = FetchSummary::default();
    for s in sources.iter().filter(|s| s.enabled) {
        if cancel.is_requested() {
            summary.cancelled = true;
            break;
        }
        match check::fetch_source(fetcher, s).await {
            Ok(stats) => {
                let Stored { new, duplicate } = store(db, s, &stats.matched)?;
                let counts = FetchCounts {
                    total: stats.total,
                    matched: stats.matched.len(),
                    new,
                    duplicate,
                };
                db.record_source_success(&s.id, &counts, now)?;
                tracing::info!(
                    source = %s.id,
                    total = stats.total,
                    matched = counts.matched,
                    new,
                    duplicate,
                    "fetched"
                );
                summary.new_articles += new;
            }
            Err(e) => {
                let message = errors::error_chain(&e);
                tracing::warn!(source = %s.id, "{message}");
                db.record_source_failure(&s.id, &message)?;
                summary.failed_sources.push(s.id.clone());
            }
        }
    }
    Ok(summary)
}

/// `store` で登録した件数と、登録済みだった件数。
struct Stored {
    new: usize,
    duplicate: usize,
}

/// 候補を登録する。URL が不正な候補は記録せずに飛ばす。
fn store(db: &Db, s: &Source, candidates: &[Candidate]) -> Result<Stored, DbError> {
    let mut new = 0;
    let mut duplicate = 0;
    for c in candidates {
        let published_at = c.published_at.map(db::timestamp);
        let lead = c.summary.as_deref().map(text::html_to_text);
        let body = c.content.as_deref().map(text::html_to_text);
        let contents: Vec<_> = [(ContentKind::Lead, lead), (ContentKind::Body, body)]
            .into_iter()
            .filter_map(|(kind, text)| text.filter(|t| !t.is_empty()).map(|t| (kind, t)))
            .collect();
        let contents: Vec<_> = contents
            .iter()
            .map(|(kind, t)| (*kind, ContentOrigin::Feed, t.as_str()))
            .collect();
        let article = NewArticle {
            source_id: &s.id,
            url: &c.url,
            title: &c.title,
            lang: s.lang,
            published_at: published_at.as_deref(),
        };
        match db.insert_article_with_contents(&article, &contents) {
            Ok(Some(_)) => new += 1,
            Ok(None) => duplicate += 1,
            Err(e @ (DbError::InvalidUrl { .. } | DbError::UnsupportedScheme { .. })) => {
                tracing::warn!(source = %s.id, "skipping candidate: {}", errors::error_chain(&e));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Stored { new, duplicate })
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
            label: None,
            kind: SourceKind::Feed,
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
                    title_excludes: vec![],
                },
            ),
            src("blocked", server.url("/blocked"), true, Filter::default()),
            src("utility", server.url("/atom"), true, Filter::default()),
            src("off", server.url("/atom"), false, Filter::default()),
        ];
        (server, sources)
    }

    fn at() -> DateTime<Utc> {
        "2026-09-27T00:00:00Z".parse().unwrap()
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.query_i64(sql).unwrap()
    }

    #[tokio::test]
    async fn stores_new_matching_articles_and_records_failures() {
        let (_server, sources) = setup();
        let db = Db::open_in_memory().unwrap();
        let summary = fetch_sources(&db, &fetcher(), &sources, &Cancel::default(), at())
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
        fetch_sources(&db, &fetcher(), &sources, &Cancel::default(), at())
            .await
            .unwrap();
        let again = fetch_sources(&db, &fetcher(), &sources, &Cancel::default(), at())
            .await
            .unwrap();
        assert_eq!(again.new_articles, 0);
        assert_eq!(count(&db, "SELECT count(*) FROM articles"), 3);
        // 成功したソースは回ごとに件数を残す（reg はリンクのある 2 件中 1 件が絞り込みに一致）。
        // 失敗した回は残さない
        assert_eq!(
            db.query_strings(
                "SELECT source_id || ':' || total || '/' || matched || ' new ' || new
                        || ' dup ' || duplicate
                 FROM fetch_runs ORDER BY id"
            )
            .unwrap(),
            [
                "reg:2/1 new 1 dup 0",
                "utility:2/2 new 2 dup 0",
                "reg:2/1 new 0 dup 1",
                "utility:2/2 new 0 dup 2",
            ]
        );
    }

    #[tokio::test]
    async fn stores_feed_summary_and_content_as_text() {
        let (_server, sources) = setup();
        let db = Db::open_in_memory().unwrap();
        fetch_sources(&db, &fetcher(), &sources[..1], &Cancel::default(), at())
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
        let summary = fetch_sources(&db, &fetcher(), &sources, &cancel, at())
            .await
            .unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.new_articles, 0);
        assert_eq!(count(&db, "SELECT count(*) FROM articles"), 0);
    }
}
