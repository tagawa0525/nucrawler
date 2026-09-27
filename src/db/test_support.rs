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

pub(super) fn add_digest(
    db: &Db,
    article_id: i64,
    model: &str,
    title: &str,
    relevant: bool,
    at: &str,
) -> i64 {
    let c = db
        .insert_content(article_id, ContentKind::Body, ContentOrigin::Page, "body")
        .unwrap();
    let payload = serde_json::json!({
        "title_ja": title, "summary_ja": format!("{title}の要約"), "points_ja": ["点"],
        "implications_ja": "", "lwr_relevant": relevant, "topics": ["規制・審査"],
    });
    db.insert_artifact(
        &NewArtifact {
            article_id,
            kind: ArtifactKind::Digest,
            backend: "claude-cli",
            model,
            prompt_version: 1,
            payload: &payload,
            inputs: &[c],
            glossary_at: None,
        },
        t(at),
    )
    .unwrap()
}

pub(super) fn score_key(db: &Db) -> ScoreKey<'static> {
    ScoreKey {
        user_id: db.owner_id().unwrap(),
        profile_hash: "h1",
        backend: "claude-cli",
        model: "sonnet",
    }
}

/// 英語の記事に本文と digest と採点を付ける。
pub(super) fn scored_article(db: &Db, url: &str, lang: Lang, published: &str, score: u8) -> i64 {
    let id = db
        .insert_article(&NewArticle {
            lang,
            published_at: Some(published),
            ..article(url)
        })
        .unwrap()
        .unwrap();
    let digest = add_digest(db, id, "sonnet", "題", true, "2026-09-26T01:00:00Z");
    db.insert_score(
        ScoreKey {
            user_id: db.owner_id().unwrap(),
            profile_hash: "h1",
            backend: "claude-cli",
            model: "sonnet",
        },
        digest,
        score,
        None,
        t("2026-09-26T02:00:00Z"),
    )
    .unwrap();
    id
}

pub(super) fn list_query(db: &Db, show_all: bool) -> ListQuery<'static> {
    ListQuery {
        user_id: db.owner_id().unwrap(),
        profile_hash: Some("h1"),
        min_score: 60,
        since: t("2026-09-20T00:00:00Z"),
        show_all,
        limit: 50,
    }
}

pub(super) fn list_ids(db: &Db, show_all: bool) -> Vec<i64> {
    db.list_articles(list_query(db, show_all))
        .unwrap()
        .into_iter()
        .map(|i| i.article_id)
        .collect()
}

pub(super) fn search_query(db: &Db) -> SearchQuery<'static> {
    SearchQuery {
        user_id: db.owner_id().unwrap(),
        profile_hash: Some("h1"),
        limit: 50,
        ..SearchQuery::default()
    }
}

pub(super) fn found(db: &Db, q: SearchQuery) -> Vec<i64> {
    db.search_articles(&q)
        .unwrap()
        .into_iter()
        .map(|i| i.article_id)
        .collect()
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
