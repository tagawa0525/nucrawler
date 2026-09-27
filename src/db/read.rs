//! 画面向けの読み出し（一覧・検索・記事の詳細）。

use super::*;

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

pub(super) fn lang_code(lang: Lang) -> &'static str {
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
pub(super) fn linked_topics(alias: &str) -> String {
    format!(
        "(SELECT json_group_array(name) FROM (
           SELECT t.name FROM artifact_topics AS at
           JOIN topics AS t ON t.id = at.topic_id
           WHERE at.artifact_id = {alias}.id
           ORDER BY t.id))"
    )
}

/// 別名 `alias` の成果物を、利用者（`:user`）が閲覧できる条件。
pub(super) fn viewable(alias: &str) -> String {
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
pub(super) enum ContentSet {
    All,
    Body,
}

impl Db {
    /// 記事の公開の本文の部分。`All` は概要から本文まで（要約の入力）、`Body` は本文だけ（和訳の入力）。
    pub(super) fn public_contents(
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
                       -- 採点のプロンプトの最新の版を使い、その版で複数のモデルの採点があれば、
                       -- 先回り和訳と同じく最高点を使う
                       ORDER BY s.prompt_version DESC, s.score DESC, s.created_at DESC, s.id DESC
                       LIMIT 1) AS score_id,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

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
                prompt_version: 1,
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

    /// 採点のプロンプトの版を上げたら、古い版の点数ではなく最新の版の点数を使う
    /// （版をまたいだ最高点にすると、古いプロンプトの高い点が新しい採点を隠してしまう）。
    #[test]
    fn list_prefers_latest_score_prompt_version() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        rescore_with_version(&db, a, 2, 40);
        // 閾値未満になるので「すべて表示」で確かめる
        let item = &db.list_articles(list_query(&db, true)).unwrap()[0];
        assert_eq!(item.score, Some(40));
        assert!(list_ids(&db, false).is_empty());
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
}
