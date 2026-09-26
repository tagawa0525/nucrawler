use std::path::Path;

use rusqlite::Connection;

use crate::config::Lang;

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
    SchemaTooNew { found: i64, supported: i64 },
    /// CHECK 制約で防いでいるはずの値が入っていた
    #[error("unexpected value in the database: {0}")]
    UnexpectedValue(String),
    /// 入力の無い成果物は閲覧資格を導出できず、公開扱いになってしまうので登録しない。
    #[error("artifact for article {article_id} has no input contents")]
    NoArtifactInputs { article_id: i64 },
}

/// 適用順に並べたマイグレーション。`PRAGMA user_version` は適用済みの件数。
/// 既存の要素は書き換えず、変更は新しい要素の追加で行う。
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_init.sql"),
    include_str!("migrations/0002_membership_code_check.sql"),
];

/// 現在時刻（UTC、RFC 3339、ミリ秒まで）を返す SQL 式。
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Lead,
    Body,
    Abstract,
    Fulltext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentOrigin {
    Feed,
    Page,
    Pdf,
    Upload,
    Login,
}

impl ContentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Body => "body",
            Self::Abstract => "abstract",
            Self::Fulltext => "fulltext",
        }
    }
}

impl ContentOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Feed => "feed",
            Self::Page => "page",
            Self::Pdf => "pdf",
            Self::Upload => "upload",
            Self::Login => "login",
        }
    }
}

/// `status` 用：ソースごとの記事数と取得状況。
#[derive(Debug, PartialEq, Eq)]
pub struct SourceOverview {
    pub source_id: String,
    pub articles: i64,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
}

/// DB に書く時刻の書式。SQL の `NOW` と同じく UTC・ミリ秒・'Z' に揃え、文字列の大小で比較できるようにする。
pub fn timestamp(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// `attempts` 回目の失敗の後に待つ時間：1 時間から倍々で、最大 7 日。
fn backoff(attempts: i64) -> chrono::Duration {
    let hours = 1i64 << (attempts - 1).clamp(0, 16);
    chrono::Duration::hours(hours).min(chrono::Duration::days(7))
}

/// 一時的な失敗の再試行は `MAX_ATTEMPTS` 回まで。間隔は 1 時間から倍々で、最大 7 日。
pub const MAX_ATTEMPTS: i64 = 5;

/// `stage_errors` の行を特定するキー。LLM を使わないステージは backend と model を "" にする。
#[derive(Debug, Clone, Copy)]
pub struct StageKey<'a> {
    pub article_id: i64,
    pub stage: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
}

/// `llm_calls` に記録する 1 回の呼び出し。
#[derive(Debug)]
pub struct LlmCall<'a> {
    pub stage: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
    pub n_items: usize,
    pub ok: bool,
    pub duration_ms: u64,
    pub error: Option<&'a str>,
    pub rate_limit: Option<&'a crate::llm::RateLimit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Digest,
    Translation,
    Judgment,
}

impl ArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Digest => "digest",
            Self::Translation => "translation",
            Self::Judgment => "judgment",
        }
    }
}

/// 登録する成果物。`inputs` は元にした本文の部分（contents.id）で、空は許さない。
#[derive(Debug)]
pub struct NewArtifact<'a> {
    pub article_id: i64,
    pub kind: ArtifactKind,
    pub backend: &'a str,
    pub model: &'a str,
    pub prompt_version: i64,
    pub payload: &'a serde_json::Value,
    pub inputs: &'a [i64],
}

/// 要約の入力にする記事と、その公開の本文の部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestInput {
    pub article_id: i64,
    pub source_id: String,
    pub title: String,
    pub lang: String,
    pub contents: Vec<InputContent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputContent {
    pub id: i64,
    pub kind: String,
    pub text: String,
}

/// 採点の対象を特定するキー（誰の・どのプロファイルで・どのモデルで）。
#[derive(Debug, Clone, Copy)]
pub struct ScoreKey<'a> {
    pub user_id: i64,
    pub profile_hash: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
}

/// 採点の失敗を記録するステージ名。`stage_errors` の主キーは記事・ステージ・バックエンド・
/// モデルで、利用者とプロファイルを持たないので、ステージ名にそれらを含めて範囲を区別する。
pub fn score_stage(key: ScoreKey) -> String {
    format!("score:{}:{}", key.user_id, key.profile_hash)
}

/// 採点に渡す記事（その利用者が閲覧できる最新の digest）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreInput {
    pub article_id: i64,
    pub artifact_id: i64,
    pub title_ja: String,
    pub summary_ja: String,
    pub topics: Vec<String>,
}

/// 利用者の行動。推薦への効き方は 👎 ≫ 詳細を開いた ＜ 和訳を開いた ≪ 👍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    OpenDetail,
    OpenTranslation,
    Up,
    Down,
}

impl SignalKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::OpenDetail => "open_detail",
            Self::OpenTranslation => "open_translation",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

/// 採点の参考にする直近の行動と、その記事の見出し。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub kind: SignalKind,
    pub title_ja: String,
}

