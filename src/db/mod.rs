use std::path::Path;

use rusqlite::Connection;

use crate::config::Lang;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid url {url:?}: {source}")]
    InvalidUrl {
        url: String,
        source: url::ParseError,
    },
    #[error("unsupported url scheme {scheme:?} in {url:?}")]
    UnsupportedScheme { url: String, scheme: String },
    #[error("database schema version {found} is newer than this binary supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
}

/// 適用順に並べたマイグレーション。`PRAGMA user_version` は適用済みの件数。
/// 既存の要素は書き換えず、変更は新しい要素の追加で行う。
const MIGRATIONS: &[&str] = &[include_str!("migrations/0001_init.sql")];

pub struct Db {
    conn: Connection,
}

pub struct NewArticle<'a> {
    pub source_id: &'a str,
    pub url: &'a str,
    pub title: &'a str,
    pub lang: Lang,
    /// RFC 3339
    pub published_at: Option<&'a str>,
}

impl Db {
    pub fn open(_path: &Path) -> Result<Self, DbError> {
        todo!()
    }

    pub fn open_in_memory() -> Result<Self, DbError> {
        todo!()
    }

    pub fn schema_version(&self) -> Result<i64, DbError> {
        todo!()
    }

    pub fn owner_id(&self) -> Result<i64, DbError> {
        todo!()
    }

    /// URL を正規化して登録する。既に同じ URL があれば `None`。
    pub fn insert_article(&self, _a: &NewArticle) -> Result<Option<i64>, DbError> {
        todo!()
    }

    #[cfg(test)]
    fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn migrate(_conn: &mut Connection) -> Result<(), DbError> {
    todo!()
}

/// 重複判定用に URL を正規化する：fragment と追跡用のクエリ（utm_*、fbclid、gclid）を除く。
pub fn normalize_url(_url: &str) -> Result<String, DbError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(url: &str) -> NewArticle<'_> {
        NewArticle {
            source_id: "s",
            url,
            title: "t",
            lang: Lang::En,
            published_at: Some("2026-09-27T00:00:00Z"),
        }
    }

    #[test]
    fn migrates_to_latest_version() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut db = Db::open_in_memory().unwrap();
        migrate(&mut db.conn).unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }

    #[test]
    fn rejects_schema_newer_than_binary() {
        let mut db = Db::open_in_memory().unwrap();
        db.conn.pragma_update(None, "user_version", 999).unwrap();
        let err = migrate(&mut db.conn).unwrap_err();
        assert!(
            matches!(err, DbError::SchemaTooNew { found: 999, .. }),
            "{err}"
        );
    }

    #[test]
    fn open_file_persists_schema() {
        let dir = std::env::temp_dir().join(format!("nucrawler-{}-db", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("n.db");
        let id = {
            let db = Db::open(&path).unwrap();
            db.insert_article(&article("https://example.com/a"))
                .unwrap()
        };
        let db = Db::open(&path).unwrap();
        assert!(id.is_some());
        assert_eq!(
            db.insert_article(&article("https://example.com/a"))
                .unwrap(),
            None
        );
    }

    #[test]
    fn seeds_owner_and_aesj() {
        let db = Db::open_in_memory().unwrap();
        db.owner_id().unwrap();
        let name: String = db
            .conn()
            .query_row(
                "SELECT name FROM memberships WHERE code = 'aesj'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "日本原子力学会");
    }

    #[test]
    fn insert_article_dedupes_by_normalized_url() {
        let db = Db::open_in_memory().unwrap();
        let first = db
            .insert_article(&article("https://example.com/a?id=1"))
            .unwrap();
        assert!(first.is_some());
        let again = db
            .insert_article(&article("https://EXAMPLE.com/a?id=1&utm_source=rss#top"))
            .unwrap();
        assert_eq!(again, None);
        let other = db
            .insert_article(&article("https://example.com/a?id=2"))
            .unwrap();
        assert!(other.is_some());
    }

    #[test]
    fn insert_article_rejects_invalid_url() {
        let db = Db::open_in_memory().unwrap();
        let err = db.insert_article(&article("not a url")).unwrap_err();
        assert!(matches!(err, DbError::InvalidUrl { .. }), "{err}");
    }

    #[test]
    fn normalize_url_cases() {
        let cases = [
            ("https://Example.COM/a#frag", "https://example.com/a"),
            (
                "https://e.com/a?utm_source=x&id=3&utm_medium=y&fbclid=z",
                "https://e.com/a?id=3",
            ),
            ("https://e.com/a?utm_source=x", "https://e.com/a"),
            ("https://e.com/a?gclid=1&b=2&a=1", "https://e.com/a?b=2&a=1"),
            ("http://e.com/", "http://e.com/"),
        ];
        for (input, want) in cases {
            assert_eq!(normalize_url(input).unwrap(), want, "{input}");
        }
    }

    #[test]
    fn normalize_url_rejects_non_http() {
        let err = normalize_url("ftp://e.com/a").unwrap_err();
        assert!(matches!(err, DbError::UnsupportedScheme { .. }), "{err}");
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .conn()
            .execute(
                "INSERT INTO contents (article_id, kind, text, origin, fetched_at)
                 VALUES (999, 'body', 'x', 'page', '2026-09-27T00:00:00Z')",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
    }

    #[test]
    fn artifact_exposes_generated_columns() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO artifacts
                   (article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
                 VALUES (?1, 'digest', 'claude-cli', 'sonnet', 1, 'public',
                         '{\"title_ja\":\"題\",\"summary_ja\":\"要約\"}', '2026-09-27T00:00:00Z')",
                [id],
            )
            .unwrap();
        let (t, s): (String, String) = db
            .conn()
            .query_row("SELECT title_ja, summary_ja FROM artifacts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((t.as_str(), s.as_str()), ("題", "要約"));
    }

    #[test]
    fn only_one_owner_allowed() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .conn()
            .execute(
                "INSERT INTO users (login, display_name, is_owner) VALUES ('x', 'x', 1)",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("UNIQUE"), "{err}");
    }
}
