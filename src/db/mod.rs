use std::path::Path;

use rusqlite::Connection;

use crate::config::Lang;

mod articles;
mod artifacts;
mod feedback;
mod notes;
mod read;
mod redo;
mod score;
mod sources;
mod stages;
#[cfg(test)]
mod test_support;
mod translate;
mod vocab;
mod warnings;

pub use articles::*;
pub use artifacts::*;
pub use feedback::*;
pub use notes::*;
pub use read::*;
pub use redo::*;
pub use score::*;
pub use sources::*;
pub use stages::*;
pub use translate::*;
pub use vocab::*;
pub use warnings::*;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("failed to encode json")]
    Json(#[from] serde_json::Error),
    #[error("invalid url {url:?}")]
    InvalidUrl {
        url: String,
        source: url::ParseError,
    },
    #[error("unsupported url scheme {scheme:?} in {url:?}")]
    UnsupportedScheme { url: String, scheme: String },
    #[error("invalid database schema version {0}")]
    InvalidSchemaVersion(i64),
    #[error("database schema version {found} is newer than this binary supports ({supported})")]
    SchemaTooNew { found: i64, supported: usize },
    /// CHECK 制約で防いでいるはずの値が入っていた
    #[error("unexpected value in the database: {0}")]
    UnexpectedValue(String),
    /// 入力の無い成果物は閲覧資格を導出できず、公開扱いになってしまうので登録しない。
    #[error("artifact for article {article_id} has no input contents")]
    NoArtifactInputs { article_id: i64 },
    /// 要約に付いているトピックは語彙から消せない
    #[error("topics in use cannot be removed: {}", .0.join(", "))]
    TopicsInUse(Vec<String>),
    /// 要約のトピックが語彙に無く、新しい語として提案もされていない
    #[error("unknown topic {0:?}")]
    UnknownTopic(String),
    /// 訳語集の訳語・略語・原語が、ほかの訳語のものと重なった
    #[error("{0}")]
    GlossaryConflict(String),
    /// マイグレーションの後に外部キーの違反が残った（その件数）
    #[error("migration left {0} foreign key violations")]
    ForeignKeyViolation(i64),
    /// 語を自分自身に統合しようとした
    #[error("cannot merge topic {0:?} into itself")]
    SelfMerge(String),
}

/// 適用順に並べたマイグレーション。`PRAGMA user_version` は適用済みの件数。
/// 既存の要素は書き換えず、変更は新しい要素の追加で行う。
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_init.sql"),
    include_str!("migrations/0002_membership_code_check.sql"),
    include_str!("migrations/0003_last_seen.sql"),
    include_str!("migrations/0004_visit_boundary.sql"),
    include_str!("migrations/0005_retry_pdf_extracts.sql"),
    include_str!("migrations/0006_search.sql"),
    include_str!("migrations/0007_topics.sql"),
    include_str!("migrations/0008_topic_proposals.sql"),
    include_str!("migrations/0009_topic_aliases.sql"),
    include_str!("migrations/0010_bookmarks.sql"),
    include_str!("migrations/0011_glossary.sql"),
    include_str!("migrations/0012_term_reports.sql"),
    include_str!("migrations/0013_glossary_changes.sql"),
    include_str!("migrations/0014_report_status.sql"),
    include_str!("migrations/0015_report_kinds.sql"),
    include_str!("migrations/0016_comments.sql"),
    include_str!("migrations/0017_artifact_glossary.sql"),
];

/// 現在時刻（UTC、RFC 3339、ミリ秒まで）を返す SQL 式。
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct Db {
    conn: Connection,
}

/// DB に書く時刻の書式。SQL の `NOW` と同じく UTC・ミリ秒・'Z' に揃え、文字列の大小で比較できるようにする。
pub fn timestamp(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        enable_wal(&conn)?;
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

    #[cfg(test)]
    pub(crate) fn query_i64(&self, sql: &str) -> Result<i64, DbError> {
        Ok(self.conn.query_row(sql, [], |r| r.get(0))?)
    }

    #[cfg(test)]
    pub(crate) fn query_strings(&self, sql: &str) -> Result<Vec<String>, DbError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// WAL への切り替えは排他ロックが必要で、競合すると busy_timeout を待たずに
/// SQLITE_BUSY を返す。WAL は DB ファイルに永続するので競合は新規作成直後だけだが、
/// 同時に開かれても失敗しないよう BUSY_TIMEOUT まで再試行する。
fn enable_wal(conn: &Connection) -> Result<(), DbError> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return Ok(result?),
        }
    }
}

