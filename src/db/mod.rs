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
    #[error("invalid database schema version {0}")]
    InvalidSchemaVersion(i64),
    #[error("database schema version {found} is newer than this binary supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
}

/// 適用順に並べたマイグレーション。`PRAGMA user_version` は適用済みの件数。
/// 既存の要素は書き換えず、変更は新しい要素の追加で行う。
const MIGRATIONS: &[&str] = &[include_str!("migrations/0001_init.sql")];

const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        // WAL への切り替え自体がロック待ちになり得るので、先に busy_timeout を設定する。
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, DbError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self, DbError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64, DbError> {
        schema_version(&self.conn)
    }

    pub fn owner_id(&self) -> Result<i64, DbError> {
        Ok(self
            .conn
            .query_row("SELECT id FROM users WHERE is_owner = 1", [], |r| r.get(0))?)
    }

    /// URL を正規化して登録する。既に同じ URL があれば `None`。
    pub fn insert_article(&self, a: &NewArticle) -> Result<Option<i64>, DbError> {
        let url = normalize_url(a.url)?;
        let lang = match a.lang {
            Lang::En => "en",
            Lang::Ja => "ja",
        };
        let inserted = self.conn.execute(
            "INSERT INTO articles (source_id, url, title, lang, published_at, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ON CONFLICT (url) DO NOTHING",
            rusqlite::params![a.source_id, url, a.title, lang, a.published_at],
        )?;
        Ok((inserted > 0).then(|| self.conn.last_insert_rowid()))
    }

    #[cfg(test)]
    fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn schema_version(conn: &Connection) -> Result<i64, DbError> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    let found = schema_version(conn)?;
    let supported = MIGRATIONS.len() as i64;
    if found < 0 {
        return Err(DbError::InvalidSchemaVersion(found));
    }
    if found > supported {
        return Err(DbError::SchemaTooNew { found, supported });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

/// 重複判定用に URL を正規化する：fragment と追跡用のクエリ（utm_*、fbclid、gclid）を除く。
pub fn normalize_url(url: &str) -> Result<String, DbError> {
    let mut u = url::Url::parse(url).map_err(|source| DbError::InvalidUrl {
        url: url.to_string(),
        source,
    })?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(DbError::UnsupportedScheme {
            url: url.to_string(),
            scheme: u.scheme().to_string(),
        });
    }
    u.set_fragment(None);
    // 追跡用パラメータがあるときだけクエリを組み直し、それ以外の元の表記は保つ。
    if u.query_pairs().any(|(k, _)| is_tracking_param(&k)) {
        let kept: Vec<(String, String)> = u
            .query_pairs()
            .filter(|(k, _)| !is_tracking_param(k))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if kept.is_empty() {
            u.set_query(None);
        } else {
            u.query_pairs_mut().clear().extend_pairs(kept);
        }
    }
    Ok(u.into())
}

fn is_tracking_param(key: &str) -> bool {
    key.starts_with("utm_") || matches!(key, "fbclid" | "gclid")
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
    fn rejects_negative_schema_version() {
        let mut db = Db::open_in_memory().unwrap();
        db.conn.pragma_update(None, "user_version", -1).unwrap();
        let err = migrate(&mut db.conn).unwrap_err();
        assert!(matches!(err, DbError::InvalidSchemaVersion(-1)), "{err}");
    }

    fn insert_membership(db: &Db) -> i64 {
        db.conn()
            .execute("INSERT INTO memberships (code, name) VALUES ('m', 'M')", [])
            .unwrap();
        db.conn().last_insert_rowid()
    }

    fn insert_artifact(db: &Db, article_id: i64) -> i64 {
        db.conn()
            .execute(
                "INSERT INTO artifacts
                   (article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
                 VALUES (?1, 'digest', 'b', 'm', 1, 'm', '{}', '2026-09-27T00:00:00Z')",
                [article_id],
            )
            .unwrap();
        db.conn().last_insert_rowid()
    }

    #[test]
    fn deleting_membership_removes_claims_and_article_markers() {
        let db = Db::open_in_memory().unwrap();
        let m = insert_membership(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let owner = db.owner_id().unwrap();
        db.conn()
            .execute("INSERT INTO user_memberships VALUES (?1, ?2)", [owner, m])
            .unwrap();
        db.conn()
            .execute("INSERT INTO article_access VALUES (?1, ?2)", [a, m])
            .unwrap();
        db.conn()
            .execute("DELETE FROM memberships WHERE id = ?1", [m])
            .unwrap();
        let n: i64 = db
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM user_memberships)
                      + (SELECT count(*) FROM article_access)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    /// 会員資格を消しても、会員限定の成果物が公開扱いにならないこと。
    #[test]
    fn deleting_membership_used_by_gated_artifact_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        let m = insert_membership(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let art = insert_artifact(&db, a);
        db.conn()
            .execute("INSERT INTO artifact_access VALUES (?1, ?2)", [art, m])
            .unwrap();
        let err = db
            .conn()
            .execute("DELETE FROM memberships WHERE id = ?1", [m])
            .unwrap_err();
        assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
    }

    /// 記事を消せば、本文・成果物・出所の記録がまとめて消えること。
    #[test]
    fn deleting_article_cascades_through_artifact_inputs() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO contents (article_id, kind, text, origin, fetched_at)
                 VALUES (?1, 'body', 'x', 'page', '2026-09-27T00:00:00Z')",
                [a],
            )
            .unwrap();
        let c = db.conn().last_insert_rowid();
        let art = insert_artifact(&db, a);
        db.conn()
            .execute("INSERT INTO artifact_inputs VALUES (?1, ?2)", [art, c])
            .unwrap();
        db.conn()
            .execute("DELETE FROM articles WHERE id = ?1", [a])
            .unwrap();
        let n: i64 = db
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM contents) + (SELECT count(*) FROM artifacts)
                      + (SELECT count(*) FROM artifact_inputs)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
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
