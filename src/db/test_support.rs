//! db のテストで共有する補助。

use super::*;

pub(super) fn article(url: &str) -> NewArticle<'_> {
    NewArticle {
        source_id: "s",
        url,
        title: "t",
        lang: Lang::En,
        published_at: Some("2026-09-27T00:00:00Z"),
    }
}

pub(super) fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().to_utc()
}

pub(super) fn page_article(db: &Db, url: &str, published: &str) -> i64 {
    db.insert_article(&NewArticle {
        published_at: Some(published),
        ..article(url)
    })
    .unwrap()
    .unwrap()
}
