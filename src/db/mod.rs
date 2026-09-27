use std::path::Path;

use rusqlite::Connection;

use crate::config::Lang;

mod articles;
mod artifacts;
mod feedback;
mod redo;
mod score;
mod sources;
mod stages;
#[cfg(test)]
mod test_support;
mod translate;

pub use articles::*;
pub use artifacts::*;
pub use feedback::*;
pub use redo::*;
pub use score::*;
pub use sources::*;
pub use stages::*;
pub use translate::*;

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

/// 指摘の種類。訳語の指摘は気になった訳を、ほかの種類は内容を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    /// 訳語
    Term,
    /// 和訳の誤り
    Translation,
    /// 要約の誤り
    Digest,
    /// トピック
    Topic,
    /// 本文の取得漏れ
    Body,
    /// その他
    Other,
}

impl ReportKind {
    pub const ALL: [ReportKind; 6] = [
        ReportKind::Term,
        ReportKind::Translation,
        ReportKind::Digest,
        ReportKind::Topic,
        ReportKind::Body,
        ReportKind::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReportKind::Term => "term",
            ReportKind::Translation => "translation",
            ReportKind::Digest => "digest",
            ReportKind::Topic => "topic",
            ReportKind::Body => "body",
            ReportKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// 送られた指摘。訳語の指摘は `found`（気になった訳）が必須で、ほかは分からなければ `None`。
/// ほかの種類は内容（`body`）だけを持つ。
#[derive(Debug, Clone, Copy)]
pub enum NewReport<'a> {
    Term {
        found: &'a str,
        wanted: Option<&'a str>,
        source: Option<&'a str>,
        note: Option<&'a str>,
    },
    Other {
        kind: ReportKind,
        body: &'a str,
    },
}

/// 指摘の対応状況。状況を変えた時刻を対応日時にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStatus {
    /// まだ対応していない
    Pending,
    /// 訳語集に反映した（訳語の指摘だけ）
    Added,
    /// 訳語集にあったのに、その訳が使われていなかった（訳語の指摘だけ）
    Existing,
    /// 対応した（訳語以外の指摘）
    Done,
    /// 今のままでよい
    Rejected,
}

impl ReportStatus {
    pub const ALL: [ReportStatus; 5] = [
        ReportStatus::Pending,
        ReportStatus::Added,
        ReportStatus::Existing,
        ReportStatus::Done,
        ReportStatus::Rejected,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReportStatus::Pending => "pending",
            ReportStatus::Added => "added",
            ReportStatus::Existing => "existing",
            ReportStatus::Done => "done",
            ReportStatus::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }

    /// その種類の指摘に付けられる状況。
    pub fn for_kind(kind: ReportKind) -> &'static [ReportStatus] {
        use ReportStatus::*;
        match kind {
            ReportKind::Term => &[Pending, Added, Existing, Rejected],
            _ => &[Pending, Done, Rejected],
        }
    }
}

/// 受付箱の 1 件。訳語の指摘は `found` を、ほかの種類は内容を `note` に持つ。
/// `term` は結び付けた訳語（id と訳。訳語の指摘だけ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub id: i64,
    pub article_id: i64,
    /// 閲覧者が見られる最新の要約の見出し（無ければ原題）
    pub article_title: String,
    pub kind: ReportKind,
    pub found: Option<String>,
    pub wanted: Option<String>,
    pub source: Option<String>,
    pub note: Option<String>,
    pub status: ReportStatus,
    pub term: Option<(i64, String)>,
    pub reply: Option<String>,
    pub reported_at: String,
    pub resolved_at: Option<String>,
}

/// 受付箱の絞り込み。`None` は絞らない。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportFilter {
    pub status: Option<ReportStatus>,
    pub kind: Option<ReportKind>,
    pub article_id: Option<i64>,
}

/// コメントの公開範囲。公開はほかの利用者にも見せ、非公開は書いた本人だけが見る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Visibility::Public, Visibility::Private]
            .into_iter()
            .find(|x| x.as_str() == s)
    }
}

/// 記事へのコメント。`mine` は閲覧者が書いたもの（直せる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub visibility: Visibility,
    pub mine: bool,
    pub created_at: String,
    pub updated_at: String,
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

/// 一覧の 1 行。digest は利用者が閲覧できる最新の版。
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub article_id: i64,
    pub source_id: String,
    pub url: String,
    pub title: String,
    pub lang: String,
    /// 公開日時（無ければ取得日時）
    pub at: String,
    pub fetched_at: String,
    pub title_ja: Option<String>,
    pub summary_ja: Option<String>,
    pub lwr_relevant: Option<bool>,
    /// 現在のプロファイルでの点数（最新の digest に付いたもの）
    pub score: Option<u8>,
    pub reason: Option<String>,
    /// 詳細か和訳を開いたことがある
    pub read: bool,
    pub feedback: Option<Feedback>,
    pub bookmarked: bool,
    pub has_translation: bool,
    pub translation_requested: bool,
    /// 原文を読むのに必要で、利用者が持っていない会員資格の名前（🔒 の表示用）
    pub locked_by: Vec<String>,
}

/// 一覧の条件。
#[derive(Debug, Clone, Copy)]
pub struct ListQuery<'a> {
    pub user_id: i64,
    pub profile_hash: Option<&'a str>,
    /// `show_all` でないときに表示する最低点
    pub min_score: u8,
    /// これ以降に公開（無ければ取得）された記事
    pub since: chrono::DateTime<chrono::Utc>,
    /// 👎、見ない、閾値未満、未採点、非軽水炉の記事も表示する
    pub show_all: bool,
    pub limit: usize,
}

/// 検索の条件。指定しなかった条件（空・None・false）では絞らない。
#[derive(Debug, Clone, Default)]
pub struct SearchQuery<'a> {
    pub user_id: i64,
    pub profile_hash: Option<&'a str>,
    /// 原題・本文・要約・和訳のどれかに含む語。すべてを含む記事に絞る
    pub terms: Vec<String>,
    /// これ以降に公開（無ければ取得）された記事
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// これより前に公開（無ければ取得）された記事
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    /// 閲覧できる最新の要約に付いている語（別名でもよい）。すべてが付いている記事に絞る
    pub topics: Vec<String>,
    /// ソースの ID。どれかのソースの記事に絞る
    pub sources: Vec<String>,
    pub lang: Option<Lang>,
    /// 閲覧できる和訳がある
    pub translated: bool,
    /// 最新の評価が 👍
    pub liked: bool,
    /// 詳細も和訳も開いていない
    pub unread: bool,
    /// ブックマークしている
    pub bookmarked: bool,
    /// この点数以上（未採点は除く）
    pub min_score: Option<u8>,
    /// 一覧の既定と同じく、👎・見ない・非軽水炉・未採点・この点数未満を隠す
    pub hide_below: Option<u8>,
    pub order: SearchOrder,
    pub limit: usize,
}

/// 検索結果の並び。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchOrder {
    /// 新しい順
    #[default]
    Newest,
    /// 一覧と同じく点数の高い順（未採点は後ろ）、同点なら新しい順
    Score,
}

/// 語彙の語と、その使われ方（語彙の整理に使う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicUsage {
    pub name: String,
    pub facet: crate::topics::Facet,
    /// 要約が提案して語彙に加えた時刻。初期語彙と `topics import` で入れた語は None
    pub added_at: Option<String>,
    /// この語が付いている要約の版の数
    pub uses: i64,
}

/// 語彙の統合：`from` の語を `into` にまとめ、`from` は以後 `into` の別名として扱う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicMerge {
    pub from: String,
    pub into: String,
}

/// 成果物の 1 版。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactVersion {
    pub id: i64,
    pub backend: String,
    pub model: String,
    pub prompt_version: i64,
    pub created_at: String,
    pub payload: serde_json::Value,
}

/// 詳細画面の内容。版は新しい順で、利用者が閲覧できるものだけ。
#[derive(Debug, Clone, PartialEq)]
pub struct ArticleDetail {
    pub item: ListItem,
    pub digests: Vec<ArtifactVersion>,
    pub translations: Vec<ArtifactVersion>,
    /// 和訳の依頼に使える公開の本文がある
    pub has_body: bool,
}

impl ArticleDetail {
    /// 和訳を依頼できる（`pending_translate` が拾える）記事：公開の本文がある英語の記事。
    pub fn can_request_translation(&self) -> bool {
        self.item.lang == "en" && self.has_body
    }
}

/// 画面の上部に出す警告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// 最後の取得が失敗している（最後の成功より新しい失敗がある）ソース
    SourceFailing {
        source_id: String,
        error: String,
        at: String,
    },
    /// 直近の LLM の呼び出しが失敗している
    LlmFailed { error: String, at: String },
}

/// `query_items` の範囲：1 件（詳細）か、条件つきの一覧か、検索。
enum ItemScope<'a> {
    One(i64),
    List {
        since: chrono::DateTime<chrono::Utc>,
        show_all: bool,
        min_score: u8,
        limit: usize,
    },
    Search(&'a SearchQuery<'a>),
}

/// 検索の条件を、`query_items` の SQL に足す条件とその名前付きパラメータにしたもの。
/// `items` は記事（`a`）の条件、`rows` は組み立てた行（`rows`、点数 `s`）の条件で、どちらも `AND` で始まる。
#[derive(Default)]
struct SearchFilters {
    items: String,
    rows: String,
    params: Vec<(String, Box<dyn rusqlite::ToSql>)>,
}

impl SearchFilters {
    fn new(q: &SearchQuery) -> Self {
        let mut f = Self::default();
        for (i, term) in q.terms.iter().enumerate() {
            let param = format!(":t{i}");
            f.items
                .push_str(&format!(" AND {}", search_term_filter(term, &param)));
            let value = if is_indexable(term) {
                fts_phrase(term)
            } else {
                like_pattern(term)
            };
            f.params.push((param, Box::new(value)));
        }
        if let Some(until) = q.until {
            f.items
                .push_str(" AND coalesce(a.published_at, a.fetched_at) < :until");
            f.params.push((":until".into(), Box::new(timestamp(until))));
        }
        if !q.sources.is_empty() {
            f.items
                .push_str(" AND a.source_id IN (SELECT value FROM json_each(:sources))");
            let sources = serde_json::to_string(&q.sources).expect("strings serialize");
            f.params.push((":sources".into(), Box::new(sources)));
        }
        if let Some(lang) = q.lang {
            f.items.push_str(" AND a.lang = :lang");
            f.params.push((":lang".into(), Box::new(lang_code(lang))));
        }
        // 語は別名でもよい（統合先の語で判定する）。語彙に無い名前は何にも一致しない
        for (i, topic) in q.topics.iter().enumerate() {
            let param = format!(":topic{i}");
            f.rows.push_str(&format!(
                " AND EXISTS (
                   SELECT 1 FROM artifact_topics AS at
                   WHERE at.artifact_id = rows.digest_id
                     AND at.topic_id IN (
                       SELECT id FROM topics WHERE name = {param}
                       UNION ALL
                       SELECT topic_id FROM topic_aliases WHERE alias = {param}))"
            ));
            f.params.push((param, Box::new(topic.clone())));
        }
        if q.translated {
            f.rows.push_str(" AND rows.has_translation = 1");
        }
        if q.liked {
            f.rows.push_str(" AND rows.feedback = 'up'");
        }
        if q.unread {
            f.rows.push_str(" AND rows.read = 0");
        }
        if q.bookmarked {
            f.rows.push_str(" AND rows.bookmarked = 1");
        }
        if let Some(min) = q.min_score {
            f.rows.push_str(" AND s.score >= :min_score");
            f.params.push((":min_score".into(), Box::new(min)));
        }
        f
    }
}