/// 抽出待ちの記事。
#[derive(Debug, PartialEq, Eq)]
pub struct PendingPage {
    pub article_id: i64,
    pub source_id: String,
    pub url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SourceState {
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
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

    /// URL を正規化して登録する。既に同じ URL があれば `None`。
    pub fn insert_article(&self, a: &NewArticle) -> Result<Option<i64>, DbError> {
        let url = normalize_url(a.url)?;
        let lang = match a.lang {
            Lang::En => "en",
            Lang::Ja => "ja",
        };
        let inserted = self.conn.execute(
            &format!(
                "INSERT INTO articles (source_id, url, title, lang, published_at, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, {NOW})
                 ON CONFLICT (url) DO NOTHING"
            ),
            rusqlite::params![a.source_id, url, a.title, lang, a.published_at],
        )?;
        Ok((inserted > 0).then(|| self.conn.last_insert_rowid()))
    }

    /// 公開の本文の部分を登録する。会員限定の部分はログイン取得の実装時に別の関数で扱う。
    pub fn insert_content(
        &self,
        article_id: i64,
        kind: ContentKind,
        origin: ContentOrigin,
        text: &str,
    ) -> Result<i64, DbError> {
        Ok(self.conn.query_row(
            &format!(
                "INSERT INTO contents (article_id, kind, origin, text, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, {NOW}) RETURNING id"
            ),
            rusqlite::params![article_id, kind.as_str(), origin.as_str(), text],
            |r| r.get(0),
        )?)
    }

    /// 記事と、その公開の本文の部分を 1 つのトランザクションで登録する。既に同じ URL があれば
    /// 何もせず `None`。途中で止まっても「記事だけあって本文が無い」状態を残さない。
    pub fn insert_article_with_contents(
        &self,
        a: &NewArticle,
        contents: &[(ContentKind, ContentOrigin, &str)],
    ) -> Result<Option<i64>, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        let Some(id) = self.insert_article(a)? else {
            return Ok(None);
        };
        for &(kind, origin, text) in contents {
            self.insert_content(id, kind, origin, text)?;
        }
        tx.commit()?;
        Ok(Some(id))
    }

    /// 取得に成功した時刻を記録する。直前のエラーは消す。
    pub fn record_source_success(&self, source_id: &str) -> Result<(), DbError> {
        self.conn.execute(
            &format!(
                "INSERT INTO source_state (source_id, last_success_at) VALUES (?1, {NOW})
                 ON CONFLICT (source_id) DO UPDATE SET
                   last_success_at = excluded.last_success_at,
                   last_error = NULL,
                   last_error_at = NULL"
            ),
            [source_id],
        )?;
        Ok(())
    }

    /// 取得の失敗を記録する。最後に成功した時刻は残す。
    pub fn record_source_failure(&self, source_id: &str, error: &str) -> Result<(), DbError> {
        self.conn.execute(
            &format!(
                "INSERT INTO source_state (source_id, last_error, last_error_at) VALUES (?1, ?2, {NOW})
                 ON CONFLICT (source_id) DO UPDATE SET
                   last_error = excluded.last_error,
                   last_error_at = excluded.last_error_at"
            ),
            [source_id, error],
        )?;
        Ok(())
    }