fn schema_version(conn: &Connection) -> Result<i64, DbError> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// 同時に開いたプロセス同士で二重に適用しないよう、IMMEDIATE トランザクションで
/// 書き込みロックを取ってから版を読み、未適用分をまとめて適用する。
fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    migrate_with(conn, MIGRATIONS)
}

/// `migrations` のうち未適用のものを 1 つのトランザクションで適用する。テーブルを作り直すときに
/// 参照している側の行が連鎖して消えないよう、適用の間は外部キーを止め、最後に違反が無いことを
/// 確かめてから確定する（SQLite の推奨する手順）。外部キーは失敗しても有効に戻す。
fn migrate_with(conn: &mut Connection, migrations: &[&str]) -> Result<(), DbError> {
    // トランザクションの中では切り替えられないので、その外で止める
    conn.pragma_update(None, "foreign_keys", false)?;
    let applied = apply_migrations(conn, migrations);
    conn.pragma_update(None, "foreign_keys", true)?;
    applied
}

fn apply_migrations(conn: &mut Connection, migrations: &[&str]) -> Result<(), DbError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let found = schema_version(&tx)?;
    let applied = usize::try_from(found).map_err(|_| DbError::InvalidSchemaVersion(found))?;
    let pending = migrations.get(applied..).ok_or(DbError::SchemaTooNew {
        found,
        supported: migrations.len(),
    })?;
    let mut version = found;
    for sql in pending {
        tx.execute_batch(sql)?;
        version += 1;
    }
    let violations: i64 =
        tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })?;
    if violations > 0 {
        return Err(DbError::ForeignKeyViolation(violations));
    }
    tx.pragma_update(None, "user_version", version)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::linked_topics;
    use crate::db::test_support::*;

    #[test]
    fn migrates_to_latest_version() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }

    /// PDF に対応する前に「未対応」で断念した抽出は、対応後に再試行する。
    #[test]
    fn migration_retries_extracts_given_up_on_pdf() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        for sql in &MIGRATIONS[..4] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn.execute_batch(
            "INSERT INTO articles (id, source_id, url, title, lang, fetched_at)
               VALUES (1, 's', 'https://e.example/a.pdf', 't', 'ja', '2026-09-27T00:00:00.000Z'),
                      (2, 's', 'https://e.example/b', 't', 'ja', '2026-09-27T00:00:00.000Z');
             INSERT INTO stage_errors VALUES
               (1, 'extract', '', '', 5, 'PDF is not supported yet (application/pdf)', '9999'),
               (2, 'extract', '', '', 5, 'no article text found', '9999');",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let left: Vec<i64> = conn
            .prepare("SELECT article_id FROM stage_errors")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(left, [2]);
    }

    /// 外部キーの違反を残すマイグレーションは適用せず、外部キーは有効に戻る。
    #[test]
    fn migration_leaving_foreign_key_violations_is_not_applied() {
        let mut db = Db::open_in_memory().unwrap();
        let mut migrations = MIGRATIONS.to_vec();
        migrations.push("INSERT INTO artifact_topics (artifact_id, topic_id) VALUES (999, 1);");
        let err = migrate_with(&mut db.conn, &migrations).unwrap_err();
        assert!(matches!(err, DbError::ForeignKeyViolation(1)), "{err}");
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
        assert_eq!(
            db.query_i64("SELECT count(*) FROM artifact_topics")
                .unwrap(),
            0
        );
        let on: bool = db
            .conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert!(on);
    }

    /// 成果物を作り直しても、行・参照している側の行・全文検索を保つ。
    /// 作り直した後は、訳語集の時点が違えば同じモデル・プロンプト版でも別の版として残せる。
    #[test]
    fn migration_rebuilds_artifacts_keeping_references() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let before = MIGRATIONS
            .iter()
            .position(|m| m.contains("glossary_at"))
            .unwrap();
        for sql in &MIGRATIONS[..before] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", before as i64)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO articles (id, source_id, url, title, lang, fetched_at)
               VALUES (1, 's', 'https://e.example/a', 't', 'en', '2026-09-27T00:00:00.000Z');
             INSERT INTO contents (id, article_id, kind, text, origin, fetched_at)
               VALUES (1, 1, 'body', 'x', 'page', '2026-09-27T00:00:00.000Z');
             INSERT INTO artifacts
               (id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
               VALUES (1, 1, 'digest', 'b', 'm', 1, 'public',
                       '{\"title_ja\": \"題\", \"summary_ja\": \"要約\"}', '2026-09-27T00:00:00.000Z');
             INSERT INTO artifact_inputs VALUES (1, 1, 1);
             INSERT INTO artifact_topics (artifact_id, topic_id) VALUES (1, 1);
             INSERT INTO scores (user_id, artifact_id, profile_hash, backend, model, score, created_at)
               VALUES (1, 1, 'h', 'b', 'm', 50, '2026-09-27T00:00:00.000Z');",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        let count = |sql: &str| db.query_i64(sql).unwrap();
        let children = ["artifact_inputs", "artifact_topics", "scores"];
        for table in children {
            assert_eq!(
                count(&format!("SELECT count(*) FROM {table}")),
                1,
                "{table}"
            );
        }
        assert_eq!(
            count(
                "SELECT count(*) FROM artifacts WHERE id = 1 AND glossary_at IS NULL AND title_ja = '題'"
            ),
            1
        );
        assert_eq!(
            count("SELECT count(*) FROM search_docs WHERE artifact_id = 1"),
            1
        );

        let insert = |glossary_at: &str| {
            db.conn().execute(
                &format!(
                    "INSERT INTO artifacts
                       (article_id, kind, backend, model, prompt_version, input_scope, payload,
                        created_at, glossary_at)
                     VALUES (1, 'digest', 'b', 'm', 1, 'public', '{{}}', '2026-09-28T00:00:00.000Z',
                             {glossary_at})"
                ),
                [],
            )
        };
        insert("'2026-09-27T12:00:00.000Z'").unwrap();
        assert!(insert("NULL").is_err());
        assert!(insert("'2026-09-27T12:00:00.000Z'").is_err());
        assert_eq!(
            count("SELECT count(*) FROM search_docs WHERE artifact_id IS NOT NULL"),
            2
        );

        // 参照している側は引き続き連鎖して消える
        db.conn()
            .execute("DELETE FROM artifacts WHERE id = 1", [])
            .unwrap();
        for table in children {
            assert_eq!(
                count(&format!("SELECT count(*) FROM {table}")),
                0,
                "{table}"
            );
        }
        assert_eq!(
            count("SELECT count(*) FROM search_docs WHERE artifact_id = 1"),
            0
        );
    }

    #[test]
    fn migration_adds_prompt_version_to_scores() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let before = MIGRATIONS
            .iter()
            .position(|m| m.contains("scores_new"))
            .unwrap();
        for sql in &MIGRATIONS[..before] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", before as i64)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO articles (id, source_id, url, title, lang, fetched_at)
               VALUES (1, 's', 'https://e.example/a', 't', 'en', '2026-09-27T00:00:00.000Z');
             INSERT INTO artifacts
               (id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
               VALUES (1, 1, 'digest', 'b', 'm', 1, 'public', '{}', '2026-09-27T00:00:00.000Z');
             INSERT INTO scores
               (id, user_id, artifact_id, profile_hash, backend, model, score, reason, created_at)
               VALUES (7, 1, 1, 'h', 'b', 'm', 50, '理由', '2026-09-27T00:00:00.000Z');",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        assert_eq!(
            db.query_strings(
                "SELECT id || '|' || prompt_version || '|' || score || '|' || reason FROM scores"
            )
            .unwrap(),
            ["7|1|50|理由"]
        );
        let insert = |version: i64| {
            db.conn().execute(
                "INSERT INTO scores
                   (user_id, artifact_id, profile_hash, backend, model, prompt_version, score,
                    created_at)
                 VALUES (1, 1, 'h', 'b', 'm', ?1, 60, '2026-09-28T00:00:00.000Z')",
                [version],
            )
        };
        insert(2).unwrap();
        assert!(insert(2).is_err());
        assert_eq!(
            db.query_i64(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'scores_by_artifact'"
            )
            .unwrap(),
            1
        );
        // 記事を消せば採点も連鎖して消える
        db.conn()
            .execute("DELETE FROM artifacts WHERE id = 1", [])
            .unwrap();
        assert_eq!(db.query_i64("SELECT count(*) FROM scores").unwrap(), 0);
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

    /// 別の記事の本文を成果物の入力にできないこと。
    #[test]
    fn artifact_input_must_belong_to_same_article() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let b = db
            .insert_article(&article("https://e.com/b"))
            .unwrap()
            .unwrap();
        let content_of_b = insert_content(&db, b, None);
        let art = insert_artifact(&db, a, "public");
        let err = link_input(&db, art, content_of_b).unwrap_err();
        assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
    }

    /// 閲覧に必要な資格は、入力に使った本文の資格から必ず導出されること。
    #[test]
    fn artifact_access_is_derived_from_inputs() {
        let db = Db::open_in_memory().unwrap();
        let m = insert_membership(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let public = insert_content(&db, a, None);
        let gated = insert_content(&db, a, Some(m));

        let public_only = insert_artifact(&db, a, "public");
        link_input(&db, public_only, public).unwrap();
        assert!(access_of(&db, public_only).is_empty());

        let mixed = insert_artifact(&db, a, "m");
        for c in [public, gated] {
            link_input(&db, mixed, c).unwrap();
        }
        assert_eq!(access_of(&db, mixed), vec![m]);
    }

    /// 閲覧資格を直接書き込んで、入力と食い違わせることはできないこと。
    #[test]
    fn artifact_access_cannot_be_written_directly() {
        let db = Db::open_in_memory().unwrap();
        let m = insert_membership(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let art = insert_artifact(&db, a, "m");
        let insert = db
            .conn()
            .execute("INSERT INTO artifact_access VALUES (?1, ?2)", [art, m]);
        assert!(insert.is_err(), "{insert:?}");
        let delete = db
            .conn()
            .execute("DELETE FROM artifact_access WHERE artifact_id = ?1", [art]);
        assert!(delete.is_err(), "{delete:?}");
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
        let gated = insert_content(&db, a, Some(m));
        let art = insert_artifact(&db, a, "m");
        link_input(&db, art, gated).unwrap();
        let err = db
            .conn()
            .execute("DELETE FROM memberships WHERE id = ?1", [m])
            .unwrap_err();
        assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
        assert_eq!(access_of(&db, art), vec![m]);
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
        let art = insert_artifact(&db, a, "m");
        link_input(&db, art, c).unwrap();
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

    /// serve と timer の crawl が同時に新しい DB を開いても、マイグレーションが競合しないこと。
    #[test]
    fn concurrent_opens_migrate_once() {
        let dir = std::env::temp_dir().join(format!("nucrawler-{}-concurrent", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for round in 0..10 {
            let path = dir.join(format!("{round}.db"));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let path = path.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        Db::open(&path).map(|db| db.schema_version().unwrap())
                    })
                })
                .collect();
            for h in handles {
                let version = h.join().unwrap().unwrap();
                assert_eq!(version, MIGRATIONS.len() as i64);
            }
        }
    }

    #[test]
    fn timestamp_matches_sql_now_format() {
        assert_eq!(
            timestamp(t("2026-09-27T01:02:03Z")),
            "2026-09-27T01:02:03.000Z"
        );
        let db = Db::open_in_memory().unwrap();
        let now: String = db
            .conn()
            .query_row(&format!("SELECT {NOW}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(now.len(), "2026-09-27T01:02:03.000Z".len(), "{now}");
    }

    /// input_scope は会員資格の code を "+" でつないだものなので、区切りや予約語を code に使わせない。
    #[test]
    fn membership_codes_cannot_collide_with_scope_encoding() {
        let db = Db::open_in_memory().unwrap();
        for bad in ["public", "a+b", "", "AESJ", "a b"] {
            let err = db
                .conn()
                .execute(
                    "INSERT INTO memberships (code, name) VALUES (?1, 'x')",
                    [bad],
                )
                .unwrap_err();
            assert!(
                err.to_string().contains("membership code"),
                "{bad:?}: {err}"
            );
        }
        db.conn()
            .execute(
                "INSERT INTO memberships (code, name) VALUES ('ans_2', 'ANS')",
                [],
            )
            .unwrap();
        // 成果物の input_scope に code を複製して持つので、code は変更させない（正しい値にも）
        for new_code in ["public", "ans_3"] {
            let err = db
                .conn()
                .execute(
                    "UPDATE memberships SET code = ?1 WHERE code = 'ans_2'",
                    [new_code],
                )
                .unwrap_err();
            assert!(
                err.to_string().contains("membership code"),
                "{new_code}: {err}"
            );
        }
        db.conn()
            .execute(
                "UPDATE memberships SET name = 'ANS member' WHERE code = 'ans_2'",
                [],
            )
            .unwrap();
    }

    /// 索引を作る前に入っていた記事も引ける。
    #[test]
    fn migration_indexes_existing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute_batch(
            "INSERT INTO articles (id, source_id, url, title, lang, published_at, fetched_at)
               VALUES (1, 's', 'https://e.example/a', 'Old Title', 'en', '2026-09-01T00:00:00Z',
                       '2026-09-27T00:00:00.000Z');
             INSERT INTO contents (id, article_id, kind, text, origin, fetched_at)
               VALUES (1, 1, 'body', '古い本文の記述', 'page', '2026-09-27T00:00:00.000Z');
             INSERT INTO artifacts
               (id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
               VALUES (1, 1, 'digest', 'b', 'm', 1, 'public',
                       '{\"title_ja\": \"古い要約の題\", \"summary_ja\": \"s\"}', '2026-09-27T00:00:00Z');
             INSERT INTO artifact_inputs VALUES (1, 1, 1);",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        assert_eq!(search_ids(&db, &["old title"]), [1]);
        assert_eq!(search_ids(&db, &["本文の記述"]), [1]);
        assert_eq!(search_ids(&db, &["要約の題"]), [1]);
    }

    #[test]
    fn migration_seeds_topic_vocabulary() {
        use crate::topics::Facet;
        let db = Db::open_in_memory().unwrap();
        let topics = db.topics().unwrap();
        for facet in Facet::ALL {
            assert!(
                topics.iter().any(|t| t.facet == facet),
                "no seeded topic for {facet:?}"
            );
        }
        // 関心プロファイルの例の分野は語彙にある
        for name in ["規制・審査", "燃料", "高経年化", "安全解析"] {
            assert!(topics.iter().any(|t| t.name == name), "{name}");
        }
        let vocabulary = db.vocabulary().unwrap();
        assert_eq!(
            crate::topics::parse(&crate::topics::to_toml(&vocabulary)).unwrap(),
            vocabulary,
            "seeded vocabulary passes the import validation"
        );
    }

    /// 以前の定数の訳語集を移し、原語の表記の揺れや略語は 1 つの訳語にまとめる。
    #[test]
    fn migration_seeds_glossary_with_sources_and_abbreviations() {
        let db = Db::open_in_memory().unwrap();
        let glossary: Vec<_> = db
            .glossary_entries()
            .unwrap()
            .into_iter()
            .map(|e| e.term)
            .collect();
        let nrc = glossary
            .iter()
            .find(|t| t.target == "米国原子力規制委員会")
            .unwrap();
        assert_eq!(nrc.sources, ["Nuclear Regulatory Commission", "NRC"]);
        assert_eq!(nrc.abbr.as_deref(), Some("NRC"));
        assert!(
            glossary
                .iter()
                .any(|t| t.sources.contains(&"refueling outage".to_string())),
            "{glossary:?}"
        );
    }

    /// 種類を持つ前の訳語の指摘は、対応状況ごと訳語の指摘として移る。
    #[test]
    fn migration_moves_term_reports_with_their_handling() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let before = MIGRATIONS
            .iter()
            .position(|m| m.contains("ADD COLUMN reply"))
            .unwrap()
            + 1;
        for sql in &MIGRATIONS[..before] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", before as i64)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO articles (id, source_id, url, title, lang, fetched_at)
               VALUES (1, 's', 'https://e.example/a', 't', 'en', '2026-09-27T00:00:00.000Z');
             INSERT INTO term_reports
               (user_id, article_id, found, wanted, status, term_id, reply, reported_at, resolved_at)
               VALUES (1, 1, '給油停止', '燃料取替停止', 'added', 1, '追加', '2026-09-27T00:00:00.000Z',
                       '2026-09-27T01:00:00.000Z');",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let row: (String, String, String, i64, String) = conn
            .query_row(
                "SELECT kind, found, status, term_id, resolved_at FROM reports",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "term".into(),
                "給油停止".into(),
                "added".into(),
                1,
                "2026-09-27T01:00:00.000Z".into()
            )
        );
    }

    /// 語彙を入れる前の要約も、語彙と同じ名前のトピックは付与として移し、消せないようにする。
    #[test]
    fn migration_links_existing_digest_topics_in_the_vocabulary() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        for sql in &MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn.execute_batch(
            "INSERT INTO articles (id, source_id, url, title, lang, fetched_at)
               VALUES (1, 's', 'https://e.example/a', 't', 'en', '2026-09-27T00:00:00.000Z');
             INSERT INTO contents (id, article_id, kind, text, origin, fetched_at)
               VALUES (1, 1, 'body', 'x', 'page', '2026-09-27T00:00:00.000Z');
             INSERT INTO artifacts
               (id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
               VALUES (1, 1, 'digest', 'b', 'm', 1, 'public',
                       '{\"topics\": [\"規制・審査\", \"新設炉\", \"規制・審査\"]}',
                       '2026-09-27T00:00:00Z'),
                      (2, 1, 'judgment', 'b', 'm', 1, 'public',
                       '{\"topics\": [\"燃料\"]}', '2026-09-27T00:00:00Z');
             INSERT INTO artifact_inputs VALUES (1, 1, 1), (2, 1, 1);",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        let linked = db
            .query_strings(
                "SELECT t.name FROM artifact_topics AS at JOIN topics AS t ON t.id = at.topic_id
                 WHERE at.artifact_id IN (1, 2)",
            )
            .unwrap();
        assert_eq!(
            linked,
            ["規制・審査"],
            "only digest topics in the vocabulary"
        );
        let without: Vec<_> = db
            .vocabulary()
            .unwrap()
            .into_iter()
            .filter(|t| t.name != "規制・審査")
            .collect();
        let err = db.replace_topics(&without).unwrap_err();
        assert!(matches!(err, DbError::TopicsInUse(_)), "{err}");
    }

    /// 語彙の表を作ってから要約の保存が付与を書くまでの間に作られた要約も、付与を移す。
    #[test]
    fn migration_links_digest_topics_saved_before_linking() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        for sql in &MIGRATIONS[..7] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 7).unwrap();
        conn.execute_batch(
            "INSERT INTO articles (id, source_id, url, title, lang, fetched_at)
               VALUES (1, 's', 'https://e.example/a', 't', 'en', '2026-09-27T00:00:00.000Z');
             INSERT INTO contents (id, article_id, kind, text, origin, fetched_at)
               VALUES (1, 1, 'body', 'x', 'page', '2026-09-27T00:00:00.000Z');
             INSERT INTO artifacts
               (id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
               VALUES (1, 1, 'digest', 'b', 'm', 1, 'public',
                       '{\"topics\": [\"燃料\", \"新設炉\"]}', '2026-09-27T00:00:00Z');
             INSERT INTO artifact_inputs VALUES (1, 1, 1);",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        assert_eq!(linked_topics(&db, 1), ["燃料"]);
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

    #[test]
    fn migration_keeps_existing_events_and_accepts_new_kinds() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        for sql in &MIGRATIONS[..9] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 9).unwrap();
        conn.execute_batch(
            "INSERT INTO articles (id, source_id, url, title, lang, fetched_at)
               VALUES (1, 's', 'https://e.example/a', 't', 'en', '2026-09-27T00:00:00.000Z');
             INSERT INTO events (id, user_id, article_id, kind, created_at)
               VALUES (7, 1, 1, 'up', '2026-09-27T00:00:00.000Z');",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        assert_eq!(
            db.query_strings("SELECT id || kind || created_at FROM events")
                .unwrap(),
            ["7up2026-09-27T00:00:00.000Z"]
        );
        // 作り直した events の索引と、bookmarks の外部キーの子側の索引
        assert_eq!(
            db.query_strings(
                "SELECT name FROM sqlite_master
                 WHERE type = 'index' AND tbl_name IN ('events', 'bookmarks') AND sql IS NOT NULL
                 ORDER BY name"
            )
            .unwrap(),
            [
                "bookmarks_by_article",
                "bookmarks_by_event",
                "events_by_article",
                "events_by_user"
            ]
        );
        db.record_event(1, 1, SignalKind::Dismiss, t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.record_event(1, 1, SignalKind::Bookmark, t("2026-09-27T02:00:00Z"))
            .unwrap();
        let err = db
            .conn
            .execute(
                "INSERT INTO events (user_id, article_id, kind, created_at)
                 VALUES (1, 1, 'unknown', '2026-09-27T00:00:00.000Z')",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("CHECK"), "{err}");
    }
}