fn lang_code(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        Lang::Ja => "ja",
    }
}

/// trigram の索引で引ける語の最短の文字数。これより短い語は本文を走査する。
const TRIGRAM_MIN_CHARS: usize = 3;

/// 検索の語 `param`（`:t0` など）を含み、利用者（`:user`）が閲覧できる文書のある記事に絞る条件。
/// 短い語は索引を使えないので LIKE で走査する（本文 500MB で 0.2 秒ほど）。
fn search_term_filter(term: &str, param: &str) -> String {
    let matches = if is_indexable(term) {
        format!("d.text MATCH {param}")
    } else {
        format!("d.text LIKE {param} ESCAPE '\\'")
    };
    format!(
        "a.id IN (
           SELECT d.article_id FROM search_docs AS d
           WHERE {matches}
             AND (d.content_id IS NULL OR d.content_id IN (
               SELECT c.id FROM contents AS c
               WHERE c.access_membership_id IS NULL
                  OR c.access_membership_id IN (
                    SELECT membership_id FROM user_memberships WHERE user_id = :user)))
             AND (d.artifact_id IS NULL OR d.artifact_id IN (
               SELECT r.id FROM artifacts AS r WHERE {viewable_r})))",
        viewable_r = viewable("r"),
    )
}

fn is_indexable(term: &str) -> bool {
    term.chars().count() >= TRIGRAM_MIN_CHARS
}

/// 語を、その語を含む文字列に一致する LIKE のパターンにする。`%` `_` `\` は文字どおりに扱う。
fn like_pattern(term: &str) -> String {
    let escaped = term
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// 語を FTS5 のフレーズにする。構文として解釈させないよう全体を `"` で囲み、中の `"` は二重にする。
fn fts_phrase(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

/// 別名 `alias` の要約に付いている語の名前（語彙の登録順の JSON 配列）。統合を反映するので、
/// payload の `topics`（LLM が出した名前のまま）ではなくこちらを見せる。
fn linked_topics(alias: &str) -> String {
    format!(
        "(SELECT json_group_array(name) FROM (
           SELECT t.name FROM artifact_topics AS at
           JOIN topics AS t ON t.id = at.topic_id
           WHERE at.artifact_id = {alias}.id
           ORDER BY t.id))"
    )
}

/// 別名 `alias` の成果物を、利用者（`:user`）が閲覧できる条件。
fn viewable(alias: &str) -> String {
    format!(
        "NOT EXISTS (
           SELECT 1 FROM artifact_access AS aa
           WHERE aa.artifact_id = {alias}.id
             AND aa.membership_id NOT IN (
               SELECT membership_id FROM user_memberships WHERE user_id = :user))"
    )
}

/// 入力に使う本文の部分の範囲。
#[derive(Debug, Clone, Copy)]
enum ContentSet {
    All,
    Body,
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