    pub fn source_state(&self, source_id: &str) -> Result<Option<SourceState>, DbError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT last_success_at, last_error, last_error_at FROM source_state
                 WHERE source_id = ?1",
                [source_id],
                |r| {
                    Ok(SourceState {
                        last_success_at: r.get(0)?,
                        last_error: r.get(1)?,
                        last_error_at: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// 記事か取得記録のあるソースすべて（source_id 順）。
    pub fn source_overview(&self) -> Result<Vec<SourceOverview>, DbError> {
        let mut stmt = self.conn.prepare(
            "WITH ids AS (SELECT source_id FROM articles UNION SELECT source_id FROM source_state),
                  counts AS (SELECT source_id, count(*) AS n FROM articles GROUP BY source_id)
             SELECT ids.source_id, coalesce(counts.n, 0),
                    st.last_success_at, st.last_error, st.last_error_at
             FROM ids
             LEFT JOIN counts USING (source_id)
             LEFT JOIN source_state AS st USING (source_id)
             ORDER BY ids.source_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SourceOverview {
                source_id: r.get(0)?,
                articles: r.get(1)?,
                last_success_at: r.get(2)?,
                last_error: r.get(3)?,
                last_error_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 失敗を記録する。`permanent` なら再試行しない（試行回数を上限にする）。
    /// そうでなければ試行回数を 1 増やし、次に試してよい時刻を指数的に先へ延ばす。
    /// 以後は再試行しない（断念した）なら `true` を返す。
    pub fn record_stage_failure(
        &self,
        key: StageKey,
        error: &str,
        now: chrono::DateTime<chrono::Utc>,
        permanent: bool,
    ) -> Result<bool, DbError> {
        use rusqlite::OptionalExtension;
        let tx = self.conn.unchecked_transaction()?;
        let previous: i64 = tx
            .query_row(
                "SELECT attempts FROM stage_errors
                 WHERE article_id = ?1 AND stage = ?2 AND backend = ?3 AND model = ?4",
                rusqlite::params![key.article_id, key.stage, key.backend, key.model],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let attempts = if permanent {
            MAX_ATTEMPTS
        } else {
            (previous + 1).min(MAX_ATTEMPTS)
        };
        tx.execute(
            "INSERT INTO stage_errors
               (article_id, stage, backend, model, attempts, last_error, next_retry_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (article_id, stage, backend, model) DO UPDATE SET
               attempts = excluded.attempts,
               last_error = excluded.last_error,
               next_retry_at = excluded.next_retry_at",
            rusqlite::params![
                key.article_id,
                key.stage,
                key.backend,
                key.model,
                attempts,
                error,
                timestamp(now + backoff(attempts)),
            ],
        )?;
        tx.commit()?;
        Ok(attempts >= MAX_ATTEMPTS)
    }

    /// 成功したら失敗の記録を消す。
    pub fn clear_stage_failure(&self, key: StageKey) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM stage_errors
             WHERE article_id = ?1 AND stage = ?2 AND backend = ?3 AND model = ?4",
            rusqlite::params![key.article_id, key.stage, key.backend, key.model],
        )?;
        Ok(())
    }

    /// 本文（body か fulltext）が無く、`cutoff` 以降に公開（無ければ取得）され、再試行待ちでも
    /// 断念済みでもない記事を、新しい順に最大 `limit` 件返す。
    pub fn pending_extract(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<PendingPage>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.source_id, a.url FROM articles AS a
             WHERE coalesce(a.published_at, a.fetched_at) >= ?1
               AND NOT EXISTS (
                 SELECT 1 FROM contents AS c
                 WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext'))
               AND NOT EXISTS (
                 SELECT 1 FROM stage_errors AS e
                 WHERE e.article_id = a.id AND e.stage = 'extract'
                   AND e.backend = '' AND e.model = ''
                   AND (e.attempts >= ?2 OR e.next_retry_at > ?3))
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                timestamp(cutoff),
                MAX_ATTEMPTS,
                timestamp(now),
                // 負の LIMIT は SQLite では無制限になるので、桁あふれさせずに丸める
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            |r| {
                Ok(PendingPage {
                    article_id: r.get(0)?,
                    source_id: r.get(1)?,
                    url: r.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn record_llm_call(
        &self,
        call: &LlmCall,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let rate_limit = call.rate_limit.map(serde_json::to_string).transpose()?;
        self.conn.execute(
            "INSERT INTO llm_calls
               (at, stage, backend, model, n_items, ok, duration_ms, error, rate_limit)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                timestamp(at),
                call.stage,
                call.backend,
                call.model,
                i64::try_from(call.n_items).unwrap_or(i64::MAX),
                call.ok,
                i64::try_from(call.duration_ms).unwrap_or(i64::MAX),
                call.error,
                rate_limit,
            ],
        )?;
        Ok(())
    }

    /// 最後に記録された使用率（無ければ `None`）。
    pub fn latest_rate_limit(&self) -> Result<Option<crate::llm::RateLimit>, DbError> {
        use rusqlite::OptionalExtension;
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT rate_limit FROM llm_calls WHERE rate_limit IS NOT NULL
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    /// ユーザーのプロファイルを保存する（既にあれば置き換える）。
    pub fn save_profile(
        &self,
        user_id: i64,
        profile: &crate::profile::Profile,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO profiles (user_id, interests, excludes, hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (user_id) DO UPDATE SET
               interests = excluded.interests,
               excludes = excluded.excludes,
               hash = excluded.hash,
               updated_at = excluded.updated_at",
            rusqlite::params![
                user_id,
                serde_json::to_string(&profile.interests)?,
                serde_json::to_string(&profile.exclude)?,
                crate::profile::hash(profile),
                timestamp(now),
            ],
        )?;
        Ok(())
    }

    /// ユーザーのプロファイルとそのハッシュ。
    pub fn load_profile(
        &self,
        user_id: i64,
    ) -> Result<Option<(crate::profile::Profile, String)>, DbError> {
        use rusqlite::OptionalExtension;
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT interests, excludes, hash FROM profiles WHERE user_id = ?1",
                [user_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        row.map(|(interests, excludes, hash)| {
            let profile = crate::profile::Profile {
                interests: serde_json::from_str(&interests)?,
                exclude: serde_json::from_str(&excludes)?,
            };
            Ok((profile, hash))
        })
        .transpose()
    }

    /// 成果物と、その入力（artifact_inputs）を 1 つのトランザクションで登録する。
    /// `input_scope` は入力の会員資格から導出する（会員限定の部分が無ければ "public"）。
    pub fn insert_artifact(
        &self,
        a: &NewArtifact,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        if a.inputs.is_empty() {
            return Err(DbError::NoArtifactInputs {
                article_id: a.article_id,
            });
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut codes = std::collections::BTreeSet::new();
        for &content_id in a.inputs {
            let code: Option<String> = tx.query_row(
                "SELECT m.code FROM contents AS c
                 LEFT JOIN memberships AS m ON m.id = c.access_membership_id
                 WHERE c.id = ?1",
                [content_id],
                |r| r.get(0),
            )?;
            codes.extend(code);
        }
        let input_scope = if codes.is_empty() {
            "public".to_string()
        } else {
            codes.into_iter().collect::<Vec<_>>().join("+")
        };
        let id: i64 = tx.query_row(
            "INSERT INTO artifacts
               (article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) RETURNING id",
            rusqlite::params![
                a.article_id,
                a.kind.as_str(),
                a.backend,
                a.model,
                a.prompt_version,
                input_scope,
                a.payload.to_string(),
                timestamp(now),
            ],
            |r| r.get(0),
        )?;
        for &content_id in a.inputs {
            tx.execute(
                "INSERT INTO artifact_inputs (artifact_id, article_id, content_id)
                 VALUES (?1, ?2, ?3)",
                [id, a.article_id, content_id],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// まだ digest が 1 つも無い記事を、新しい順に最大 `limit` 件、公開の入力とともに返す
    /// （会員限定の本文は、ログイン取得を実装するまで扱わない）。
    /// 本文（body/fulltext）がある記事に加え、抽出を断念して概要（lead/abstract）しか無い記事も含める。
    /// 抽出の再試行待ちの記事は、本文が取れるのを待つので含めない。
    /// `backend`/`model` の digest の失敗で再試行待ち・断念済みの記事も含めない。
    pub fn pending_digest(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        backend: &str,
        model: &str,
        limit: usize,
    ) -> Result<Vec<DigestInput>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.source_id, a.title, a.lang FROM articles AS a
             WHERE coalesce(a.published_at, a.fetched_at) >= ?1
               AND NOT EXISTS (
                 SELECT 1 FROM artifacts AS r WHERE r.article_id = a.id AND r.kind = 'digest')
               AND (
                 EXISTS (
                   SELECT 1 FROM contents AS c
                   WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext')
                     AND c.access_membership_id IS NULL)
                 OR (
                   EXISTS (
                     SELECT 1 FROM contents AS c
                     WHERE c.article_id = a.id AND c.kind IN ('lead', 'abstract')
                       AND c.access_membership_id IS NULL)
                   AND EXISTS (
                     SELECT 1 FROM stage_errors AS e
                     WHERE e.article_id = a.id AND e.stage = 'extract'
                       AND e.backend = '' AND e.model = '' AND e.attempts >= ?2)))
               AND NOT EXISTS (
                 SELECT 1 FROM stage_errors AS e
                 WHERE e.article_id = a.id AND e.stage = 'digest'
                   AND e.backend = ?3 AND e.model = ?4
                   AND (e.attempts >= ?2 OR e.next_retry_at > ?5))
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?6",
        )?;
        let articles = stmt
            .query_map(
                rusqlite::params![
                    timestamp(cutoff),
                    MAX_ATTEMPTS,
                    backend,
                    model,
                    timestamp(now),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?
            .collect::<Result<Vec<(i64, String, String, String)>, _>>()?;
        let mut contents = self.conn.prepare(
            "SELECT id, kind, text FROM contents
             WHERE article_id = ?1 AND access_membership_id IS NULL
             ORDER BY CASE kind WHEN 'lead' THEN 0 WHEN 'abstract' THEN 1
                                WHEN 'body' THEN 2 ELSE 3 END, id",
        )?;
        articles
            .into_iter()
            .map(|(article_id, source_id, title, lang)| {
                let contents = contents
                    .query_map([article_id], |r| {
                        Ok(InputContent {
                            id: r.get(0)?,
                            kind: r.get(1)?,
                            text: r.get(2)?,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(DigestInput {
                    article_id,
                    source_id,
                    title,
                    lang,
                    contents,
                })
            })
            .collect()
    }

    /// 各記事について利用者が閲覧できる最新の digest のうち、軽水炉に関係し（lwr_relevant）、
    /// `cutoff` 以降の記事で、このキーの採点がまだ無いものを新しい順に返す。
    /// このモデルの採点の失敗で再試行待ち・断念済みの記事は含めない。
    pub fn pending_score(
        &self,
        key: ScoreKey,
        cutoff: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<ScoreInput>, DbError> {
        let mut stmt = self.conn.prepare(
            "WITH viewable AS (
               -- 利用者が持っていない会員資格を必要とする digest は見せない
               SELECT r.id, r.article_id, r.created_at, r.title_ja, r.summary_ja, r.payload
               FROM artifacts AS r
               WHERE r.kind = 'digest'
                 AND NOT EXISTS (
                   SELECT 1 FROM artifact_access AS aa
                   WHERE aa.artifact_id = r.id
                     AND aa.membership_id NOT IN (
                       SELECT membership_id FROM user_memberships WHERE user_id = ?1))
             ),
             latest AS (
               SELECT v.* FROM viewable AS v
               WHERE NOT EXISTS (
                 SELECT 1 FROM viewable AS w
                 WHERE w.article_id = v.article_id
                   AND (w.created_at > v.created_at
                        OR (w.created_at = v.created_at AND w.id > v.id)))
             )
             SELECT l.article_id, l.id, l.title_ja, l.summary_ja,
                    json_extract(l.payload, '$.topics')
             FROM latest AS l
             JOIN articles AS a ON a.id = l.article_id
             WHERE json_extract(l.payload, '$.lwr_relevant') = 1
               AND coalesce(a.published_at, a.fetched_at) >= ?2
               AND NOT EXISTS (
                 SELECT 1 FROM scores AS s
                 WHERE s.user_id = ?1 AND s.artifact_id = l.id AND s.profile_hash = ?3
                   AND s.backend = ?4 AND s.model = ?5)
               AND NOT EXISTS (
                 SELECT 1 FROM stage_errors AS e
                 WHERE e.article_id = l.article_id AND e.stage = ?9
                   AND e.backend = ?4 AND e.model = ?5
                   AND (e.attempts >= ?6 OR e.next_retry_at > ?7))
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?8",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                key.user_id,
                timestamp(cutoff),
                key.profile_hash,
                key.backend,
                key.model,
                MAX_ATTEMPTS,
                timestamp(now),
                i64::try_from(limit).unwrap_or(i64::MAX),
                score_stage(key),
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (article_id, artifact_id, title_ja, summary_ja, topics) = row?;
            let topics = match topics {
                Some(json) => serde_json::from_str(&json)?,
                None => Vec::new(),
            };
            Ok(ScoreInput {
                article_id,
                artifact_id,
                title_ja,
                summary_ja,
                topics,
            })
        })
        .collect()
    }

    pub fn insert_score(
        &self,
        key: ScoreKey,
        artifact_id: i64,
        score: u8,
        reason: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO scores
               (user_id, artifact_id, profile_hash, backend, model, score, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                key.user_id,
                artifact_id,
                key.profile_hash,
                key.backend,
                key.model,
                score,
                reason,
                timestamp(now),
            ],
        )?;
        Ok(())
    }

    pub fn record_event(
        &self,
        user_id: i64,
        article_id: i64,
        kind: SignalKind,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO events (user_id, article_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![user_id, article_id, kind.as_str(), timestamp(now)],
        )?;
        Ok(())
    }

    /// 直近の行動を新しい順に最大 `limit` 件。digest の無い記事の行動は含めない。
    pub fn recent_signals(&self, user_id: i64, limit: usize) -> Result<Vec<Signal>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, title_ja FROM (
               SELECT e.id, e.created_at, e.kind,
                      (SELECT r.title_ja FROM artifacts AS r
                       WHERE r.article_id = e.article_id AND r.kind = 'digest'
                         -- 利用者が閲覧できない（会員限定の）digest の見出しは使わない
                         AND NOT EXISTS (
                           SELECT 1 FROM artifact_access AS aa
                           WHERE aa.artifact_id = r.id
                             AND aa.membership_id NOT IN (
                               SELECT membership_id FROM user_memberships
                               WHERE user_id = ?1))
                       ORDER BY r.created_at DESC, r.id DESC LIMIT 1) AS title_ja
               FROM events AS e WHERE e.user_id = ?1)
             WHERE title_ja IS NOT NULL
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![user_id, i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?;
        rows.map(|row| {
            let (kind, title_ja) = row?;
            let kind = match kind.as_str() {
                "open_detail" => SignalKind::OpenDetail,
                "open_translation" => SignalKind::OpenTranslation,
                "up" => SignalKind::Up,
                "down" => SignalKind::Down,
                other => {
                    return Err(DbError::UnexpectedValue(format!("events.kind = {other:?}")));
                }
            };
            Ok(Signal { kind, title_ja })
        })
        .collect()
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
    fn conn(&self) -> &Connection {
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
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let found = schema_version(&tx)?;
    let supported = MIGRATIONS.len() as i64;
    if found < 0 {
        return Err(DbError::InvalidSchemaVersion(found));
    }
    if found > supported {
        return Err(DbError::SchemaTooNew { found, supported });
    }
    for sql in &MIGRATIONS[found as usize..] {
        tx.execute_batch(sql)?;
    }
    tx.pragma_update(None, "user_version", supported)?;
    tx.commit()?;
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

    fn insert_artifact(db: &Db, article_id: i64, input_scope: &str) -> i64 {
        db.conn()
            .execute(
                "INSERT INTO artifacts
                   (article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
                 VALUES (?1, 'digest', 'b', 'm', 1, ?2, '{}', '2026-09-27T00:00:00Z')",
                rusqlite::params![article_id, input_scope],
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

    fn insert_content(db: &Db, article_id: i64, membership: Option<i64>) -> i64 {
        db.conn()
            .execute(
                "INSERT INTO contents (article_id, kind, access_membership_id, text, origin, fetched_at)
                 VALUES (?1, 'body', ?2, 'x', 'page', '2026-09-27T00:00:00Z')",
                rusqlite::params![article_id, membership],
            )
            .unwrap();
        db.conn().last_insert_rowid()
    }

    /// 成果物の記事 id を使って入力を紐付ける。
    fn link_input(db: &Db, artifact_id: i64, content_id: i64) -> rusqlite::Result<usize> {
        db.conn().execute(
            "INSERT INTO artifact_inputs (artifact_id, article_id, content_id)
             SELECT id, article_id, ?2 FROM artifacts WHERE id = ?1",
            [artifact_id, content_id],
        )
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

    fn access_of(db: &Db, artifact_id: i64) -> Vec<i64> {
        let mut stmt = db
            .conn()
            .prepare("SELECT membership_id FROM artifact_access WHERE artifact_id = ?1 ORDER BY 1")
            .unwrap();
        stmt.query_map([artifact_id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
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
    fn inserts_public_content() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        db.insert_content(a, ContentKind::Lead, ContentOrigin::Feed, "概要")
            .unwrap();
        let rows = db
            .query_strings(
                "SELECT kind || '|' || origin || '|' || coalesce(access_membership_id, 'public')
                        || '|' || text || '|' || (fetched_at LIKE '____-__-__T__:__:__%Z')
                 FROM contents",
            )
            .unwrap();
        assert_eq!(rows, ["lead|feed|public|概要|1"]);
    }

    #[test]
    fn records_source_success_and_failure() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.source_state("s").unwrap(), None);

        db.record_source_failure("s", "HTTP 403").unwrap();
        let st = db.source_state("s").unwrap().unwrap();
        assert_eq!(st.last_error.as_deref(), Some("HTTP 403"));
        assert!(st.last_error_at.is_some());
        assert!(st.last_success_at.is_none());

        db.record_source_success("s").unwrap();
        let st = db.source_state("s").unwrap().unwrap();
        assert!(st.last_success_at.is_some());
        assert_eq!((st.last_error, st.last_error_at), (None, None));

        db.record_source_failure("s", "timeout").unwrap();
        let st = db.source_state("s").unwrap().unwrap();
        assert!(st.last_success_at.is_some(), "last success is kept");
        assert_eq!(st.last_error.as_deref(), Some("timeout"));
    }

    #[test]
    fn overview_combines_articles_and_state() {
        let db = Db::open_in_memory().unwrap();
        for url in ["https://e.com/1", "https://e.com/2"] {
            db.insert_article(&NewArticle {
                source_id: "a",
                ..article(url)
            })
            .unwrap();
        }
        db.record_source_success("a").unwrap();
        db.record_source_failure("b", "HTTP 403").unwrap();
        let ov = db.source_overview().unwrap();
        let ids: Vec<_> = ov
            .iter()
            .map(|o| (o.source_id.as_str(), o.articles))
            .collect();
        assert_eq!(ids, [("a", 2), ("b", 0)]);
        assert!(ov[0].last_success_at.is_some());
        assert_eq!(ov[1].last_error.as_deref(), Some("HTTP 403"));
    }

    fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().to_utc()
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

    fn page_article(db: &Db, url: &str, published: &str) -> i64 {
        db.insert_article(&NewArticle {
            published_at: Some(published),
            ..article(url)
        })
        .unwrap()
        .unwrap()
    }

    fn pending_ids(db: &Db, now: &str) -> Vec<i64> {
        db.pending_extract(t("2026-09-10T00:00:00Z"), t(now), 10)
            .unwrap()
            .into_iter()
            .map(|p| p.article_id)
            .collect()
    }

    #[test]
    fn pending_extract_selects_recent_articles_without_body_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let old = page_article(&db, "https://e.com/old", "2026-09-01T00:00:00.000Z");
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-25T00:00:00.000Z");
        let with_body = page_article(&db, "https://e.com/c", "2026-09-26T00:00:00.000Z");
        db.insert_content(with_body, ContentKind::Body, ContentOrigin::Feed, "x")
            .unwrap();
        let with_lead = page_article(&db, "https://e.com/d", "2026-09-24T00:00:00.000Z");
        db.insert_content(with_lead, ContentKind::Lead, ContentOrigin::Feed, "x")
            .unwrap();
        let _ = old;
        assert_eq!(pending_ids(&db, "2026-09-27T00:00:00Z"), [b, with_lead, a]);
        let limited = db
            .pending_extract(t("2026-09-10T00:00:00Z"), t("2026-09-27T00:00:00Z"), 1)
            .unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].url, "https://e.com/b");
    }

    #[test]
    fn transient_failures_back_off_exponentially_then_give_up() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let key = StageKey {
            article_id: a,
            stage: "extract",
            backend: "",
            model: "",
        };
        db.record_stage_failure(key, "HTTP 500", t("2026-09-27T00:00:00Z"), false)
            .unwrap();
        // 1 回目の失敗後は 1 時間待つ
        assert!(pending_ids(&db, "2026-09-27T00:59:00Z").is_empty());
        assert_eq!(pending_ids(&db, "2026-09-27T01:00:00Z"), [a]);
        // 2 回目は 2 時間
        db.record_stage_failure(key, "HTTP 500", t("2026-09-27T01:00:00Z"), false)
            .unwrap();
        assert!(pending_ids(&db, "2026-09-27T02:59:00Z").is_empty());
        assert_eq!(pending_ids(&db, "2026-09-27T03:00:00Z"), [a]);
        // 上限に達したら断念する
        for _ in 2..MAX_ATTEMPTS {
            db.record_stage_failure(key, "HTTP 500", t("2026-09-27T03:00:00Z"), false)
                .unwrap();
        }
        assert!(pending_ids(&db, "2026-12-31T00:00:00Z").is_empty());
        let err: String = db
            .conn()
            .query_row("SELECT last_error FROM stage_errors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(err, "HTTP 500");
    }

    #[test]
    fn permanent_failure_is_not_retried_and_success_clears() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-21T00:00:00.000Z");
        let key = |article_id| StageKey {
            article_id,
            stage: "extract",
            backend: "",
            model: "",
        };
        db.record_stage_failure(key(a), "robots", t("2026-09-27T00:00:00Z"), true)
            .unwrap();
        db.record_stage_failure(key(b), "HTTP 500", t("2026-09-27T00:00:00Z"), false)
            .unwrap();
        db.clear_stage_failure(key(b)).unwrap();
        assert_eq!(pending_ids(&db, "2026-12-31T00:00:00Z"), [b]);
    }

    #[test]
    fn failures_of_other_stages_do_not_block_extract() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let key = StageKey {
            article_id: a,
            stage: "digest",
            backend: "claude-cli",
            model: "sonnet",
        };
        db.record_stage_failure(key, "x", t("2026-09-27T00:00:00Z"), true)
            .unwrap();
        assert_eq!(pending_ids(&db, "2026-09-27T00:00:00Z"), [a]);
    }

    #[test]
    fn records_llm_calls_with_rate_limit() {
        let db = Db::open_in_memory().unwrap();
        let rate = crate::llm::RateLimit {
            five_hour: Some(crate::llm::Window {
                utilization: 0.5,
                resets_at: 1790457000,
            }),
            seven_day: None,
        };
        db.record_llm_call(
            &LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 5,
                ok: true,
                duration_ms: 1234,
                error: None,
                rate_limit: Some(&rate),
            },
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        db.record_llm_call(
            &LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 5,
                ok: false,
                duration_ms: 10,
                error: Some("timeout"),
                rate_limit: None,
            },
            t("2026-09-27T01:05:00Z"),
        )
        .unwrap();
        let rows = db
            .query_strings(
                "SELECT at || '|' || stage || '|' || n_items || '|' || ok || '|' || coalesce(error, '-')
                        || '|' || coalesce(json_extract(rate_limit, '$.five_hour.utilization'), '-')
                 FROM llm_calls ORDER BY id",
            )
            .unwrap();
        assert_eq!(
            rows,
            [
                "2026-09-27T01:00:00.000Z|digest|5|1|-|0.5",
                "2026-09-27T01:05:00.000Z|digest|5|0|timeout|-",
            ]
        );
    }

    #[test]
    fn latest_rate_limit_skips_calls_without_usage() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.latest_rate_limit().unwrap(), None);
        fn call(rate: Option<&crate::llm::RateLimit>) -> LlmCall<'_> {
            LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 1,
                ok: rate.is_some(),
                duration_ms: 1,
                error: None,
                rate_limit: rate,
            }
        }
        let older = crate::llm::RateLimit {
            five_hour: Some(crate::llm::Window {
                utilization: 0.2,
                resets_at: 1,
            }),
            seven_day: None,
        };
        let newer = crate::llm::RateLimit {
            five_hour: Some(crate::llm::Window {
                utilization: 0.4,
                resets_at: 2,
            }),
            seven_day: None,
        };
        db.record_llm_call(&call(Some(&older)), t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.record_llm_call(&call(Some(&newer)), t("2026-09-27T02:00:00Z"))
            .unwrap();
        db.record_llm_call(&call(None), t("2026-09-27T03:00:00Z"))
            .unwrap();
        assert_eq!(db.latest_rate_limit().unwrap(), Some(newer));
    }

    fn digest_payload() -> serde_json::Value {
        serde_json::json!({"title_ja": "題", "summary_ja": "要約"})
    }

    #[test]
    fn insert_artifact_links_inputs_and_derives_scope() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let lead = db
            .insert_content(a, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        let body = db
            .insert_content(a, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let payload = digest_payload();
        let id = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[lead, body],
                },
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        let row = db
            .query_strings(&format!(
                "SELECT kind || '|' || input_scope || '|' || title_ja || '|' || created_at
                 FROM artifacts WHERE id = {id}"
            ))
            .unwrap();
        assert_eq!(row, ["digest|public|題|2026-09-27T00:00:00.000Z"]);
        assert_eq!(
            db.query_i64(&format!(
                "SELECT count(*) FROM artifact_inputs WHERE artifact_id = {id}"
            ))
            .unwrap(),
            2
        );
    }

    #[test]
    fn insert_artifact_scope_reflects_gated_inputs() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let gated = insert_content(&db, a, Some(aesj));
        let payload = digest_payload();
        let id = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[gated],
                },
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        assert_eq!(
            db.query_strings(&format!(
                "SELECT input_scope FROM artifacts WHERE id = {id}"
            ))
            .unwrap(),
            ["aesj"]
        );
        assert_eq!(access_of(&db, id), vec![aesj]);
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

    #[test]
    fn insert_artifact_rejects_empty_inputs() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let payload = digest_payload();
        let err = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[],
                },
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap_err();
        assert!(matches!(err, DbError::NoArtifactInputs { .. }), "{err}");
        assert_eq!(db.query_i64("SELECT count(*) FROM artifacts").unwrap(), 0);
    }

    fn digest_ids(db: &Db, now: &str) -> Vec<i64> {
        db.pending_digest(
            t("2026-09-10T00:00:00Z"),
            t(now),
            "claude-cli",
            "sonnet",
            10,
        )
        .unwrap()
        .into_iter()
        .map(|d| d.article_id)
        .collect()
    }

    #[test]
    fn pending_digest_selects_articles_ready_for_summary() {
        let db = Db::open_in_memory().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let extract_key = |article_id| StageKey {
            article_id,
            stage: "extract",
            backend: "",
            model: "",
        };
        // 本文あり → 対象
        let with_body = page_article(&db, "https://e.com/body", "2026-09-26T00:00:00.000Z");
        db.insert_content(with_body, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        db.insert_content(with_body, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        // 概要だけで抽出を断念 → 対象
        let lead_only = page_article(&db, "https://e.com/lead", "2026-09-25T00:00:00.000Z");
        db.insert_content(lead_only, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        db.record_stage_failure(extract_key(lead_only), "403", t(now), true)
            .unwrap();
        // 概要だけで抽出の再試行待ち → 本文を待つ
        let waiting = page_article(&db, "https://e.com/wait", "2026-09-24T00:00:00.000Z");
        db.insert_content(waiting, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        db.record_stage_failure(extract_key(waiting), "500", t(now), false)
            .unwrap();
        // 本文なし・概要なし → 入力が無い
        let _empty = page_article(&db, "https://e.com/empty", "2026-09-23T00:00:00.000Z");
        // 期間外
        let old = page_article(&db, "https://e.com/old", "2026-09-01T00:00:00.000Z");
        db.insert_content(old, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        // digest 済み
        let done = page_article(&db, "https://e.com/done", "2026-09-22T00:00:00.000Z");
        let c = db
            .insert_content(done, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let payload = digest_payload();
        db.insert_artifact(
            &NewArtifact {
                article_id: done,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "haiku",
                prompt_version: 1,
                payload: &payload,
                inputs: &[c],
            },
            t(now),
        )
        .unwrap();

        assert_eq!(digest_ids(&db, now), [with_body, lead_only]);

        let inputs = db
            .pending_digest(t("2026-09-10T00:00:00Z"), t(now), "claude-cli", "sonnet", 1)
            .unwrap();
        assert_eq!(inputs.len(), 1);
        let kinds: Vec<_> = inputs[0].contents.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, ["lead", "body"]);
        assert_eq!(inputs[0].lang, "en");
        assert_eq!(inputs[0].source_id, "s");
    }

    /// 公開の本文が無ければ（会員限定の本文しか無ければ）、入力が作れないので選ばない。
    #[test]
    fn pending_digest_ignores_member_only_contents() {
        let db = Db::open_in_memory().unwrap();
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        insert_content(&db, a, Some(aesj));
        assert!(digest_ids(&db, "2026-09-27T00:00:00Z").is_empty());
    }

    /// 抽出の断念は、抽出ステージのキー（backend と model が空）の記録だけで判断する。
    #[test]
    fn pending_digest_checks_extract_failures_by_exact_key() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        db.insert_content(a, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        let other = StageKey {
            article_id: a,
            stage: "extract",
            backend: "chromium",
            model: "",
        };
        db.record_stage_failure(other, "x", t("2026-09-27T00:00:00Z"), true)
            .unwrap();
        assert!(digest_ids(&db, "2026-09-27T00:00:00Z").is_empty());
    }

    #[test]
    fn pending_digest_skips_articles_backing_off_for_this_model() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        db.insert_content(a, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let key = StageKey {
            article_id: a,
            stage: "digest",
            backend: "claude-cli",
            model: "sonnet",
        };
        db.record_stage_failure(key, "bad output", t("2026-09-27T00:00:00Z"), false)
            .unwrap();
        assert!(digest_ids(&db, "2026-09-27T00:30:00Z").is_empty());
        assert_eq!(digest_ids(&db, "2026-09-27T01:00:00Z"), [a]);
    }

    #[test]
    fn saves_and_replaces_profiles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        assert_eq!(db.load_profile(owner).unwrap(), None);
        let mut p = crate::profile::parse(include_str!("../../examples/profile.toml")).unwrap();
        db.save_profile(owner, &p, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let (loaded, hash) = db.load_profile(owner).unwrap().unwrap();
        assert_eq!(loaded, p);
        assert_eq!(hash, crate::profile::hash(&p));

        p.exclude.push("医療".into());
        db.save_profile(owner, &p, t("2026-09-28T00:00:00Z"))
            .unwrap();
        let (loaded, new_hash) = db.load_profile(owner).unwrap().unwrap();
        assert_eq!(loaded.exclude.last().map(String::as_str), Some("医療"));
        assert_ne!(new_hash, hash);
        assert_eq!(db.query_i64("SELECT count(*) FROM profiles").unwrap(), 1);
    }

    fn add_digest(
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
            },
            t(at),
        )
        .unwrap()
    }

    fn score_key(db: &Db) -> ScoreKey<'static> {
        ScoreKey {
            user_id: db.owner_id().unwrap(),
            profile_hash: "h1",
            backend: "claude-cli",
            model: "sonnet",
        }
    }

    fn score_ids(db: &Db, key: ScoreKey, now: &str) -> Vec<i64> {
        db.pending_score(key, t("2026-09-10T00:00:00Z"), t(now), 10)
            .unwrap()
            .into_iter()
            .map(|s| s.article_id)
            .collect()
    }

    #[test]
    fn pending_score_uses_latest_relevant_digest_without_score() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let now = "2026-09-27T00:00:00Z";
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(&db, a, "haiku", "古い版", true, "2026-09-26T01:00:00Z");
        let latest = add_digest(&db, a, "sonnet", "新しい版", true, "2026-09-26T02:00:00Z");
        let unrelated = page_article(&db, "https://e.com/u", "2026-09-25T00:00:00.000Z");
        add_digest(
            &db,
            unrelated,
            "sonnet",
            "非軽水炉",
            false,
            "2026-09-26T02:00:00Z",
        );
        let old = page_article(&db, "https://e.com/old", "2026-09-01T00:00:00.000Z");
        add_digest(&db, old, "sonnet", "期間外", true, "2026-09-26T02:00:00Z");

        let pending = db
            .pending_score(key, t("2026-09-10T00:00:00Z"), t(now), 10)
            .unwrap();
        assert_eq!(
            pending,
            [ScoreInput {
                article_id: a,
                artifact_id: latest,
                title_ja: "新しい版".into(),
                summary_ja: "新しい版の要約".into(),
                topics: vec!["規制・審査".into()],
            }]
        );

        db.insert_score(key, latest, 80, Some("規制に直結"), t(now))
            .unwrap();
        assert!(score_ids(&db, key, now).is_empty());
        // プロファイルが変われば採点し直しの対象になる
        let changed = ScoreKey {
            profile_hash: "h2",
            ..key
        };
        assert_eq!(score_ids(&db, changed, now), [a]);
    }

    #[test]
    fn pending_score_hides_digests_the_user_cannot_view() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let gated = insert_content(&db, a, Some(aesj));
        let payload = serde_json::json!({
            "title_ja": "会員限定", "summary_ja": "s", "points_ja": ["p"],
            "implications_ja": "", "lwr_relevant": true, "topics": ["t"],
        });
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &payload,
                inputs: &[gated],
            },
            t("2026-09-26T01:00:00Z"),
        )
        .unwrap();
        assert!(score_ids(&db, key, "2026-09-27T00:00:00Z").is_empty());
        db.conn()
            .execute(
                "INSERT INTO user_memberships VALUES (?1, ?2)",
                [key.user_id, aesj],
            )
            .unwrap();
        assert_eq!(score_ids(&db, key, "2026-09-27T00:00:00Z"), [a]);
    }

    /// 採点の失敗は利用者とプロファイルごと。あるプロファイルの失敗が、別のプロファイルを止めない。
    #[test]
    fn score_failures_are_scoped_to_user_and_profile() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(&db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        let now = "2026-09-27T00:00:00Z";
        db.record_stage_failure(
            StageKey {
                article_id: a,
                stage: &score_stage(key),
                backend: key.backend,
                model: key.model,
            },
            "bad output",
            t(now),
            true,
        )
        .unwrap();
        assert!(score_ids(&db, key, now).is_empty());
        let other_profile = ScoreKey {
            profile_hash: "h2",
            ..key
        };
        assert_eq!(score_ids(&db, other_profile, now), [a]);
    }

    /// 見出しは、利用者が閲覧できる digest からだけ取る。
    #[test]
    fn recent_signals_use_viewable_digests_only() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(
            &db,
            a,
            "sonnet",
            "公開の見出し",
            true,
            "2026-09-26T01:00:00Z",
        );
        let gated = insert_content(&db, a, Some(aesj));
        let payload = serde_json::json!({
            "title_ja": "会員限定の見出し", "summary_ja": "s", "points_ja": ["p"],
            "implications_ja": "", "lwr_relevant": true, "topics": ["t"],
        });
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "opus",
                prompt_version: 1,
                payload: &payload,
                inputs: &[gated],
            },
            t("2026-09-26T02:00:00Z"),
        )
        .unwrap();
        db.record_event(owner, a, SignalKind::Up, t("2026-09-27T01:00:00Z"))
            .unwrap();
        assert_eq!(
            db.recent_signals(owner, 10).unwrap()[0].title_ja,
            "公開の見出し"
        );
    }

    #[test]
    fn score_is_limited_to_0_through_100() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let d = add_digest(&db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        assert!(
            db.insert_score(key, d, 101, None, t("2026-09-27T00:00:00Z"))
                .is_err()
        );
    }

    #[test]
    fn recent_signals_are_newest_first_with_titles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(&db, a, "sonnet", "記事A", true, "2026-09-26T01:00:00Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        add_digest(&db, b, "sonnet", "記事B", true, "2026-09-26T01:00:00Z");
        let no_digest = page_article(&db, "https://e.com/c", "2026-09-26T00:00:00.000Z");
        db.record_event(owner, a, SignalKind::OpenDetail, t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.record_event(owner, b, SignalKind::Down, t("2026-09-27T02:00:00Z"))
            .unwrap();
        db.record_event(owner, no_digest, SignalKind::Up, t("2026-09-27T03:00:00Z"))
            .unwrap();
        db.record_event(owner, a, SignalKind::Up, t("2026-09-27T04:00:00Z"))
            .unwrap();
        assert_eq!(
            db.recent_signals(owner, 10).unwrap(),
            [
                Signal {
                    kind: SignalKind::Up,
                    title_ja: "記事A".into()
                },
                Signal {
                    kind: SignalKind::Down,
                    title_ja: "記事B".into()
                },
                Signal {
                    kind: SignalKind::OpenDetail,
                    title_ja: "記事A".into()
                },
            ]
        );
        assert_eq!(db.recent_signals(owner, 1).unwrap().len(), 1);
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
