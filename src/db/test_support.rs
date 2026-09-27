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

pub(super) fn insert_content(db: &Db, article_id: i64, membership: Option<i64>) -> i64 {
    db.conn()
        .execute(
            "INSERT INTO contents (article_id, kind, access_membership_id, text, origin, fetched_at)
             VALUES (?1, 'body', ?2, 'x', 'page', '2026-09-27T00:00:00Z')",
            rusqlite::params![article_id, membership],
        )
        .unwrap();
    db.conn().last_insert_rowid()
}

pub(super) fn access_of(db: &Db, artifact_id: i64) -> Vec<i64> {
    let mut stmt = db
        .conn()
        .prepare("SELECT membership_id FROM artifact_access WHERE artifact_id = ?1 ORDER BY 1")
        .unwrap();
    stmt.query_map([artifact_id], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
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

pub(super) fn digest_with_topics(
    db: &Db,
    topics: serde_json::Value,
    new: serde_json::Value,
) -> Result<i64, DbError> {
    let a = db
        .insert_article(&article(&format!(
            "https://e.com/{}",
            db.query_i64("SELECT count(*) FROM articles").unwrap()
        )))
        .unwrap()
        .unwrap();
    let c = db
        .insert_content(a, ContentKind::Body, ContentOrigin::Page, "body")
        .unwrap();
    let payload = serde_json::json!({"title_ja": "題", "summary_ja": "s", "topics": topics, "new_topics": new});
    db.insert_artifact(
        &NewArtifact {
            article_id: a,
            kind: ArtifactKind::Digest,
            backend: "claude-cli",
            model: "sonnet",
            prompt_version: 2,
            payload: &payload,
            inputs: &[c],
            glossary_at: None,
        },
        t("2026-09-27T00:00:00Z"),
    )
}

pub(super) fn linked_topics(db: &Db, artifact_id: i64) -> Vec<String> {
    db.query_strings(&format!(
        "SELECT t.name FROM artifact_topics AS at JOIN topics AS t ON t.id = at.topic_id
         WHERE at.artifact_id = {artifact_id} ORDER BY t.id"
    ))
    .unwrap()
}