    /// トピックの語彙（登録順）。
    pub fn topics(&self) -> Result<Vec<crate::topics::Topic>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, facet FROM topics ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|row| {
            let (name, facet) = row?;
            let facet = crate::topics::Facet::parse(&facet)
                .ok_or_else(|| DbError::UnexpectedValue(format!("topic facet {facet:?}")))?;
            Ok(crate::topics::Topic { name, facet })
        })
        .collect()
    }

    /// 訳語集（登録順）。原語も登録順に並べる。
    pub fn glossary_entries(&self) -> Result<Vec<crate::glossary::Entry>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.target, t.abbr, t.note, t.changed_at, s.source, s.added_at
             FROM glossary_terms AS t JOIN glossary_sources AS s ON s.term_id = t.id
             ORDER BY t.id, s.rowid",
        )?;
        let mut rows = stmt.query([])?;
        let mut entries: Vec<crate::glossary::Entry> = Vec::new();
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let source: String = r.get(5)?;
            let added_at: Option<String> = r.get(6)?;
            match entries.last_mut() {
                Some(entry) if entry.id == id => {
                    entry.term.sources.push(source);
                    entry.sources_added_at.push(added_at);
                }
                _ => entries.push(crate::glossary::Entry {
                    id,
                    term: crate::glossary::Term {
                        sources: vec![source],
                        target: r.get(1)?,
                        abbr: r.get(2)?,
                        note: r.get(3)?,
                    },
                    term_changed_at: r.get(4)?,
                    sources_added_at: vec![added_at],
                }),
            }
        }
        Ok(entries)
    }

    /// 訳語を加えて id を返す。訳語・略語・原語がほかの訳語のものと重なれば、何も変えずに失敗する。
    pub fn add_glossary_term(
        &self,
        term: &crate::glossary::Term,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        check_glossary_conflicts(&tx, None, term)?;
        tx.execute(
            "INSERT INTO glossary_terms (target, abbr, note, changed_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![term.target, term.abbr, term.note, timestamp(now)],
        )?;
        let id = tx.last_insert_rowid();
        replace_glossary_sources(&tx, id, &term.sources, now)?;
        tx.commit()?;
        Ok(id)
    }

    /// 訳語を置き換える。無ければ false。訳語・略語・原語がほかの訳語のものと重なれば、
    /// 何も変えずに失敗する。残した原語は加えた時刻を保ち、訳・略語・メモは変わったときだけ時刻を進める。
    pub fn update_glossary_term(
        &self,
        id: i64,
        term: &crate::glossary::Term,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        use rusqlite::OptionalExtension;
        let tx = self.conn.unchecked_transaction()?;
        let exists = tx
            .query_row("SELECT 1 FROM glossary_terms WHERE id = ?1", [id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        check_glossary_conflicts(&tx, Some(id), term)?;
        tx.execute(
            "UPDATE glossary_terms SET target = ?2, abbr = ?3, note = ?4, changed_at = ?5
             WHERE id = ?1
               AND (target IS NOT ?2 OR abbr IS NOT ?3 OR note IS NOT ?4)",
            rusqlite::params![id, term.target, term.abbr, term.note, timestamp(now)],
        )?;
        replace_glossary_sources(&tx, id, &term.sources, now)?;
        tx.commit()?;
        Ok(true)
    }

    /// 訳語と原語を消す。無ければ false。
    pub fn delete_glossary_term(&self, id: i64) -> Result<bool, DbError> {
        Ok(self
            .conn
            .execute("DELETE FROM glossary_terms WHERE id = ?1", [id])?
            > 0)
    }

    /// 書き出す語彙（登録順）。LLM が足した語は追加した時刻を持つ。
    pub fn vocabulary(&self) -> Result<Vec<crate::topics::Entry>, DbError> {
        Ok(self
            .topic_usage()?
            .into_iter()
            .map(|u| crate::topics::Entry {
                name: u.name,
                facet: u.facet,
                added_at: u.added_at,
            })
            .collect())
    }

    /// 語彙を `topics` に置き換える。名前で突き合わせ、無い語は追加、軸が変わった語は更新し、
    /// 並びに無い語は削除する。要約に付いている語を消そうとしたら何も変えずに失敗する。
    pub fn replace_topics(&self, topics: &[crate::topics::Entry]) -> Result<(), DbError> {
        let names = serde_json::to_string(&topics.iter().map(|t| &t.name).collect::<Vec<_>>())?;
        let tx = self.conn.unchecked_transaction()?;
        let in_use: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT t.name FROM topics AS t
                 JOIN artifact_topics AS at ON at.topic_id = t.id
                 WHERE t.name NOT IN (SELECT value FROM json_each(?1))
                 ORDER BY t.name",
            )?;
            stmt.query_map([&names], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        if !in_use.is_empty() {
            return Err(DbError::TopicsInUse(in_use));
        }
        tx.execute(
            "DELETE FROM topics WHERE name NOT IN (SELECT value FROM json_each(?1))",
            [&names],
        )?;
        for t in topics {
            // LLM が足した語かどうか（added_at）も語彙ファイルに従う
            tx.execute(
                "INSERT INTO topics (name, facet, added_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (name) DO UPDATE SET facet = excluded.facet, added_at = excluded.added_at",
                rusqlite::params![t.name, t.facet.as_str(), t.added_at],
            )?;
        }
        // 語として取り込んだ名前は、別名ではなくその語を指すようにする
        tx.execute(
            "DELETE FROM topic_aliases WHERE alias IN (SELECT value FROM json_each(?1))",
            [&names],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 語彙の語と使われ方（登録順）。
    pub fn topic_usage(&self) -> Result<Vec<TopicUsage>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name, t.facet, t.added_at,
                    (SELECT count(*) FROM artifact_topics AS at WHERE at.topic_id = t.id)
             FROM topics AS t ORDER BY t.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get(2)?,
                r.get(3)?,
            ))
        })?;
        rows.map(|row| {
            let (name, facet, added_at, uses) = row?;
            let facet = crate::topics::Facet::parse(&facet)
                .ok_or_else(|| DbError::UnexpectedValue(format!("topic facet {facet:?}")))?;
            Ok(TopicUsage {
                name,
                facet,
                added_at,
                uses,
            })
        })
        .collect()
    }

    /// 語を統合する。要約への付与を統合先に付け替え、統合元の名前を別名として記録し、統合元を消す。
    /// 統合元を指していた別名も統合先に付け替える。どれか 1 つでも失敗したら何も変えない。
    /// `backend` と `model` は統合を決めた LLM（別名の記録に残す）。
    pub fn merge_topics(
        &self,
        merges: &[TopicMerge],
        backend: &str,
        model: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        use rusqlite::OptionalExtension;
        let tx = self.conn.unchecked_transaction()?;
        let topic_id = |name: &str| -> Result<i64, DbError> {
            tx.query_row("SELECT id FROM topics WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| DbError::UnknownTopic(name.to_string()))
        };
        for m in merges {
            if m.from == m.into {
                return Err(DbError::SelfMerge(m.from.clone()));
            }
            let from = topic_id(&m.from)?;
            let into = topic_id(&m.into)?;
            // 両方が付いている要約は、統合先の付与を残す
            tx.execute(
                "UPDATE OR IGNORE artifact_topics SET topic_id = ?2 WHERE topic_id = ?1",
                [from, into],
            )?;
            tx.execute("DELETE FROM artifact_topics WHERE topic_id = ?1", [from])?;
            tx.execute(
                "UPDATE topic_aliases SET topic_id = ?2 WHERE topic_id = ?1",
                [from, into],
            )?;
            tx.execute(
                "INSERT INTO topic_aliases (alias, topic_id, merged_at, backend, model)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![m.from, into, timestamp(now), backend, model],
            )?;
            tx.execute("DELETE FROM topics WHERE id = ?1", [from])?;
        }
        tx.commit()?;
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

    /// 記事へのコメント（古い順）。利用者 `user_id` が書いたものと、ほかの利用者の公開のもの。
    pub fn comments(&self, user_id: i64, article_id: i64) -> Result<Vec<Comment>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, body, visibility, user_id = ?1, created_at, updated_at
             FROM comments
             WHERE article_id = ?2 AND (user_id = ?1 OR visibility = 'public')
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([user_id, article_id], |r| {
            Ok((
                r.get::<_, String>(2)?,
                Comment {
                    id: r.get(0)?,
                    body: r.get(1)?,
                    visibility: Visibility::Private,
                    mine: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                },
            ))
        })?;
        rows.map(|row| {
            let (visibility, comment) = row?;
            let visibility = Visibility::parse(&visibility).ok_or_else(|| {
                DbError::UnexpectedValue(format!("comment visibility {visibility:?}"))
            })?;
            Ok(Comment {
                visibility,
                ..comment
            })
        })
        .collect()
    }

    /// コメントを書いて id を返す。
    pub fn add_comment(
        &self,
        user_id: i64,
        article_id: i64,
        body: &str,
        visibility: Visibility,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        self.conn.execute(
            "INSERT INTO comments (user_id, article_id, body, visibility, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            rusqlite::params![
                user_id,
                article_id,
                body,
                visibility.as_str(),
                timestamp(now)
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 自分のコメントを直し、その記事の id を返す。無いか他人のものなら None。
    pub fn update_comment(
        &self,
        user_id: i64,
        id: i64,
        body: &str,
        visibility: Visibility,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<i64>, DbError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "UPDATE comments SET body = ?3, visibility = ?4, updated_at = ?5
                 WHERE id = ?1 AND user_id = ?2
                 RETURNING article_id",
                rusqlite::params![id, user_id, body, visibility.as_str(), timestamp(now)],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 自分のコメントを消し、その記事の id を返す。無いか他人のものなら None。
    pub fn delete_comment(&self, user_id: i64, id: i64) -> Result<Option<i64>, DbError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "DELETE FROM comments WHERE id = ?1 AND user_id = ?2 RETURNING article_id",
                [id, user_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 指摘を受付箱に入れる。
    pub fn add_report(
        &self,
        user_id: i64,
        article_id: i64,
        report: &NewReport<'_>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let (kind, found, wanted, source, note) = match *report {
            NewReport::Term {
                found,
                wanted,
                source,
                note,
            } => (ReportKind::Term, Some(found), wanted, source, note),
            NewReport::Other { kind, body } => (kind, None, None, None, Some(body)),
        };
        self.conn.execute(
            "INSERT INTO reports
               (user_id, article_id, kind, found, wanted, source, note, reported_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                user_id,
                article_id,
                kind.as_str(),
                found,
                wanted,
                source,
                note,
                timestamp(now),
            ],
        )?;
        Ok(())
    }

    /// 受付箱（新しい順）。記事の見出しは、利用者 `user_id` が閲覧できる最新の要約から取る
    /// （無ければ原題）。
    pub fn reports(&self, user_id: i64, filter: &ReportFilter) -> Result<Vec<Report>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT r.id, r.article_id,
                    coalesce(nullif(trim((SELECT d.title_ja FROM artifacts AS d
                                          WHERE d.article_id = a.id AND d.kind = 'digest'
                                            AND {viewable}
                                          ORDER BY d.created_at DESC, d.id DESC LIMIT 1)), ''),
                             a.title),
                    r.kind, r.found, r.wanted, r.source, r.note, r.status, r.term_id, t.target,
                    r.reply, r.reported_at, r.resolved_at
             FROM reports AS r
             JOIN articles AS a ON a.id = r.article_id
             LEFT JOIN glossary_terms AS t ON t.id = r.term_id
             WHERE (:status IS NULL OR r.status = :status)
               AND (:kind IS NULL OR r.kind = :kind)
               AND (:article IS NULL OR r.article_id = :article)
             ORDER BY r.reported_at DESC, r.id DESC",
            viewable = viewable("d")
        ))?;
        let mut rows = stmt.query(rusqlite::named_params! {
            ":user": user_id,
            ":status": filter.status.map(ReportStatus::as_str),
            ":kind": filter.kind.map(ReportKind::as_str),
            ":article": filter.article_id,
        })?;
        let mut reports = Vec::new();
        while let Some(r) = rows.next()? {
            let kind: String = r.get(3)?;
            let status: String = r.get(8)?;
            let term = match (r.get::<_, Option<i64>>(9)?, r.get::<_, Option<String>>(10)?) {
                (Some(id), Some(target)) => Some((id, target)),
                _ => None,
            };
            reports.push(Report {
                id: r.get(0)?,
                article_id: r.get(1)?,
                article_title: r.get(2)?,
                kind: ReportKind::parse(&kind)
                    .ok_or_else(|| DbError::UnexpectedValue(format!("report kind {kind:?}")))?,
                found: r.get(4)?,
                wanted: r.get(5)?,
                source: r.get(6)?,
                note: r.get(7)?,
                status: ReportStatus::parse(&status)
                    .ok_or_else(|| DbError::UnexpectedValue(format!("report status {status:?}")))?,
                term,
                reply: r.get(11)?,
                reported_at: r.get(12)?,
                resolved_at: r.get(13)?,
            });
        }
        Ok(reports)
    }

    /// 指摘の種類。無ければ None。
    pub fn report_kind(&self, id: i64) -> Result<Option<ReportKind>, DbError> {
        use rusqlite::OptionalExtension;
        let kind: Option<String> = self
            .conn
            .query_row("SELECT kind FROM reports WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        kind.map(|k| {
            ReportKind::parse(&k)
                .ok_or_else(|| DbError::UnexpectedValue(format!("report kind {k:?}")))
        })
        .transpose()
    }

    /// 対応状況ごとの件数（`ReportStatus::ALL` の順。0 件も含む）。
    pub fn report_counts(&self) -> Result<Vec<(ReportStatus, i64)>, DbError> {
        ReportStatus::ALL
            .into_iter()
            .map(|status| {
                let n = self.conn.query_row(
                    "SELECT count(*) FROM reports WHERE status = ?1",
                    [status.as_str()],
                    |r| r.get(0),
                )?;
                Ok((status, n))
            })
            .collect()
    }

    /// 指摘の対応状況を変える。無ければ false。対応日時は状況が変わったときだけ `now` にし、
    /// 受付中に戻せば消す。
    pub fn resolve_report(
        &self,
        id: i64,
        status: ReportStatus,
        term_id: Option<i64>,
        reply: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        // 対応日時は状況を変えたときだけ進める（ひとことや訳語だけの修正では変えない）
        Ok(self.conn.execute(
            "UPDATE reports
             SET resolved_at = CASE WHEN ?2 = 'pending' THEN NULL
                                    WHEN status = ?2 THEN resolved_at
                                    ELSE ?5 END,
                 status = ?2, term_id = ?3, reply = ?4
             WHERE id = ?1",
            rusqlite::params![id, status.as_str(), term_id, reply, timestamp(now)],
        )? > 0)
    }

    /// 記事の公開の本文の部分。`All` は概要から本文まで（要約の入力）、`Body` は本文だけ（和訳の入力）。
    fn public_contents(
        &self,
        article_id: i64,
        set: ContentSet,
    ) -> Result<Vec<InputContent>, DbError> {
        let sql = match set {
            ContentSet::All => {
                "SELECT id, kind, text FROM contents
                 WHERE article_id = ?1 AND access_membership_id IS NULL
                 ORDER BY CASE kind WHEN 'lead' THEN 0 WHEN 'abstract' THEN 1
                                    WHEN 'body' THEN 2 ELSE 3 END, id"
            }
            ContentSet::Body => {
                "SELECT id, kind, text FROM contents
                 WHERE article_id = ?1 AND kind IN ('body', 'fulltext')
                   AND access_membership_id IS NULL
                 ORDER BY CASE kind WHEN 'fulltext' THEN 0 ELSE 1 END, id"
            }
        };
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map([article_id], |r| {
            Ok(InputContent {
                id: r.get(0)?,
                kind: r.get(1)?,
                text: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 一覧を見た時刻を記録し、この訪問の区切り（前の訪問で最後に見た時刻。初回は None）を返す。
    /// 最後に見てから `gap` 以内の閲覧は同じ訪問とみなし、区切りを変えない。
    pub fn begin_visit(
        &self,
        user_id: i64,
        now: chrono::DateTime<chrono::Utc>,
        gap: chrono::Duration,
    ) -> Result<Option<String>, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        let (last_seen, boundary): (Option<String>, Option<String>) = tx.query_row(
            "SELECT last_seen_at, visit_boundary_at FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let boundary = match last_seen {
            // 時刻はどれも timestamp() の書式なので、文字列の比較で前後がわかる
            Some(last) if last < timestamp(now - gap) => Some(last),
            Some(_) => boundary,
            None => None,
        };
        tx.execute(
            "UPDATE users SET last_seen_at = ?2, visit_boundary_at = ?3 WHERE id = ?1",
            rusqlite::params![user_id, timestamp(now), boundary],
        )?;
        tx.commit()?;
        Ok(boundary)
    }

    /// 一覧。点数の高い順（未採点は後ろ）、同点なら新しい順。
    pub fn list_articles(&self, q: ListQuery) -> Result<Vec<ListItem>, DbError> {
        self.query_items(
            q.user_id,
            q.profile_hash,
            ItemScope::List {
                since: q.since,
                show_all: q.show_all,
                min_score: q.min_score,
                limit: q.limit,
            },
        )
    }

    /// 検索。条件は `SearchQuery` のとおりで、`hide_below` を指定しなければ一覧で隠す記事も含め、
    /// `order` の順（既定は新しい順）に並べる。
    pub fn search_articles(&self, q: &SearchQuery) -> Result<Vec<ListItem>, DbError> {
        self.query_items(q.user_id, q.profile_hash, ItemScope::Search(q))
    }

    /// 詳細画面の内容。記事が無ければ None。
    pub fn article_detail(
        &self,
        user_id: i64,
        profile_hash: Option<&str>,
        article_id: i64,
    ) -> Result<Option<ArticleDetail>, DbError> {
        let Some(item) = self
            .query_items(user_id, profile_hash, ItemScope::One(article_id))?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let has_body = !self
            .public_contents(article_id, ContentSet::Body)?
            .is_empty();
        Ok(Some(ArticleDetail {
            digests: self.versions(user_id, article_id, ArtifactKind::Digest)?,
            translations: self.versions(user_id, article_id, ArtifactKind::Translation)?,
            item,
            has_body,
        }))
    }

    /// 利用者が閲覧できる版を新しい順に。
    /// 要約の payload の `topics` は、統合を反映した付与の名前に差し替える。
    fn versions(
        &self,
        user_id: i64,
        article_id: i64,
        kind: ArtifactKind,
    ) -> Result<Vec<ArtifactVersion>, DbError> {
        let sql = format!(
            "SELECT r.id, r.backend, r.model, r.prompt_version, r.created_at, r.payload,
                    {linked}
             FROM artifacts AS r
             WHERE r.article_id = :article AND r.kind = :kind AND {viewable}
             ORDER BY r.created_at DESC, r.id DESC",
            linked = linked_topics("r"),
            viewable = viewable("r"),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":article": article_id,
                ":kind": kind.as_str(),
                ":user": user_id,
            },
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (id, backend, model, prompt_version, created_at, payload, topics) = row?;
            let mut payload: serde_json::Value = serde_json::from_str(&payload)?;
            if kind == ArtifactKind::Digest {
                payload["topics"] = serde_json::from_str(&topics)?;
            }
            Ok(ArtifactVersion {
                id,
                backend,
                model,
                prompt_version,
                created_at,
                payload,
            })
        })
        .collect()
    }

    /// 一覧・詳細に共通の行の組み立て。
    fn query_items(
        &self,
        user_id: i64,
        profile_hash: Option<&str>,
        scope: ItemScope,
    ) -> Result<Vec<ListItem>, DbError> {
        const BY_SCORE: &str = "s.score IS NULL, s.score DESC, rows.at DESC, rows.id DESC";
        const NEWEST: &str = "rows.at DESC, rows.id DESC";
        // ブックマークした記事は振り分け済みなので、一覧には（すべて表示でも）出さない
        let list_filter = match scope {
            ItemScope::List { .. } => "AND rows.bookmarked = 0",
            _ => "",
        };
        let (id, since, show_all, min_score, limit, order) = match scope {
            ItemScope::One(id) => (Some(id), None, true, 0, 1, BY_SCORE),
            ItemScope::List {
                since,
                show_all,
                min_score,
                limit,
            } => (None, Some(since), show_all, min_score, limit, BY_SCORE),
            ItemScope::Search(q) => (
                None,
                q.since,
                q.hide_below.is_none(),
                q.hide_below.unwrap_or(0),
                q.limit,
                match q.order {
                    SearchOrder::Newest => NEWEST,
                    SearchOrder::Score => BY_SCORE,
                },
            ),
        };
        let filters = match scope {
            ItemScope::Search(q) => SearchFilters::new(q),
            _ => SearchFilters::default(),
        };
        let SearchFilters {
            items: items_filter,
            rows: rows_filter,
            params: filter_params,
        } = &filters;
        let sql = format!(
            "WITH items AS (
               SELECT a.id, a.source_id, a.url, a.title, a.lang,
                      coalesce(a.published_at, a.fetched_at) AS at, a.fetched_at,
                      (SELECT r.id FROM artifacts AS r
                       WHERE r.article_id = a.id AND r.kind = 'digest' AND {viewable_r}
                       ORDER BY r.created_at DESC, r.id DESC LIMIT 1) AS digest_id
               FROM articles AS a
               WHERE (:id IS NULL OR a.id = :id)
                 AND (:since IS NULL OR coalesce(a.published_at, a.fetched_at) >= :since)
                 {items_filter}
             ),
             rows AS (
               SELECT i.*, d.title_ja, d.summary_ja,
                      json_extract(d.payload, '$.lwr_relevant') AS relevant,
                      (SELECT s.id FROM scores AS s
                       WHERE s.user_id = :user AND s.profile_hash = :profile
                         AND s.artifact_id = i.digest_id
                       -- 複数のモデルの採点があれば、先回り和訳と同じく最高点を使う
                       ORDER BY s.score DESC, s.created_at DESC, s.id DESC LIMIT 1) AS score_id,
                      EXISTS (
                        SELECT 1 FROM events AS e
                        WHERE e.user_id = :user AND e.article_id = i.id
                          AND e.kind IN ('open_detail', 'open_translation')) AS read,
                      (SELECT e.kind FROM events AS e
                       WHERE e.user_id = :user AND e.article_id = i.id AND e.kind IN ('up', 'down')
                       ORDER BY e.created_at DESC, e.id DESC LIMIT 1) AS feedback,
                      EXISTS (
                        SELECT 1 FROM events AS e
                        WHERE e.user_id = :user AND e.article_id = i.id AND e.kind = 'dismiss')
                        AS dismissed,
                      EXISTS (
                        SELECT 1 FROM bookmarks AS b
                        WHERE b.user_id = :user AND b.article_id = i.id) AS bookmarked,
                      EXISTS (
                        SELECT 1 FROM artifacts AS t
                        WHERE t.article_id = i.id AND t.kind = 'translation' AND {viewable_t})
                        AS has_translation,
                      EXISTS (
                        SELECT 1 FROM translation_requests AS tr
                        WHERE tr.user_id = :user AND tr.article_id = i.id AND tr.done_at IS NULL)
                        AS requested,
                      -- 原文を読むのに必要で、利用者が持っていない会員資格の名前（🔒）
                      (SELECT json_group_array(name) FROM (
                         SELECT m.name FROM article_access AS aa
                         JOIN memberships AS m ON m.id = aa.membership_id
                         WHERE aa.article_id = i.id
                           AND aa.membership_id NOT IN (
                             SELECT membership_id FROM user_memberships WHERE user_id = :user)
                         ORDER BY m.name)) AS locked_by
               FROM items AS i
               LEFT JOIN artifacts AS d ON d.id = i.digest_id
             )
             SELECT rows.id, rows.source_id, rows.url, rows.title, rows.lang, rows.at,
                    rows.fetched_at, rows.title_ja, rows.summary_ja, rows.relevant,
                    s.score, s.reason, rows.read, rows.feedback, rows.has_translation,
                    rows.requested, rows.locked_by, rows.bookmarked
             FROM rows
             LEFT JOIN scores AS s ON s.id = rows.score_id
             -- 既定では 👎、見ない、非軽水炉、未採点、閾値未満を隠す
             WHERE (:all = 1
                OR (rows.feedback IS NOT 'down' AND rows.dismissed = 0
                    AND rows.relevant = 1 AND s.score >= :min))
               {rows_filter}
               {list_filter}
             ORDER BY {order}
             LIMIT :limit",
            viewable_r = viewable("r"),
            viewable_t = viewable("t"),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let since = since.map(timestamp);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut params: Vec<(&str, &dyn rusqlite::ToSql)> = vec![
            (":id", &id),
            (":since", &since),
            (":user", &user_id),
            (":profile", &profile_hash),
            (":all", &show_all),
            (":min", &min_score),
            (":limit", &limit),
        ];
        params.extend(
            filter_params
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_ref())),
        );
        let rows = stmt.query_map(params.as_slice(), |r| {
            let feedback: Option<String> = r.get(13)?;
            let item = ListItem {
                article_id: r.get(0)?,
                source_id: r.get(1)?,
                url: r.get(2)?,
                title: r.get(3)?,
                lang: r.get(4)?,
                at: r.get(5)?,
                fetched_at: r.get(6)?,
                title_ja: r.get(7)?,
                summary_ja: r.get(8)?,
                lwr_relevant: r.get(9)?,
                score: r.get(10)?,
                reason: r.get(11)?,
                read: r.get(12)?,
                feedback: match feedback.as_deref() {
                    Some("up") => Some(Feedback::Up),
                    Some("down") => Some(Feedback::Down),
                    _ => None,
                },
                bookmarked: r.get(17)?,
                has_translation: r.get(14)?,
                translation_requested: r.get(15)?,
                locked_by: Vec::new(),
            };
            Ok((item, r.get::<_, String>(16)?))
        })?;
        rows.map(|row| {
            let (mut item, locked_by) = row?;
            item.locked_by = serde_json::from_str(&locked_by)?;
            Ok(item)
        })
        .collect()
    }

    /// 取得に失敗し続けているソースと、`since` 以降の直近の LLM の失敗。
    pub fn warnings(&self, since: chrono::DateTime<chrono::Utc>) -> Result<Vec<Warning>, DbError> {
        use rusqlite::OptionalExtension;
        // 取得に成功するとエラーは消えるので、残っているエラーは今も失敗しているもの
        let mut stmt = self.conn.prepare(
            "SELECT source_id, last_error, last_error_at FROM source_state
             WHERE last_error IS NOT NULL ORDER BY source_id",
        )?;
        let mut warnings = stmt
            .query_map([], |r| {
                Ok(Warning::SourceFailing {
                    source_id: r.get(0)?,
                    error: r.get(1)?,
                    at: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let latest: Option<(bool, Option<String>, String)> = self
            .conn
            .query_row(
                "SELECT ok, error, at FROM llm_calls WHERE at >= ?1
                 ORDER BY at DESC, id DESC LIMIT 1",
                [timestamp(since)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((false, Some(error), at)) = latest {
            warnings.push(Warning::LlmFailed { error, at });
        }
        Ok(warnings)
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

/// 訳語・略語・原語が、`id` 以外の訳語のものと重なっていないか確かめる。
/// 原語は大文字小文字を問わず比べる（`glossary_sources.source` の照合順序）。
fn check_glossary_conflicts(
    conn: &Connection,
    id: Option<i64>,
    term: &crate::glossary::Term,
) -> Result<(), DbError> {
    use rusqlite::OptionalExtension;
    let owner = |sql: &str, value: &str| -> Result<Option<String>, DbError> {
        Ok(conn
            .query_row(sql, rusqlite::params![value, id], |r| r.get(0))
            .optional()?)
    };
    if owner(
        "SELECT target FROM glossary_terms WHERE target = ?1 AND id IS NOT ?2",
        &term.target,
    )?
    .is_some()
    {
        return Err(DbError::GlossaryConflict(format!(
            "訳語「{}」は登録済み",
            term.target
        )));
    }
    if let Some(abbr) = &term.abbr
        && let Some(target) = owner(
            "SELECT target FROM glossary_terms WHERE abbr = ?1 AND id IS NOT ?2",
            abbr,
        )?
    {
        return Err(DbError::GlossaryConflict(format!(
            "略語「{abbr}」は「{target}」で使っている"
        )));
    }
    for source in &term.sources {
        if let Some(target) = owner(
            "SELECT t.target FROM glossary_sources AS s
             JOIN glossary_terms AS t ON t.id = s.term_id
             WHERE s.source = ?1 AND s.term_id IS NOT ?2",
            source,
        )? {
            return Err(DbError::GlossaryConflict(format!(
                "原語「{source}」は「{target}」に登録済み"
            )));
        }
    }
    Ok(())
}

/// 訳語 `id` の原語を `sources` にする。表記の同じ原語は加えた時刻を保ち、
/// 無くした原語は消し、新しい原語（大文字小文字だけ変えたものを含む）は `now` に加えた扱いにする。
fn replace_glossary_sources(
    conn: &Connection,
    id: i64,
    sources: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), DbError> {
    let sources = serde_json::to_string(sources)?;
    conn.execute(
        "DELETE FROM glossary_sources
         WHERE term_id = ?1
           AND source COLLATE BINARY NOT IN (SELECT value FROM json_each(?2))",
        rusqlite::params![id, sources],
    )?;
    conn.execute(
        "INSERT INTO glossary_sources (source, term_id, added_at)
         SELECT value, ?1, ?3 FROM json_each(?2) WHERE true
         ON CONFLICT (source) DO NOTHING",
        rusqlite::params![id, sources, timestamp(now)],
    )?;
    Ok(())
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

    /// 一覧を開くたびに区切りが進むと、再読み込みや詳細からの戻りで「前回から」の記事が
    /// 「それ以前」に移ってしまう。間隔の短い閲覧は同じ訪問とみなし、区切りを保つ。
    #[test]
    fn begin_visit_keeps_boundary_within_a_visit() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let gap = chrono::Duration::minutes(30);
        let visit = |at: &str| db.begin_visit(owner, t(at), gap).unwrap();
        // 初回は区切りが無い（すべて新着）。同じ訪問のうちは無いまま
        assert_eq!(visit("2026-09-27T00:00:00Z"), None);
        assert_eq!(visit("2026-09-27T00:20:00Z"), None);
        // 間が空いたら新しい訪問。区切りは前の訪問で最後に見た時刻
        assert_eq!(
            visit("2026-09-27T12:00:00Z").as_deref(),
            Some("2026-09-27T00:20:00.000Z")
        );
        // 同じ訪問の再読み込みでは区切りを保つ（最後に見た時刻は進む）
        assert_eq!(
            visit("2026-09-27T12:10:00Z").as_deref(),
            Some("2026-09-27T00:20:00.000Z")
        );
        assert_eq!(
            visit("2026-09-27T12:35:00Z").as_deref(),
            Some("2026-09-27T00:20:00.000Z")
        );
        // 最後に見てから gap を超えたら次の訪問
        assert_eq!(
            visit("2026-09-27T13:10:00Z").as_deref(),
            Some("2026-09-27T12:35:00.000Z")
        );
    }

    #[test]
    fn list_orders_by_score_and_hides_unwanted_by_default() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let high = scored_article(
            &db,
            "https://e.com/high",
            Lang::En,
            "2026-09-25T00:00:00.000Z",
            90,
        );
        let mid = scored_article(
            &db,
            "https://e.com/mid",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            70,
        );
        let low = scored_article(
            &db,
            "https://e.com/low",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            30,
        );
        let disliked = scored_article(
            &db,
            "https://e.com/down",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            95,
        );
        db.record_event(owner, disliked, SignalKind::Down, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let unscored = page_article(&db, "https://e.com/new", "2026-09-26T00:00:00.000Z");
        let old = scored_article(
            &db,
            "https://e.com/old",
            Lang::En,
            "2026-09-01T00:00:00.000Z",
            99,
        );
        let _ = old;

        assert_eq!(list_ids(&db, false), [high, mid]);
        let all = list_ids(&db, true);
        assert_eq!(&all[..4], [disliked, high, mid, low]);
        assert_eq!(all[4], unscored);
        assert_eq!(all.len(), 5, "old articles stay hidden");

        let items = db.list_articles(list_query(&db, true)).unwrap();
        let d = items.iter().find(|i| i.article_id == disliked).unwrap();
        assert_eq!(d.feedback, Some(Feedback::Down));
        let h = items.iter().find(|i| i.article_id == high).unwrap();
        assert_eq!(
            (h.score, h.title_ja.as_deref(), h.read),
            (Some(90), Some("題"), false)
        );
    }

    #[test]
    fn list_marks_read_translation_and_locks() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.record_event(owner, a, SignalKind::OpenDetail, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.request_translation(owner, a, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        db.conn()
            .execute("INSERT INTO article_access VALUES (?1, ?2)", [a, aesj])
            .unwrap();
        let item = &db.list_articles(list_query(&db, false)).unwrap()[0];
        assert!(item.read);
        assert!(item.translation_requested);
        assert!(!item.has_translation);
        assert_eq!(item.locked_by, ["日本原子力学会"]);
    }

    #[test]
    fn article_detail_lists_viewable_versions_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        add_digest(&db, a, "opus", "新版", true, "2026-09-26T05:00:00Z");
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let gated = insert_content(&db, a, Some(aesj));
        let payload = serde_json::json!({"title_ja": "会員限定", "summary_ja": "s", "points_ja": ["p"],
            "implications_ja": "", "lwr_relevant": true, "topics": ["燃料"]});
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "fable",
                prompt_version: 1,
                payload: &payload,
                inputs: &[gated],
                glossary_at: None,
            },
            t("2026-09-26T09:00:00Z"),
        )
        .unwrap();
        let detail = db.article_detail(owner, Some("h1"), a).unwrap().unwrap();
        let models: Vec<_> = detail.digests.iter().map(|d| d.model.as_str()).collect();
        assert_eq!(models, ["opus", "sonnet"], "gated version is hidden");
        assert_eq!(detail.item.title_ja.as_deref(), Some("新版"));
        assert!(detail.translations.is_empty());
        assert!(detail.has_body);
        assert_eq!(db.article_detail(owner, None, 9999).unwrap(), None);
    }

    fn search_ids(db: &Db, terms: &[&str]) -> Vec<i64> {
        db.search_articles(&SearchQuery {
            terms: terms.iter().map(|t| t.to_string()).collect(),
            ..search_query(db)
        })
        .unwrap()
        .into_iter()
        .map(|i| i.article_id)
        .collect()
    }

    #[test]
    fn search_filters_by_period_source_and_lang() {
        let db = Db::open_in_memory().unwrap();
        let early = dated_article(&db, "https://e.com/early", "t", "2026-09-04T14:59:59Z");
        let start = dated_article(&db, "https://e.com/start", "t", "2026-09-04T15:00:00Z");
        let end = dated_article(&db, "https://e.com/end", "t", "2026-09-10T14:59:59Z");
        let late = dated_article(&db, "https://e.com/late", "t", "2026-09-10T15:00:00Z");
        let nra = db
            .insert_article(&NewArticle {
                source_id: "nra",
                lang: Lang::Ja,
                published_at: Some("2026-09-06T00:00:00Z"),
                ..article("https://e.com/nra")
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    since: Some(t("2026-09-04T15:00:00Z")),
                    until: Some(t("2026-09-10T15:00:00Z")),
                    ..search_query(&db)
                }
            ),
            [end, nra, start]
        );
        assert_eq!(
            found(&db, search_query(&db)),
            [late, end, nra, start, early]
        );
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    sources: vec!["nra".into(), "none".into()],
                    ..search_query(&db)
                }
            ),
            [nra]
        );
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    lang: Some(Lang::Ja),
                    ..search_query(&db)
                }
            ),
            [nra]
        );
    }

    fn digest_on(
        db: &Db,
        article_id: i64,
        topics: serde_json::Value,
        new: serde_json::Value,
        at: &str,
    ) {
        let c = db
            .insert_content(article_id, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        db.insert_artifact(
            &NewArtifact {
                article_id,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                // 同じ記事に版を重ねられるよう、作った時刻ごとに別のモデルとして登録する
                model: at,
                prompt_version: 2,
                payload: &serde_json::json!({
                    "title_ja": "題", "summary_ja": "要約", "lwr_relevant": true,
                    "topics": topics, "new_topics": new,
                }),
                inputs: &[c],
                glossary_at: None,
            },
            t(at),
        )
        .unwrap();
    }

    /// トピックは閲覧できる最新の要約で判定し、別名でも統合先で引ける。
    #[test]
    fn search_filters_by_topics_of_the_latest_digest() {
        let db = Db::open_in_memory().unwrap();
        let both = dated_article(&db, "https://e.com/both", "t", "2026-09-01T00:00:00Z");
        digest_on(
            &db,
            both,
            serde_json::json!(["燃料", "PWR"]),
            serde_json::json!([]),
            "2026-09-01T01:00:00Z",
        );
        let fuel = dated_article(&db, "https://e.com/fuel", "t", "2026-09-02T00:00:00Z");
        digest_on(
            &db,
            fuel,
            serde_json::json!(["燃料"]),
            serde_json::json!([]),
            "2026-09-02T01:00:00Z",
        );
        let merged = dated_article(&db, "https://e.com/merged", "t", "2026-09-03T00:00:00Z");
        digest_on(
            &db,
            merged,
            serde_json::json!(["新設炉"]),
            serde_json::json!([{"name": "新設炉", "facet": "分野"}]),
            "2026-09-03T01:00:00Z",
        );
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "b",
            "m",
            t("2026-09-04T00:00:00Z"),
        )
        .unwrap();
        // 古い版にだけ付いている語では引かない
        let redone = dated_article(&db, "https://e.com/redone", "t", "2026-09-05T00:00:00Z");
        digest_on(
            &db,
            redone,
            serde_json::json!(["BWR"]),
            serde_json::json!([]),
            "2026-09-05T01:00:00Z",
        );
        digest_on(
            &db,
            redone,
            serde_json::json!(["燃料"]),
            serde_json::json!([]),
            "2026-09-06T01:00:00Z",
        );

        let by = |topics: &[&str]| {
            found(
                &db,
                SearchQuery {
                    topics: topics.iter().map(|t| t.to_string()).collect(),
                    ..search_query(&db)
                },
            )
        };
        assert_eq!(by(&["燃料"]), [redone, fuel, both]);
        assert_eq!(by(&["燃料", "PWR"]), [both]);
        assert_eq!(by(&["新設・建設"]), [merged]);
        assert_eq!(by(&["新設炉"]), [merged]);
        assert!(by(&["BWR"]).is_empty());
        assert!(by(&["無い語"]).is_empty());
    }

    #[test]
    fn search_filters_by_state_and_orders_by_score() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let translated = scored_article(
            &db,
            "https://e.com/tr",
            Lang::En,
            "2026-09-01T00:00:00.000Z",
            90,
        );
        add_translation_text(&db, translated, "和訳");
        let liked = scored_article(
            &db,
            "https://e.com/up",
            Lang::En,
            "2026-09-02T00:00:00.000Z",
            50,
        );
        db.record_event(owner, liked, SignalKind::Up, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let read = scored_article(
            &db,
            "https://e.com/read",
            Lang::En,
            "2026-09-03T00:00:00.000Z",
            70,
        );
        db.record_event(
            owner,
            read,
            SignalKind::OpenDetail,
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();
        let disliked = scored_article(
            &db,
            "https://e.com/down",
            Lang::En,
            "2026-09-04T00:00:00.000Z",
            95,
        );
        db.record_event(owner, disliked, SignalKind::Down, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let unscored = dated_article(&db, "https://e.com/unscored", "t", "2026-09-05T00:00:00Z");

        let with = |q: SearchQuery<'static>| found(&db, q);
        assert_eq!(
            with(SearchQuery {
                translated: true,
                ..search_query(&db)
            }),
            [translated]
        );
        assert_eq!(
            with(SearchQuery {
                liked: true,
                ..search_query(&db)
            }),
            [liked]
        );
        assert_eq!(
            with(SearchQuery {
                unread: true,
                ..search_query(&db)
            }),
            [unscored, disliked, liked, translated]
        );
        assert_eq!(
            with(SearchQuery {
                min_score: Some(60),
                ..search_query(&db)
            }),
            [disliked, read, translated]
        );
        // 一覧の既定と同じく隠す：👎・未採点・閾値未満
        assert_eq!(
            with(SearchQuery {
                hide_below: Some(60),
                ..search_query(&db)
            }),
            [read, translated]
        );
        assert_eq!(
            with(SearchQuery {
                order: SearchOrder::Score,
                ..search_query(&db)
            }),
            [disliked, translated, read, liked, unscored]
        );
    }

    fn dated_article(db: &Db, url: &str, title: &str, published: &str) -> i64 {
        db.insert_article(&NewArticle {
            title,
            published_at: Some(published),
            ..article(url)
        })
        .unwrap()
        .unwrap()
    }

    fn add_translation_text(db: &Db, article_id: i64, body_ja: &str) {
        let c = db
            .insert_content(article_id, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        db.insert_translation(
            &NewArtifact {
                article_id,
                kind: ArtifactKind::Translation,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &serde_json::json!({ "body_ja": body_ja }),
                inputs: &[c],
                glossary_at: None,
            },
            t("2026-09-26T03:00:00Z"),
        )
        .unwrap();
    }

    #[test]
    fn search_matches_titles_bodies_digests_and_translations() {
        let db = Db::open_in_memory().unwrap();
        let title = dated_article(
            &db,
            "https://e.com/title",
            "Reactor Vessel Inspection",
            "2026-09-01T00:00:00Z",
        );
        let body = dated_article(&db, "https://e.com/body", "t", "2026-09-02T00:00:00Z");
        db.insert_content(
            body,
            ContentKind::Body,
            ContentOrigin::Page,
            "蒸気発生器の伝熱管を交換した",
        )
        .unwrap();
        let digest = dated_article(&db, "https://e.com/digest", "t", "2026-09-03T00:00:00Z");
        add_digest(
            &db,
            digest,
            "sonnet",
            "炉心溶融の解析",
            true,
            "2026-09-03T01:00:00Z",
        );
        let translation = dated_article(
            &db,
            "https://e.com/translation",
            "t",
            "2026-09-04T00:00:00Z",
        );
        add_translation_text(&db, translation, "格納容器の漏えい率試験");

        // 3 文字以上の語は索引で引く。英語は大文字と小文字を区別しない
        assert_eq!(search_ids(&db, &["reactor vessel"]), [title]);
        assert_eq!(search_ids(&db, &["伝熱管"]), [body]);
        assert_eq!(search_ids(&db, &["炉心溶融"]), [digest]);
        assert_eq!(search_ids(&db, &["漏えい率"]), [translation]);
        // 3 文字未満の語も引ける
        assert_eq!(search_ids(&db, &["炉心"]), [digest]);
        assert_eq!(search_ids(&db, &["交換"]), [body]);
        assert!(search_ids(&db, &["存在しない語"]).is_empty());
    }

    #[test]
    fn search_requires_every_term_across_parts_of_an_article() {
        let db = Db::open_in_memory().unwrap();
        let both = dated_article(
            &db,
            "https://e.com/both",
            "NRC approves uprate",
            "2026-09-01T00:00:00Z",
        );
        db.insert_content(
            both,
            ContentKind::Body,
            ContentOrigin::Page,
            "出力向上を承認",
        )
        .unwrap();
        let one = dated_article(&db, "https://e.com/one", "NRC news", "2026-09-02T00:00:00Z");
        assert_eq!(search_ids(&db, &["NRC", "出力向上"]), [both]);
        assert_eq!(search_ids(&db, &["nrc"]), [one, both]);
    }

    /// 語は FTS5 の構文として解釈せず、そのままの文字列として探す。
    #[test]
    fn search_treats_terms_literally() {
        let db = Db::open_in_memory().unwrap();
        let a = dated_article(
            &db,
            "https://e.com/a",
            "the \"AP1000\" OR* plan",
            "2026-09-01T00:00:00Z",
        );
        assert_eq!(search_ids(&db, &["\"AP1000\""]), [a]);
        assert_eq!(search_ids(&db, &["OR*"]), [a]);
        assert!(search_ids(&db, &["NOT"]).is_empty());
    }

    #[test]
    fn search_hides_text_the_user_cannot_view() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let a = dated_article(&db, "https://e.com/a", "t", "2026-09-01T00:00:00Z");
        db.conn()
            .execute(
                "INSERT INTO contents (article_id, kind, access_membership_id, text, origin, fetched_at)
                 VALUES (?1, 'fulltext', ?2, '会員限定の燃料設計', 'login', '2026-09-27T00:00:00Z')",
                [a, aesj],
            )
            .unwrap();
        let gated = db.conn().last_insert_rowid();
        let payload = serde_json::json!({"title_ja": "限定要約の題", "summary_ja": "s", "points_ja": ["p"],
            "implications_ja": "", "lwr_relevant": true, "topics": ["燃料"]});
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &payload,
                inputs: &[gated],
                glossary_at: None,
            },
            t("2026-09-26T00:00:00Z"),
        )
        .unwrap();
        assert!(search_ids(&db, &["燃料設計"]).is_empty());
        assert!(search_ids(&db, &["限定要約"]).is_empty());

        db.conn()
            .execute(
                "INSERT INTO user_memberships VALUES (?1, ?2)",
                [owner, aesj],
            )
            .unwrap();
        assert_eq!(search_ids(&db, &["燃料設計"]), [a]);
        assert_eq!(search_ids(&db, &["限定要約"]), [a]);
    }

    /// 検索は一覧の既定で隠す記事（非軽水炉・未採点・閾値未満）も含め、点数ではなく新しい順に並べる。
    #[test]
    fn search_includes_hidden_articles_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let old_high = scored_article(
            &db,
            "https://e.com/old",
            Lang::En,
            "2026-09-01T00:00:00.000Z",
            95,
        );
        let new_low = scored_article(
            &db,
            "https://e.com/new",
            Lang::En,
            "2026-09-20T00:00:00.000Z",
            10,
        );
        let unscored = dated_article(&db, "https://e.com/unscored", "t", "2026-09-10T00:00:00Z");
        add_digest(&db, unscored, "sonnet", "題", false, "2026-09-10T01:00:00Z");
        assert_eq!(search_ids(&db, &["題"]), [new_low, unscored, old_high]);
        assert_eq!(search_ids(&db, &[]), [new_low, unscored, old_high]);
    }

    /// 3 文字未満の語は索引を使えず走査になるが、論文の本文も含めてすべての文書を探す。
    /// LIKE の `%` と `_` は文字どおりに扱う。
    #[test]
    fn short_terms_scan_every_document_literally() {
        let db = Db::open_in_memory().unwrap();
        let pdf = dated_article(&db, "https://e.com/a.pdf", "t", "2026-09-01T00:00:00Z");
        db.insert_content(pdf, ContentKind::Body, ContentOrigin::Pdf, "炉心溶融の解析")
            .unwrap();
        let fulltext = dated_article(&db, "https://e.com/paper", "t", "2026-09-02T00:00:00Z");
        db.insert_content(
            fulltext,
            ContentKind::Fulltext,
            ContentOrigin::Upload,
            "炉心溶融の実験",
        )
        .unwrap();
        assert_eq!(search_ids(&db, &["炉心"]), [fulltext, pdf]);
        let percent = dated_article(&db, "https://e.com/p", "uprate 5%", "2026-09-03T00:00:00Z");
        let underscore = dated_article(&db, "https://e.com/u", "a_b", "2026-09-04T00:00:00Z");
        dated_article(&db, "https://e.com/x", "5x axb", "2026-09-05T00:00:00Z");
        assert_eq!(search_ids(&db, &["5%"]), [percent]);
        assert_eq!(search_ids(&db, &["_"]), [underscore]);
    }

    #[test]
    fn deleting_article_removes_it_from_the_index() {
        let db = Db::open_in_memory().unwrap();
        let a = dated_article(&db, "https://e.com/a", "t", "2026-09-01T00:00:00Z");
        db.insert_content(a, ContentKind::Body, ContentOrigin::Page, "本文")
            .unwrap();
        add_digest(&db, a, "sonnet", "題", true, "2026-09-01T01:00:00Z");
        add_translation_text(&db, a, "和訳");
        let docs = || -> i64 { db.query_i64("SELECT count(*) FROM search_docs").unwrap() };
        assert!(docs() > 0);
        db.conn()
            .execute("DELETE FROM articles WHERE id = ?1", [a])
            .unwrap();
        assert_eq!(docs(), 0);
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

    fn vocab(names: &[(&str, crate::topics::Facet)]) -> Vec<crate::topics::Entry> {
        names
            .iter()
            .map(|&(name, facet)| crate::topics::Entry {
                name: name.into(),
                facet,
                added_at: None,
            })
            .collect()
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

    fn glossary_term(sources: &[&str], target: &str, abbr: Option<&str>) -> crate::glossary::Term {
        crate::glossary::Term {
            sources: sources.iter().map(|s| s.to_string()).collect(),
            target: target.into(),
            abbr: abbr.map(Into::into),
            note: None,
        }
    }

    fn glossary_entry(db: &Db, id: i64) -> crate::glossary::Entry {
        db.glossary_entries()
            .unwrap()
            .into_iter()
            .find(|e| e.id == id)
            .unwrap()
    }

    /// 初期値の語は変更した時刻を持たない。加えた語は加えた時刻を持つ。
    #[test]
    fn glossary_terms_are_added_with_their_time() {
        let db = Db::open_in_memory().unwrap();
        assert!(
            db.glossary_entries()
                .unwrap()
                .iter()
                .all(|e| e.changed_at().is_none())
        );
        let term = glossary_term(
            &["emergency diesel generator", "EDG"],
            "非常用ディーゼル発電機",
            Some("EDG"),
        );
        let id = db
            .add_glossary_term(&term, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let entry = glossary_entry(&db, id);
        assert_eq!(entry.term, term);
        assert_eq!(
            entry.changed_at(),
            Some(timestamp(t("2026-09-27T00:00:00Z")).as_str())
        );
        assert!(
            db.glossary_entries()
                .unwrap()
                .iter()
                .any(|e| e.term == term)
        );
    }

    /// 置き換えでは、残した原語はそのまま、足した原語は加え、無くした原語は消す。
    /// 変更の時刻は、訳語か原語が変わったときだけ進む。
    #[test]
    fn glossary_term_update_replaces_sources_and_tracks_changes() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .add_glossary_term(
                &glossary_term(&["reactor coolant pump", "RCP"], "一次冷却材ポンプ", None),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        let later = t("2026-09-27T00:00:00Z") + chrono::Duration::hours(1);
        let updated = glossary_term(
            &["reactor coolant pump", "primary coolant pump"],
            "一次冷却材ポンプ",
            None,
        );
        assert!(db.update_glossary_term(id, &updated, later).unwrap());
        let entry = glossary_entry(&db, id);
        assert_eq!(entry.term, updated);
        assert_eq!(entry.changed_at(), Some(timestamp(later).as_str()));
        // 同じ内容で保存しても変更にならない
        let even_later = later + chrono::Duration::hours(1);
        assert!(db.update_glossary_term(id, &updated, even_later).unwrap());
        assert_eq!(
            glossary_entry(&db, id).changed_at(),
            Some(timestamp(later).as_str())
        );
        assert!(!db.update_glossary_term(9999, &updated, later).unwrap());
    }

    /// ほかの訳語の原語・訳語・略語と重なれば、その旨を返して何も変えない。
    #[test]
    fn glossary_rejects_conflicts_without_writing() {
        let db = Db::open_in_memory().unwrap();
        let before = db.glossary_entries().unwrap();
        for term in [
            glossary_term(&["new term", "atf"], "新しい語", None),
            glossary_term(&["new term"], "事故耐性燃料", None),
            glossary_term(&["new term"], "新しい語", Some("ATF")),
        ] {
            let err = db
                .add_glossary_term(&term, t("2026-09-27T00:00:00Z"))
                .unwrap_err();
            assert!(matches!(err, DbError::GlossaryConflict(_)), "{err:?}");
        }
        let err = db
            .add_glossary_term(
                &glossary_term(&["new term", "ATF"], "新しい語", None),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap_err();
        assert!(err.to_string().contains("事故耐性燃料"), "{err}");
        // 自分の原語はそのまま保存できる
        let atf = before
            .iter()
            .find(|e| e.term.target == "事故耐性燃料")
            .unwrap();
        assert!(
            db.update_glossary_term(atf.id, &atf.term, t("2026-09-27T00:00:00Z"))
                .unwrap()
        );
        assert_eq!(db.glossary_entries().unwrap(), before);
    }

    #[test]
    fn glossary_terms_can_be_deleted() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .add_glossary_term(
                &glossary_term(&["scrams"], "スクラム回数", None),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        assert!(db.delete_glossary_term(id).unwrap());
        assert!(!db.delete_glossary_term(id).unwrap());
        assert_eq!(
            db.query_i64(&format!(
                "SELECT count(*) FROM glossary_sources WHERE term_id = {id}"
            ))
            .unwrap(),
            0
        );
    }

    fn report(db: &Db, article_id: i64, found: &str, at: &str) {
        let owner = db.owner_id().unwrap();
        let report = NewReport::Term {
            found,
            wanted: Some("燃料取替停止"),
            source: None,
            note: None,
        };
        db.add_report(owner, article_id, &report, t(at)).unwrap();
    }

    /// 指摘は受付中で入り、新しい順に並ぶ。見出しは要約が無ければ原題。
    #[test]
    fn term_reports_start_pending_and_are_listed_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let b = db
            .insert_article(&article("https://e.com/b"))
            .unwrap()
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        report(&db, b, "燃料補給停止", "2026-09-27T01:00:00Z");
        let reports = db
            .reports(db.owner_id().unwrap(), &ReportFilter::default())
            .unwrap();
        let found: Vec<&str> = reports.iter().filter_map(|r| r.found.as_deref()).collect();
        assert_eq!(found, ["燃料補給停止", "給油停止"]);
        let r = &reports[1];
        assert_eq!(r.article_id, a);
        assert_eq!(r.article_title, "t");
        assert_eq!(r.wanted.as_deref(), Some("燃料取替停止"));
        assert_eq!(r.status, ReportStatus::Pending);
        assert_eq!(r.reported_at, "2026-09-27T00:00:00.000Z");
        assert_eq!(
            (r.resolved_at.as_deref(), &r.term, &r.reply),
            (None, &None, &None)
        );
        assert_eq!(
            db.reports(
                db.owner_id().unwrap(),
                &ReportFilter {
                    article_id: Some(a),
                    ..ReportFilter::default()
                }
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            db.report_counts().unwrap(),
            [
                (ReportStatus::Pending, 2),
                (ReportStatus::Added, 0),
                (ReportStatus::Existing, 0),
                (ReportStatus::Done, 0),
                (ReportStatus::Rejected, 0),
            ]
        );
    }

    /// 対応すると状況・訳語・ひとことと対応日時を残し、受付中に戻すと対応日時を消す。
    /// 結び付けた訳語を消しても指摘は残る。
    #[test]
    fn term_reports_are_resolved_and_can_be_reopened() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let id = db
            .reports(db.owner_id().unwrap(), &ReportFilter::default())
            .unwrap()[0]
            .id;
        let term = db
            .add_glossary_term(
                &glossary_term(&["refuelling outage"], "燃料取替停止（英綴り）", None),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        assert!(
            db.resolve_report(
                id,
                ReportStatus::Added,
                Some(term),
                Some("英綴りを追加"),
                t("2026-09-27T02:00:00Z")
            )
            .unwrap()
        );
        let r = &db
            .reports(
                db.owner_id().unwrap(),
                &ReportFilter {
                    status: Some(ReportStatus::Added),
                    ..ReportFilter::default()
                },
            )
            .unwrap()[0];
        assert_eq!(r.term, Some((term, "燃料取替停止（英綴り）".to_string())));
        assert_eq!(r.reply.as_deref(), Some("英綴りを追加"));
        assert_eq!(r.resolved_at.as_deref(), Some("2026-09-27T02:00:00.000Z"));
        assert!(
            db.reports(
                db.owner_id().unwrap(),
                &ReportFilter {
                    status: Some(ReportStatus::Pending),
                    ..ReportFilter::default()
                }
            )
            .unwrap()
            .is_empty()
        );

        db.delete_glossary_term(term).unwrap();
        assert_eq!(
            db.reports(db.owner_id().unwrap(), &ReportFilter::default())
                .unwrap()[0]
                .term,
            None
        );

        assert!(
            db.resolve_report(
                id,
                ReportStatus::Pending,
                None,
                None,
                t("2026-09-27T03:00:00Z")
            )
            .unwrap()
        );
        let r = &db
            .reports(db.owner_id().unwrap(), &ReportFilter::default())
            .unwrap()[0];
        assert_eq!(
            (r.status, r.resolved_at.as_deref()),
            (ReportStatus::Pending, None)
        );
        assert!(
            !db.resolve_report(
                9999,
                ReportStatus::Rejected,
                None,
                None,
                t("2026-09-27T03:00:00Z")
            )
            .unwrap()
        );
    }

    /// 訳語以外の指摘は内容だけを持ち、種類で絞れる。対応は「対応済」で、訳語は結び付けない。
    #[test]
    fn other_reports_carry_their_kind_and_body() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let other = NewReport::Other {
            kind: ReportKind::Digest,
            body: "要約の数値が原文と違う",
        };
        db.add_report(owner, a, &other, t("2026-09-27T01:00:00Z"))
            .unwrap();
        let only = |kind| {
            let filter = ReportFilter {
                kind: Some(kind),
                ..ReportFilter::default()
            };
            db.reports(owner, &filter).unwrap()
        };
        let digest = only(ReportKind::Digest);
        assert_eq!(digest.len(), 1);
        let r = &digest[0];
        assert_eq!(r.kind, ReportKind::Digest);
        assert_eq!(
            (r.found.as_deref(), r.note.as_deref()),
            (None, Some("要約の数値が原文と違う"))
        );
        assert_eq!(db.report_kind(r.id).unwrap(), Some(ReportKind::Digest));
        assert_eq!(db.report_kind(9999).unwrap(), None);
        assert_eq!(only(ReportKind::Term)[0].found.as_deref(), Some("給油停止"));

        assert!(
            db.resolve_report(
                r.id,
                ReportStatus::Done,
                None,
                None,
                t("2026-09-27T02:00:00Z")
            )
            .unwrap()
        );
        // 訳語集の状況や訳語は、訳語以外の指摘には付けられない
        for (status, term) in [(ReportStatus::Added, None), (ReportStatus::Done, Some(1))] {
            assert!(
                db.resolve_report(r.id, status, term, None, t("2026-09-27T03:00:00Z"))
                    .is_err()
            );
        }
        let term_id = only(ReportKind::Term)[0].id;
        assert!(
            db.resolve_report(
                term_id,
                ReportStatus::Done,
                None,
                None,
                t("2026-09-27T03:00:00Z")
            )
            .is_err()
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

    /// 受付箱の見出しには、利用者が閲覧できない（会員限定の本文から作った）要約を使わない。
    #[test]
    fn term_reports_do_not_show_titles_of_digests_the_viewer_cannot_see() {
        let db = Db::open_in_memory().unwrap();
        let m = insert_membership(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let gated = insert_content(&db, a, Some(m));
        let digest = insert_artifact(&db, a, "m");
        link_input(&db, digest, gated).unwrap();
        db.conn()
            .execute(
                "UPDATE artifacts SET payload = '{\"title_ja\": \"会員限定の見出し\"}' WHERE id = ?1",
                [digest],
            )
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let owner = db.owner_id().unwrap();
        assert_eq!(
            db.reports(owner, &ReportFilter::default()).unwrap()[0].article_title,
            "t"
        );
    }

    /// 対応日時は状況を変えたときだけ進み、ひとことや訳語だけを直しても変わらない。
    #[test]
    fn term_report_resolution_time_moves_only_with_the_status() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let owner = db.owner_id().unwrap();
        let id = db.reports(owner, &ReportFilter::default()).unwrap()[0].id;
        let resolve = |status, reply, at| {
            db.resolve_report(id, status, None, Some(reply), t(at))
                .unwrap();
            db.reports(owner, &ReportFilter::default()).unwrap()[0]
                .resolved_at
                .clone()
        };
        let first = resolve(ReportStatus::Added, "a", "2026-09-27T01:00:00Z");
        assert_eq!(first.as_deref(), Some("2026-09-27T01:00:00.000Z"));
        let same = resolve(ReportStatus::Added, "b", "2026-09-27T02:00:00Z");
        assert_eq!(same, first);
        let changed = resolve(ReportStatus::Rejected, "c", "2026-09-27T03:00:00Z");
        assert_eq!(changed.as_deref(), Some("2026-09-27T03:00:00.000Z"));
    }

    fn other_user(db: &Db) -> i64 {
        db.conn()
            .execute(
                "INSERT INTO users (login, display_name) VALUES ('other', 'other')",
                [],
            )
            .unwrap();
        db.conn().last_insert_rowid()
    }

    /// コメントは古い順に並び、自分のものと他人の公開のものだけが見える。
    #[test]
    fn comments_show_own_and_public_ones() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = other_user(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let at = |h: u32| t(&format!("2026-09-27T0{h}:00:00Z"));
        db.add_comment(owner, a, "自分のメモ", Visibility::Private, at(0))
            .unwrap();
        db.add_comment(other, a, "他人の公開", Visibility::Public, at(1))
            .unwrap();
        db.add_comment(other, a, "他人の非公開", Visibility::Private, at(2))
            .unwrap();
        let seen: Vec<(String, bool)> = db
            .comments(owner, a)
            .unwrap()
            .into_iter()
            .map(|c| (c.body, c.mine))
            .collect();
        assert_eq!(
            seen,
            [("自分のメモ".into(), true), ("他人の公開".into(), false)]
        );
        let mine = &db.comments(owner, a).unwrap()[0];
        assert_eq!(mine.visibility, Visibility::Private);
        assert_eq!(mine.created_at, "2026-09-27T00:00:00.000Z");
        assert_eq!(mine.updated_at, mine.created_at);
    }

    /// 直せるのも消せるのも自分のコメントだけ。
    #[test]
    fn comments_are_updated_and_deleted_only_by_their_author() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = other_user(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let id = db
            .add_comment(
                owner,
                a,
                "下書き",
                Visibility::Private,
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        let later = t("2026-09-27T01:00:00Z");
        assert_eq!(
            db.update_comment(other, id, "乗っ取り", Visibility::Public, later)
                .unwrap(),
            None
        );
        assert_eq!(
            db.update_comment(owner, id, "清書", Visibility::Public, later)
                .unwrap(),
            Some(a)
        );
        let c = &db.comments(other, a).unwrap()[0];
        assert_eq!(
            (c.body.as_str(), c.visibility, c.mine),
            ("清書", Visibility::Public, false)
        );
        assert_eq!(c.updated_at, "2026-09-27T01:00:00.000Z");
        assert_eq!(db.delete_comment(other, id).unwrap(), None);
        assert_eq!(db.delete_comment(owner, id).unwrap(), Some(a));
        assert!(db.comments(owner, a).unwrap().is_empty());
    }

    /// 同じ原語（大文字小文字の違いを含む）を別の訳語に結び付けられない。
    #[test]
    fn glossary_rejects_a_source_of_two_terms() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .conn()
            .execute_batch(
                "INSERT INTO glossary_terms (target) VALUES ('運転許可更新');
                 INSERT INTO glossary_sources (term_id, source)
                 SELECT id, 'License Renewal' FROM glossary_terms WHERE target = '運転許可更新';",
            )
            .unwrap_err();
        assert!(err.to_string().contains("UNIQUE"), "{err}");
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

    fn merge(from: &str, into: &str) -> TopicMerge {
        TopicMerge {
            from: from.into(),
            into: into.into(),
        }
    }

    fn propose(db: &Db, name: &str) -> i64 {
        digest_with_topics(
            db,
            serde_json::json!([name]),
            serde_json::json!([{"name": name, "facet": "分野"}]),
        )
        .unwrap()
    }

    fn aliases(db: &Db) -> Vec<String> {
        db.query_strings(
            "SELECT a.alias || '>' || t.name || '|' || a.backend || '|' || a.model || '|' || a.merged_at
             FROM topic_aliases AS a JOIN topics AS t ON t.id = a.topic_id ORDER BY a.alias",
        )
        .unwrap()
    }

    #[test]
    fn merge_topics_moves_links_and_records_aliases() {
        let db = Db::open_in_memory().unwrap();
        let curated = digest_with_topics(
            &db,
            serde_json::json!(["新設・建設"]),
            serde_json::json!([]),
        )
        .unwrap();
        let proposed = propose(&db, "新設炉");
        let both = digest_with_topics(
            &db,
            serde_json::json!(["新設・建設", "新設炉"]),
            serde_json::json!([]),
        )
        .unwrap();
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "claude-cli",
            "sonnet",
            t("2026-10-04T00:00:00Z"),
        )
        .unwrap();
        for id in [curated, proposed, both] {
            assert_eq!(linked_topics(&db, id), ["新設・建設"], "digest {id}");
        }
        assert!(db.topics().unwrap().iter().all(|t| t.name != "新設炉"));
        assert_eq!(
            aliases(&db),
            ["新設炉>新設・建設|claude-cli|sonnet|2026-10-04T00:00:00.000Z"]
        );
    }

    /// 統合した語を後でさらに統合しても、古い別名は最終的な統合先を指す。
    #[test]
    fn merge_topics_repoints_aliases_of_the_merged_topic() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        propose(&db, "新規建設");
        let at = t("2026-10-04T00:00:00Z");
        db.merge_topics(&[merge("新設炉", "新規建設")], "b", "m", at)
            .unwrap();
        db.merge_topics(&[merge("新規建設", "新設・建設")], "b", "m", at)
            .unwrap();
        let targets: Vec<String> = aliases(&db)
            .iter()
            .map(|a| a.split('|').next().unwrap().to_string())
            .collect();
        assert_eq!(targets, ["新規建設>新設・建設", "新設炉>新設・建設"]);
    }

    /// 統合した語を LLM がまた付けたり提案したりしても、統合先に付き、語彙に戻らない。
    #[test]
    fn saved_digests_resolve_aliases() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "b",
            "m",
            t("2026-10-04T00:00:00Z"),
        )
        .unwrap();
        let again = propose(&db, "新設炉");
        assert_eq!(linked_topics(&db, again), ["新設・建設"]);
        assert!(db.topics().unwrap().iter().all(|t| t.name != "新設炉"));
    }

    /// 統合した語を付けた要約も、詳細・API・MCP・採点では統合先の語で見せる
    /// （payload は LLM の出力の記録として書き換えず、読み出しを付与にそろえる）。
    #[test]
    fn digest_topics_are_read_from_links_after_merges() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = db
            .insert_article(&article("https://e.com/merged"))
            .unwrap()
            .unwrap();
        let c = db
            .insert_content(a, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let payload = serde_json::json!({
            "title_ja": "題", "summary_ja": "要約", "points_ja": ["点"], "implications_ja": "",
            "lwr_relevant": true, "topics": ["燃料", "新設炉"],
            "new_topics": [{"name": "新設炉", "facet": "分野"}],
        });
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
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "b",
            "m",
            t("2026-10-04T00:00:00Z"),
        )
        .unwrap();

        let detail = db.article_detail(owner, None, a).unwrap().unwrap();
        assert_eq!(
            detail.digests[0].payload["topics"],
            serde_json::json!(["燃料", "新設・建設"]),
            "in vocabulary order"
        );
        let inputs = db
            .pending_score(
                score_key(&db),
                t("2026-09-10T00:00:00Z"),
                t("2026-09-28T00:00:00Z"),
                10,
            )
            .unwrap();
        assert_eq!(inputs[0].topics, ["燃料", "新設・建設"]);
    }

    #[test]
    fn merge_topics_changes_nothing_on_failure() {
        let db = Db::open_in_memory().unwrap();
        let id = propose(&db, "新設炉");
        let at = t("2026-10-04T00:00:00Z");
        let err = db
            .merge_topics(
                &[merge("新設炉", "新設・建設"), merge("無い語", "燃料")],
                "b",
                "m",
                at,
            )
            .unwrap_err();
        assert!(
            matches!(&err, DbError::UnknownTopic(name) if name == "無い語"),
            "{err}"
        );
        let err = db
            .merge_topics(&[merge("新設炉", "新設炉")], "b", "m", at)
            .unwrap_err();
        assert!(
            matches!(&err, DbError::SelfMerge(name) if name == "新設炉"),
            "{err}"
        );
        assert_eq!(linked_topics(&db, id), ["新設炉"]);
        assert!(aliases(&db).is_empty());
    }

    /// 手で取り込んだ語彙に別名と同じ名前があれば、その名前は語として復活し、別名ではなくなる。
    #[test]
    fn importing_an_alias_name_makes_it_a_topic_again() {
        use crate::topics::{Entry, Facet};
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        db.merge_topics(
            &[merge("新設炉", "新設・建設")],
            "b",
            "m",
            t("2026-10-04T00:00:00Z"),
        )
        .unwrap();
        let mut topics = db.vocabulary().unwrap();
        topics.push(Entry {
            name: "新設炉".into(),
            facet: Facet::Reactor,
            added_at: None,
        });
        db.replace_topics(&topics).unwrap();
        assert!(aliases(&db).is_empty());
        let id =
            digest_with_topics(&db, serde_json::json!(["新設炉"]), serde_json::json!([])).unwrap();
        assert_eq!(linked_topics(&db, id), ["新設炉"]);
    }

    #[test]
    fn topic_usage_counts_digests_and_marks_proposals() {
        use crate::topics::Facet;
        let db = Db::open_in_memory().unwrap();
        digest_with_topics(&db, serde_json::json!(["燃料"]), serde_json::json!([])).unwrap();
        propose(&db, "データセンター需要");
        digest_with_topics(
            &db,
            serde_json::json!(["燃料", "データセンター需要"]),
            serde_json::json!([]),
        )
        .unwrap();
        let usage = db.topic_usage().unwrap();
        assert_eq!(usage.len(), db.topics().unwrap().len());
        let fuel = usage.iter().find(|u| u.name == "燃料").unwrap();
        assert_eq!(
            (fuel.facet, fuel.added_at.as_deref(), fuel.uses),
            (Facet::Field, None, 2)
        );
        let dc = usage.last().unwrap();
        assert_eq!(dc.name, "データセンター需要");
        assert_eq!(dc.added_at.as_deref(), Some("2026-09-27T00:00:00.000Z"));
        assert_eq!(dc.uses, 2);
        assert_eq!(usage.iter().find(|u| u.name == "PWR").unwrap().uses, 0);
    }

    #[test]
    fn replace_topics_adds_updates_and_removes_by_name() {
        use crate::topics::Facet;
        let db = Db::open_in_memory().unwrap();
        let first = vocab(&[("燃料", Facet::Field), ("PWR", Facet::Reactor)]);
        db.replace_topics(&first).unwrap();
        assert_eq!(db.vocabulary().unwrap(), first);
        let fuel_id: i64 = db
            .conn()
            .query_row("SELECT id FROM topics WHERE name = '燃料'", [], |r| {
                r.get(0)
            })
            .unwrap();

        let second = vocab(&[("燃料", Facet::Reactor), ("米国", Facet::Region)]);
        db.replace_topics(&second).unwrap();
        assert_eq!(db.vocabulary().unwrap(), second);
        let kept_id: i64 = db
            .conn()
            .query_row("SELECT id FROM topics WHERE name = '燃料'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kept_id, fuel_id, "an updated topic keeps its id");
    }

    /// LLM が足した語かどうかは語彙ファイルの added_at で決まる。書き出した語彙を取り込み直しても変わらず、
    /// added_at を消して取り込めば人が決めた語になる（整理で統合されない）。
    #[test]
    fn import_decides_whether_topics_are_proposed() {
        let db = Db::open_in_memory().unwrap();
        propose(&db, "新設炉");
        let exported = db.vocabulary().unwrap();
        let proposed = exported.iter().find(|e| e.name == "新設炉").unwrap();
        assert_eq!(
            proposed.added_at.as_deref(),
            Some("2026-09-27T00:00:00.000Z")
        );
        assert!(
            exported
                .iter()
                .filter(|e| e.name != "新設炉")
                .all(|e| e.added_at.is_none())
        );

        db.replace_topics(&exported).unwrap();
        assert_eq!(
            db.vocabulary().unwrap(),
            exported,
            "round trip keeps origins"
        );

        let curated: Vec<_> = exported
            .iter()
            .cloned()
            .map(|e| crate::topics::Entry {
                added_at: None,
                ..e
            })
            .collect();
        db.replace_topics(&curated).unwrap();
        let usage = db.topic_usage().unwrap();
        assert!(usage.iter().all(|u| u.added_at.is_none()));
    }

    #[test]
    fn replace_topics_refuses_to_remove_topics_in_use() {
        use crate::topics::Facet;
        let db = Db::open_in_memory().unwrap();
        let before = vocab(&[("燃料", Facet::Field), ("PWR", Facet::Reactor)]);
        db.replace_topics(&before).unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let digest = insert_artifact(&db, a, "public");
        db.conn()
            .execute(
                "INSERT INTO artifact_topics (artifact_id, topic_id)
                 SELECT ?1, id FROM topics WHERE name = 'PWR'",
                [digest],
            )
            .unwrap();
        let err = db
            .replace_topics(&vocab(&[("燃料", Facet::Region)]))
            .unwrap_err();
        assert!(
            matches!(&err, DbError::TopicsInUse(names) if names == &["PWR"]),
            "{err}"
        );
        assert_eq!(
            db.vocabulary().unwrap(),
            before,
            "nothing changes on failure"
        );

        // 要約の版が消えれば付与も消え、語を削除できる
        db.conn()
            .execute("DELETE FROM artifacts WHERE id = ?1", [digest])
            .unwrap();
        db.replace_topics(&vocab(&[("燃料", Facet::Field)]))
            .unwrap();
    }

    #[test]
    fn warnings_report_failing_sources_and_recent_llm_errors() {
        let db = Db::open_in_memory().unwrap();
        db.record_source_success("ok").unwrap();
        db.record_source_failure("recovered", "old").unwrap();
        db.record_source_success("recovered").unwrap();
        db.record_source_failure("nei", "HTTP 403").unwrap();
        db.record_llm_call(
            &LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 5,
                ok: false,
                duration_ms: 1,
                error: Some("Not logged in"),
                rate_limit: None,
            },
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        let warnings = db.warnings(t("2026-09-26T00:00:00Z")).unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            matches!(&warnings[0], Warning::SourceFailing { source_id, error, .. }
            if source_id == "nei" && error == "HTTP 403")
        );
        assert!(
            matches!(&warnings[1], Warning::LlmFailed { error, .. } if error == "Not logged in")
        );
        // 失敗の後に成功した呼び出しがあれば、LLM の警告は出さない
        db.record_llm_call(
            &LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 5,
                ok: true,
                duration_ms: 1,
                error: None,
                rate_limit: None,
            },
            t("2026-09-27T02:00:00Z"),
        )
        .unwrap();
        assert_eq!(db.warnings(t("2026-09-26T00:00:00Z")).unwrap().len(), 1);
    }

    /// 同じ digest に複数のモデルの採点があれば、先回り和訳と同じく最高点を使う。
    #[test]
    fn list_uses_highest_score_across_scorers() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let digest: i64 = db
            .conn()
            .query_row("SELECT id FROM artifacts WHERE article_id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        db.insert_score(
            ScoreKey {
                user_id: db.owner_id().unwrap(),
                profile_hash: "h1",
                backend: "claude-cli",
                model: "haiku",
            },
            digest,
            50,
            Some("低い"),
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();
        let item = &db.list_articles(list_query(&db, false)).unwrap()[0];
        assert_eq!(item.score, Some(90));
    }

    /// 「最新の呼び出し」は記録の順ではなく、呼び出した時刻で決める。
    #[test]
    fn warnings_use_call_time_not_insertion_order() {
        let db = Db::open_in_memory().unwrap();
        let call = |ok: bool| LlmCall {
            stage: "digest",
            backend: "claude-cli",
            model: "sonnet",
            n_items: 1,
            ok,
            duration_ms: 1,
            error: (!ok).then_some("Not logged in"),
            rate_limit: None,
        };
        db.record_llm_call(&call(false), t("2026-09-27T02:00:00Z"))
            .unwrap();
        // 古い成功が後から記録された
        db.record_llm_call(&call(true), t("2026-09-27T01:00:00Z"))
            .unwrap();
        let warnings = db.warnings(t("2026-09-26T00:00:00Z")).unwrap();
        assert!(
            matches!(&warnings[..], [Warning::LlmFailed { .. }]),
            "{warnings:?}"
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

    /// ブックマークした記事は振り分け済みなので、「すべて表示」でも一覧に出さない。
    /// 件数の上限は除いた後にかける（ブックマークが上位を占めても一覧が減らない）。
    #[test]
    fn list_leaves_out_bookmarked_articles_before_the_limit() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let top = scored_article(
            &db,
            "https://e.com/top",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            95,
        );
        let next = scored_article(
            &db,
            "https://e.com/next",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            80,
        );
        db.record_event(owner, top, SignalKind::Bookmark, t("2026-09-27T00:00:00Z"))
            .unwrap();
        for show_all in [false, true] {
            let ids: Vec<i64> = db
                .list_articles(ListQuery {
                    limit: 1,
                    ..list_query(&db, show_all)
                })
                .unwrap()
                .into_iter()
                .map(|i| i.article_id)
                .collect();
            assert_eq!(ids, [next], "show_all = {show_all}");
        }
        // 外せば一覧に戻る
        db.unbookmark(owner, top).unwrap();
        assert_eq!(list_ids(&db, false), [top, next]);
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
