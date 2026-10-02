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
    /// 推薦点：現在のプロファイルでの LLM の点数（最新の digest に付いたもの）に、評価から学んだ補正を
    /// 足した点数（`crate::recommend`）。並び・閾値はこれで決める
    pub score: Option<u8>,
    /// 補正の前の LLM の点数
    pub llm_score: Option<u8>,
    pub reason: Option<String>,
    /// その点数が当たった関心分野（プロファイルの interest の topic）
    pub matched: Vec<String>,
    /// その点数が当たった推薦しない話題（プロファイルの exclude）
    pub excluded: Vec<String>,
    /// 既読になった時刻（開いた・既読の印を付けた。未読なら None）
    pub read_at: Option<String>,
    pub rating: Option<Rating>,
    pub bookmarked: bool,
    pub has_translation: bool,
    pub translation_requested: bool,
    /// 原文を読むのに必要で、利用者が持っていない会員資格の名前（🔒 の表示用）
    pub locked_by: Vec<String>,
    /// 同じ報道のグループ（`article_stories.story_id`。単独の記事なら自分の ID）
    pub story_id: i64,
    /// 同じグループのほかの記事のソース（記事ごと、日時の順）
    pub story_others: Vec<String>,
    /// 同じグループのどれか（この記事を含む）を読んだ
    pub story_read: bool,
    /// 同じグループのどれか（この記事を含む）に評価を付けた
    pub story_rated: bool,
}

impl ListItem {
    pub fn is_read(&self) -> bool {
        self.read_at.is_some()
    }
}

/// 一覧の条件。
#[derive(Debug, Clone, Copy)]
pub struct ListQuery<'a> {
    pub user_id: i64,
    pub profile_hash: Option<&'a str>,
    /// `show_all` でないときに表示する最低点。`None` なら推薦点で絞らない（プロファイルが無く採点が無い利用者の既定）
    pub min_score: Option<u8>,
    /// これ以降に公開（無ければ取得）された記事
    pub since: chrono::DateTime<chrono::Utc>,
    /// 評価 1〜2、閾値未満、未採点、非軽水炉の記事も表示する
    pub show_all: bool,
    /// 既読で絞る（true は既読だけ、false は未読だけ、None は絞らない）。件数の上限より前に絞る
    pub read: Option<bool>,
    /// ブックマークで絞る（true はブックマーク中だけ、false はブックマークしていない記事だけ）
    pub bookmarked: Option<bool>,
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
    /// 評価で絞る
    pub rating: RatingFilter,
    /// 既読で絞る（true は既読だけ、false は未読だけ）
    pub read: Option<bool>,
    /// ブックマークで絞る（true はブックマーク中だけ、false はブックマークしていない記事だけ）
    pub bookmarked: Option<bool>,
    /// この点数以上（未採点は除く）
    pub min_score: Option<u8>,
    /// 一覧の既定と同じく、評価 1〜2・非軽水炉の記事と、`hide_below` があれば未採点とその点数未満の記事を隠す
    pub hide: bool,
    /// `hide` のときに隠す最低点。`None` なら推薦点では隠さない（プロファイルが無く採点が無い利用者の既定）
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

/// 評価で絞る条件。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RatingFilter {
    /// 絞らない
    #[default]
    Any,
    /// 評価を付けた記事のうち、この評価以上
    AtLeast(Rating),
    /// 評価の無い記事だけ
    Unrated,
}

impl RatingFilter {
    /// 組み立てた行（`rows`）の条件（`AND` で始まる）。
    fn sql(self) -> String {
        match self {
            Self::Any => String::new(),
            Self::AtLeast(min) => format!(" AND rows.rating >= {}", min.get()),
            Self::Unrated => " AND rows.rating IS NULL".to_string(),
        }
    }
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
    /// 推薦点の補正の内訳：効いた特徴と、その特徴が無かったときから動かした点数（大きい順）
    pub adjustments: Vec<(crate::recommend::Feature, i32)>,
    /// 同じ報道のほかの記事（日時の順）
    pub story: Vec<super::StoryArticle>,
    /// 関連記事（新しい順。関連のグループは 1 件にまとめる）
    pub related: Vec<super::StoryArticle>,
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
        min_score: Option<u8>,
        read: Option<bool>,
        bookmarked: Option<bool>,
        limit: usize,
    },
    Search(&'a SearchQuery<'a>),
    /// 確認枠の候補：期間内の軽水炉の記事で、採点済みで閾値未満、未評価・未読で、確認枠の記録も無いもの。
    /// 無作為な順に `limit` 件
    Explore {
        since: chrono::DateTime<chrono::Utc>,
        min_score: u8,
        limit: usize,
    },
}

/// 既読・ブックマークの印で絞る条件（組み立てた行 `rows` の条件、`AND` で始まる）。
/// `Some(true)` は印のある記事だけ、`Some(false)` は印の無い記事だけ、`None` は絞らない。
/// 既読（`read_at` の列の式）とブックマークの条件。
fn mark_filter(read_at: &str, read: Option<bool>, bookmarked: Option<bool>) -> String {
    let read = match read {
        Some(true) => format!(" AND {read_at} IS NOT NULL"),
        Some(false) => format!(" AND {read_at} IS NULL"),
        None => String::new(),
    };
    let bookmarked = match bookmarked {
        Some(true) => " AND rows.bookmarked = 1",
        Some(false) => " AND rows.bookmarked = 0",
        None => "",
    };
    format!("{read}{bookmarked}")
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
        f.rows.push_str(&q.rating.sql());
        f.rows
            .push_str(&mark_filter("rows.read_at", q.read, q.bookmarked));
        if let Some(min) = q.min_score {
            f.rows.push_str(" AND rows.rec >= :min_score");
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
        viewable_r = viewable("r", ":user"),
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

/// 記事（式 `article`）の、利用者（パラメータ `user`）が閲覧できる最新の要約の列 `column`（要約が無ければ
/// NULL）。記事ごとにまとめて選ぶ `latest_digests` の CTE と同じ規則（同じ時刻なら id の大きい方）。
pub(super) fn latest_digest(column: &str, article: &str, user: &str) -> String {
    format!(
        "(SELECT ld.{column} FROM artifacts AS ld
          WHERE ld.article_id = {article} AND ld.kind = 'digest' AND {viewable}
          ORDER BY ld.created_at DESC, ld.id DESC LIMIT 1)",
        viewable = viewable("ld", user),
    )
}

/// 記事（式 `article`）の最新の見出しの和訳（無ければ NULL）。見出しは公開なので閲覧の制限は掛けない。
pub(super) fn latest_title_translation(article: &str) -> String {
    format!(
        "(SELECT lt.title_ja FROM artifacts AS lt
          WHERE lt.article_id = {article} AND lt.kind = 'title'
          ORDER BY lt.created_at DESC, lt.id DESC LIMIT 1)"
    )
}

/// 和文の見出し：要約の見出し（式 `digest_title`）、空か無ければ記事（式 `article`）の最新の見出しの和訳
/// （本文が取れず要約できない記事）。どちらも無ければ NULL。
pub(super) fn title_ja(digest_title: &str, article: &str) -> String {
    format!(
        "coalesce(nullif(trim({digest_title}), ''), {translation})",
        translation = latest_title_translation(article),
    )
}

/// 点数（式 `score_id`）で当たった語（`kind` は `interest` か `exclude`）の名前の JSON 配列（名前の順）。
/// 推薦の補正の特徴になるので、一覧・学習・`eval` で同じ値を読む。
pub(super) fn matched_topics(score_id: &str, kind: &str) -> String {
    format!(
        "(SELECT json_group_array(topic) FROM (
           SELECT sm.topic FROM score_matches AS sm
           WHERE sm.score_id = {score_id} AND sm.kind = '{kind}' ORDER BY sm.topic))"
    )
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

/// 利用者（パラメータ `user`）が閲覧できる要約（`viewable`）と、そのうち記事ごとに最新のもの（`latest`）の
/// CTE（`WITH` の後に置く）。採点の対象を選ぶ処理で、LLM と embedding の条件をそろえる。記事ごとに選ぶ
/// `latest_digest` と同じ規則（まとめて選ぶ場面の実行計画を変えないよう、別に持つ）。
pub(super) fn latest_digests(user: &str) -> String {
    format!(
        "viewable AS (
           SELECT r.* FROM artifacts AS r
           WHERE r.kind = 'digest' AND {viewable}
         ),
         latest AS (
           SELECT v.* FROM viewable AS v
           WHERE NOT EXISTS (
             SELECT 1 FROM viewable AS w
             WHERE w.article_id = v.article_id
               AND (w.created_at > v.created_at
                    OR (w.created_at = v.created_at AND w.id > v.id)))
         )",
        viewable = viewable("r", user),
    )
}

/// 別名 `alias` の成果物を、利用者（パラメータ `user`。`:user` や `?1`）が閲覧できる条件。
pub(super) fn viewable(alias: &str, user: &str) -> String {
    format!(
        "NOT EXISTS (
           SELECT 1 FROM artifact_access AS aa
           WHERE aa.artifact_id = {alias}.id
             AND aa.membership_id NOT IN (
               SELECT membership_id FROM user_memberships WHERE user_id = {user}))"
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
                read: q.read,
                bookmarked: q.bookmarked,
                limit: q.limit,
            },
        )
    }

    /// 検索。条件は `SearchQuery` のとおりで、`hide` でなければ一覧で隠す記事も含め、
    /// `order` の順（既定は新しい順）に並べる。
    pub fn search_articles(&self, q: &SearchQuery) -> Result<Vec<ListItem>, DbError> {
        self.query_items(q.user_id, q.profile_hash, ItemScope::Search(q))
    }

    /// 記事の URL（原文へ移るとき）。記事が無ければ None。
    pub fn article_url(&self, article_id: i64) -> Result<Option<String>, DbError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT url FROM articles WHERE id = ?1",
                [article_id],
                |r| r.get(0),
            )
            .optional()?)
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
        let digests = self.versions(user_id, article_id, ArtifactKind::Digest)?;
        // 補正の内訳は、一覧と同じ特徴（採点した最新の digest のトピック）で求める
        let adjustments = match item.llm_score {
            Some(llm) => {
                let topics: Vec<String> = digests
                    .first()
                    .and_then(|d| d.payload["topics"].as_array())
                    .map(|t| {
                        t.iter()
                            .filter_map(|t| t.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let features = crate::recommend::features(
                    &item.source_id,
                    &topics,
                    &item.matched,
                    &item.excluded,
                );
                self.recommend_model(user_id, profile_hash)?
                    .contributions(llm, &features)
            }
            None => Vec::new(),
        };
        Ok(Some(ArticleDetail {
            digests,
            translations: self.versions(user_id, article_id, ArtifactKind::Translation)?,
            item,
            has_body,
            adjustments,
            story: self.story_members(user_id, article_id)?,
            related: self.related_articles(user_id, article_id)?,
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
            viewable = viewable("r", ":user"),
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

    /// その日（日本時間の日付 `today`）の確認枠の記事。閾値（`q.min_score`）未満の記事から無作為に
    /// 選び、日ごとに `per_day` 件まで記録する。同じ日は同じ記事を返し、1 つの記事は 1 回しか選ばない。
    /// 選んだ記事のうち、評価が付いたものは返さない。
    pub fn explore(
        &self,
        q: ListQuery,
        per_day: usize,
        today: &str,
    ) -> Result<Vec<ListItem>, DbError> {
        // 最低点が無ければ、閾値未満という区別も無い
        let Some(min_score) = q.min_score.filter(|_| per_day > 0) else {
            return Ok(Vec::new());
        };
        let labeled: std::collections::HashSet<i64> = self
            .eval_labels(q.user_id)?
            .into_iter()
            .map(|l| l.article_id)
            .collect();
        let picked_on = |day: Option<&str>| -> Result<Vec<i64>, DbError> {
            let mut stmt = self.conn.prepare(
                "SELECT article_id FROM explore_picks
                 WHERE user_id = ?1 AND (?2 IS NULL OR picked_on = ?2) ORDER BY article_id",
            )?;
            let rows = stmt.query_map(rusqlite::params![q.user_id, day], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        };
        let need = per_day.saturating_sub(picked_on(Some(today))?.len());
        if need > 0 {
            // 条件と無作為な選択は SQL で済ませ、足りない分だけを読む
            let candidates = self.query_items(
                q.user_id,
                q.profile_hash,
                ItemScope::Explore {
                    since: q.since,
                    min_score,
                    limit: need,
                },
            )?;
            let tx = self.conn.unchecked_transaction()?;
            for c in &candidates {
                tx.execute(
                    "INSERT INTO explore_picks (user_id, article_id, picked_on) VALUES (?1, ?2, ?3)",
                    rusqlite::params![q.user_id, c.article_id, today],
                )?;
            }
            tx.commit()?;
        }
        let mut items = Vec::new();
        for id in picked_on(Some(today))? {
            if labeled.contains(&id) {
                continue;
            }
            // 選んだ後に採点し直されたり要約が変わったりして条件を外れた記事は出さない
            items.extend(
                self.query_items(q.user_id, q.profile_hash, ItemScope::One(id))?
                    .into_iter()
                    // 選んだ後に同じグループのほかの記事を評価したら、記事自身の評価と同じく外す
                    .filter(|i| {
                        i.lwr_relevant == Some(true)
                            && i.score.is_some_and(|s| s < min_score)
                            && !i.story_rated
                    }),
            );
        }
        Ok(items)
    }

    /// 一覧・詳細に共通の行の組み立て。
    fn query_items(
        &self,
        user_id: i64,
        profile_hash: Option<&str>,
        scope: ItemScope,
    ) -> Result<Vec<ListItem>, DbError> {
        // 並び・閾値は推薦点（rows.rec）で決める
        const BY_SCORE: &str = "rows.rec IS NULL, rows.rec DESC, rows.at DESC, rows.id DESC";
        const NEWEST: &str = "rows.at DESC, rows.id DESC";
        let list_filter = match scope {
            // 同じ報道のグループは、どれかを読んだ・評価したらグループごと選ばない
            ItemScope::Explore { .. } => "AND rows.relevant = 1 AND rows.rec < :min
                 AND rows.rating IS NULL AND rows.read_at IS NULL
                 AND rows.story_read_at IS NULL AND rows.story_rated = 0
                 AND NOT EXISTS (
                   SELECT 1 FROM explore_picks AS p
                   WHERE p.user_id = :user AND p.article_id = rows.id)"
                .to_string(),
            // 同じ報道のグループは、どれかを読んだら既読、どれかの評価が 1〜2 なら隠す
            ItemScope::List {
                read, bookmarked, ..
            } => format!(
                "{} AND (:all = 1 OR rows.story_low = 0)",
                mark_filter(
                    "coalesce(rows.read_at, rows.story_read_at)",
                    read,
                    bookmarked
                )
            ),
            _ => String::new(),
        };
        // 一覧と確認枠では、同じ報道のグループを並びの先頭の 1 件にまとめる
        let fold = match scope {
            ItemScope::List { .. } | ItemScope::Explore { .. } => "rows.story_rank = 1",
            _ => "1",
        };
        let (id, since, show_all, min_score, limit, order) = match scope {
            ItemScope::One(id) => (Some(id), None, true, None, 1, BY_SCORE),
            ItemScope::List {
                since,
                show_all,
                min_score,
                limit,
                ..
            } => (None, Some(since), show_all, min_score, limit, BY_SCORE),
            // 既定の条件（:all = 0 のときの絞り込み）は使わず、list_filter で絞る
            ItemScope::Explore {
                since,
                min_score,
                limit,
            } => (None, Some(since), true, Some(min_score), limit, "random()"),
            ItemScope::Search(q) => (
                None,
                q.since,
                !q.hide,
                q.hide_below,
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
                      {digest_id} AS digest_id
               FROM articles AS a
               WHERE (:id IS NULL OR a.id = :id)
                 AND (:since IS NULL OR coalesce(a.published_at, a.fetched_at) >= :since)
                 {items_filter}
             ),
             rows AS (
               -- 要約が無ければ見出しの和訳を使う（本文が取れず要約できない記事。見出しは公開なので
               -- 本文の閲覧の制限は掛からない）
               SELECT i.*,
                      {title_ja} AS title_ja,
                      d.summary_ja,
                      json_extract(d.payload, '$.lwr_relevant') AS relevant,
                      (SELECT s.id FROM scores AS s
                       WHERE s.user_id = :user AND s.profile_hash = :profile
                         AND s.artifact_id = i.digest_id
                         -- embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）使わない
                         AND s.backend <> 'embedding'
                       -- 採点のプロンプトの最新の版を使い、その版で複数のモデルの採点があれば、
                       -- 先回り和訳と同じく最高点を使う
                       ORDER BY s.prompt_version DESC, s.score DESC, s.created_at DESC, s.id DESC
                       LIMIT 1) AS score_id,
                      (SELECT rd.read_at FROM reads AS rd
                       WHERE rd.user_id = :user AND rd.article_id = i.id) AS read_at,
                      (SELECT rt.value FROM ratings AS rt
                       WHERE rt.user_id = :user AND rt.article_id = i.id) AS rating,
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
                         ORDER BY m.name)) AS locked_by,
                      (SELECT st.story_id FROM article_stories AS st
                       WHERE st.article_id = i.id) AS story_id,
                      -- 同じ報道のグループのほかの記事のソース（日時の順）
                      (SELECT json_group_array(source_id) FROM (
                         SELECT a2.source_id FROM article_stories AS s1
                         JOIN article_stories AS s2
                           ON s2.story_id = s1.story_id AND s2.article_id <> s1.article_id
                         JOIN articles AS a2 ON a2.id = s2.article_id
                         WHERE s1.article_id = i.id
                         ORDER BY coalesce(a2.published_at, a2.fetched_at), a2.id))
                        AS story_others,
                      -- グループのどれかを読んだ時刻、どれかに付けた評価
                      (SELECT max(rd.read_at) FROM article_stories AS s1
                       JOIN article_stories AS s2 ON s2.story_id = s1.story_id
                       JOIN reads AS rd ON rd.article_id = s2.article_id AND rd.user_id = :user
                       WHERE s1.article_id = i.id) AS story_read_at,
                      EXISTS (
                        SELECT 1 FROM article_stories AS s1
                        JOIN article_stories AS s2 ON s2.story_id = s1.story_id
                        JOIN ratings AS rt ON rt.article_id = s2.article_id AND rt.user_id = :user
                        WHERE s1.article_id = i.id) AS story_rated,
                      EXISTS (
                        SELECT 1 FROM article_stories AS s1
                        JOIN article_stories AS s2 ON s2.story_id = s1.story_id
                        JOIN ratings AS rt ON rt.article_id = s2.article_id AND rt.user_id = :user
                        WHERE s1.article_id = i.id AND rt.value <= 2) AS story_low
               FROM items AS i
               LEFT JOIN artifacts AS d ON d.id = i.digest_id
             ),
             scored AS (
               SELECT rows.*, {rec} AS rec
               FROM rows
               LEFT JOIN scores AS s ON s.id = rows.score_id
             )
             SELECT * FROM (
             SELECT rows.id, rows.source_id, rows.url, rows.title, rows.lang, rows.at,
                    rows.fetched_at, rows.title_ja, rows.summary_ja, rows.relevant,
                    rows.rec, s.reason, rows.read_at, rows.rating, rows.has_translation,
                    rows.requested, rows.locked_by, rows.bookmarked,
                    {matched} AS matched,
                    {excluded} AS excluded,
                    s.score, rows.story_id, rows.story_others,
                    coalesce(rows.read_at, rows.story_read_at) IS NOT NULL AS story_read,
                    rows.rating IS NOT NULL OR rows.story_rated AS story_rated,
                    row_number() OVER (
                      PARTITION BY rows.story_id ORDER BY {order})
                      AS story_rank
             FROM scored AS rows
             LEFT JOIN scores AS s ON s.id = rows.score_id
             -- 既定では評価 1〜2、非軽水炉を隠し、最低点があれば未採点と閾値未満も隠す
             WHERE (:all = 1
                OR ((rows.rating IS NULL OR rows.rating > 2)
                    AND rows.relevant = 1 AND (:min IS NULL OR rows.rec >= :min)))
               {rows_filter}
               {list_filter}
             ) AS rows
             WHERE {fold}
             ORDER BY {order}
             LIMIT :limit",
            digest_id = latest_digest("id", "a.id", ":user"),
            title_ja = title_ja("d.title_ja", "i.id"),
            matched = matched_topics("s.id", "interest"),
            excluded = matched_topics("s.id", "exclude"),
            viewable_t = viewable("t", ":user"),
            rec = super::recommend::recommend_score_sql(),
        );
        let model = self.recommend_model(user_id, profile_hash)?;
        let weights = model.weights_json();
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
            (":rec_weights", &weights),
        ];
        params.extend(
            filter_params
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_ref())),
        );
        let rows = stmt.query_map(params.as_slice(), |r| {
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
                llm_score: r.get(20)?,
                reason: r.get(11)?,
                matched: Vec::new(),
                excluded: Vec::new(),
                read_at: r.get(12)?,
                rating: r.get(13)?,
                bookmarked: r.get(17)?,
                has_translation: r.get(14)?,
                translation_requested: r.get(15)?,
                locked_by: Vec::new(),
                story_id: r.get(21)?,
                story_others: Vec::new(),
                story_read: r.get(23)?,
                story_rated: r.get(24)?,
            };
            Ok((
                item,
                r.get::<_, String>(16)?,
                r.get::<_, String>(18)?,
                r.get::<_, String>(19)?,
                r.get::<_, String>(22)?,
            ))
        })?;
        rows.map(|row| {
            let (mut item, locked_by, matched, excluded, story_others) = row?;
            item.locked_by = serde_json::from_str(&locked_by)?;
            item.matched = serde_json::from_str(&matched)?;
            item.excluded = serde_json::from_str(&excluded)?;
            item.story_others = serde_json::from_str(&story_others)?;
            Ok(item)
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    /// 記事を同じ報道のグループにする（グループの ID は最小の記事 ID）。
    fn group(db: &Db, ids: &[i64]) {
        let story = *ids.iter().min().unwrap();
        for id in ids {
            db.conn()
                .execute(
                    "UPDATE article_stories SET story_id = ?2 WHERE article_id = ?1",
                    [*id, story],
                )
                .unwrap();
        }
    }

    fn set_source(db: &Db, id: i64, source: &str) {
        db.conn()
            .execute(
                "UPDATE articles SET source_id = ?2 WHERE id = ?1",
                rusqlite::params![id, source],
            )
            .unwrap();
    }

    /// 一覧では、同じ報道のグループを推薦点の最も高い 1 件にまとめ、ほかの記事のソースを添える。
    /// 件数の上限はまとめた後の件数に掛ける。
    #[test]
    fn list_folds_a_story_into_its_best_article() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(&db, "https://e.com/a", Lang::En, "2026-09-25T00:00:00Z", 70);
        let b = scored_article(&db, "https://e.com/b", Lang::En, "2026-09-26T00:00:00Z", 90);
        let c = scored_article(&db, "https://e.com/c", Lang::En, "2026-09-24T00:00:00Z", 80);
        let other = scored_article(&db, "https://e.com/o", Lang::En, "2026-09-24T00:00:00Z", 65);
        set_source(&db, a, "wnn");
        set_source(&db, c, "jaif");
        group(&db, &[a, b, c]);
        assert_eq!(list_ids(&db, false), [b, other]);
        let items = db.list_articles(list_query(&db, false)).unwrap();
        assert_eq!(items[0].story_id, a);
        // 日時の順
        assert_eq!(items[0].story_others, ["jaif", "wnn"]);
        assert_eq!(items[1].story_id, other);
        assert!(items[1].story_others.is_empty());
        let limited: Vec<i64> = db
            .list_articles(ListQuery {
                limit: 2,
                ..list_query(&db, false)
            })
            .unwrap()
            .into_iter()
            .map(|i| i.article_id)
            .collect();
        assert_eq!(limited, [b, other]);
    }

    /// embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）一覧では使わない。
    #[test]
    fn list_ignores_embedding_scores_for_now() {
        let db = Db::open_in_memory().unwrap();
        let a = embedding_scored_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z", 90);
        let item = db
            .list_articles(list_query(&db, true))
            .unwrap()
            .into_iter()
            .find(|i| i.article_id == a)
            .unwrap();
        assert_eq!((item.score, item.llm_score), (None, None));
    }

    /// 未読だけの一覧では、グループのどれかを読んだらグループごと出さない。評価 1〜2 も同じ。
    #[test]
    fn list_hides_a_story_read_or_rated_low_anywhere() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(&db, "https://e.com/a", Lang::En, "2026-09-25T00:00:00Z", 70);
        let b = scored_article(&db, "https://e.com/b", Lang::En, "2026-09-26T00:00:00Z", 90);
        let c = scored_article(&db, "https://e.com/c", Lang::En, "2026-09-25T00:00:00Z", 70);
        let d = scored_article(&db, "https://e.com/d", Lang::En, "2026-09-26T00:00:00Z", 85);
        group(&db, &[a, b]);
        group(&db, &[c, d]);
        let user = db.owner_id().unwrap();
        let unread = |db: &Db| -> Vec<i64> {
            db.list_articles(ListQuery {
                read: Some(false),
                ..list_query(db, false)
            })
            .unwrap()
            .into_iter()
            .map(|i| i.article_id)
            .collect()
        };
        assert_eq!(unread(&db), [b, d]);
        db.set_read(user, a, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert_eq!(unread(&db), [d]);
        db.rate(
            user,
            c,
            Some(Rating::new(1).unwrap()),
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();
        assert!(unread(&db).is_empty());
        // 「すべて」では隠さない
        assert_eq!(list_ids(&db, true).len(), 2);
    }

    /// 検索ではまとめない（グループの記事を全部出し、ほかの記事のソースは添える）。
    #[test]
    fn search_keeps_every_article_of_a_story() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(&db, "https://e.com/a", Lang::En, "2026-09-25T00:00:00Z", 70);
        let b = scored_article(&db, "https://e.com/b", Lang::En, "2026-09-26T00:00:00Z", 90);
        group(&db, &[a, b]);
        let items = db.search_articles(&search_query(&db)).unwrap();
        let mut ids: Vec<i64> = items.iter().map(|i| i.article_id).collect();
        ids.sort_unstable();
        assert_eq!(ids, [a, b]);
        assert!(items.iter().all(|i| i.story_others == ["s"]));
    }

    /// 選んだ後にグループのほかの記事を評価したら、その日の確認枠からも外す（記事自身の評価と同じ）。
    /// 読んだことは印として返し、一覧と同じく画面で絞る。
    #[test]
    fn explore_drops_picks_whose_story_was_rated_later() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(&db, "https://e.com/a", Lang::En, "2026-09-25T00:00:00Z", 30);
        let b = scored_article(&db, "https://e.com/b", Lang::En, "2026-09-26T00:00:00Z", 90);
        let c = scored_article(&db, "https://e.com/c", Lang::En, "2026-09-25T00:00:00Z", 30);
        let d = scored_article(&db, "https://e.com/d", Lang::En, "2026-09-26T00:00:00Z", 90);
        let user = db.owner_id().unwrap();
        let picks = |db: &Db| -> Vec<(i64, bool)> {
            db.explore(list_query(db, false), 5, "2026-09-27")
                .unwrap()
                .into_iter()
                .map(|i| (i.article_id, i.story_read))
                .collect()
        };
        assert_eq!(picks(&db).len(), 2);
        group(&db, &[a, b]);
        group(&db, &[c, d]);
        db.set_read(user, d, true, t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.rate(
            user,
            b,
            Some(Rating::new(4).unwrap()),
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        assert_eq!(picks(&db), [(c, true)]);
    }

    /// 確認枠でも 1 グループ 1 件にし、読んだグループは選ばない。
    #[test]
    fn explore_picks_one_article_per_unread_story() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(&db, "https://e.com/a", Lang::En, "2026-09-25T00:00:00Z", 30);
        let b = scored_article(&db, "https://e.com/b", Lang::En, "2026-09-26T00:00:00Z", 40);
        let c = scored_article(&db, "https://e.com/c", Lang::En, "2026-09-25T00:00:00Z", 30);
        let d = scored_article(&db, "https://e.com/d", Lang::En, "2026-09-26T00:00:00Z", 40);
        group(&db, &[a, b]);
        group(&db, &[c, d]);
        let user = db.owner_id().unwrap();
        db.set_read(user, c, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let picks = db.explore(list_query(&db, false), 5, "2026-09-27").unwrap();
        let ids: Vec<i64> = picks.iter().map(|i| i.article_id).collect();
        assert_eq!(ids.len(), 1, "{ids:?}");
        assert!([a, b].contains(&ids[0]), "{ids:?}");
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
        db.rate(owner, disliked, Rating::new(2), t("2026-09-27T00:00:00Z"))
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
        assert_eq!(d.rating, Rating::new(2));
        let h = items.iter().find(|i| i.article_id == high).unwrap();
        // 評価 2 の記事と特徴（ソース・トピック）を共有するので、推薦点は LLM の点数から少し下がる
        assert!(h.score.is_some_and(|s| s < 90), "{h:?}");
        assert_eq!(
            (h.llm_score, h.title_ja.as_deref(), h.read_at.as_deref()),
            (Some(90), Some("題"), None)
        );
    }

    /// 最低点なし（プロファイルの無い利用者の既定）は推薦点で絞らず、未採点も新しい順に出す。
    /// 評価 1〜2 と軽水炉と無関係の記事を隠すのは、最低点があるときと同じ。
    #[test]
    fn list_without_a_score_floor_shows_unscored_articles_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let digested = |url: &str, relevant: bool, at: &str| {
            let id = page_article(&db, url, at);
            add_digest(&db, id, "sonnet", "題", relevant, "2026-09-27T00:00:00Z");
            id
        };
        let older = digested("https://e.com/older", true, "2026-09-25T00:00:00.000Z");
        let newer = digested("https://e.com/newer", true, "2026-09-26T00:00:00.000Z");
        digested("https://e.com/unrelated", false, "2026-09-26T00:00:00.000Z");
        let disliked = digested("https://e.com/down", true, "2026-09-26T00:00:00.000Z");
        db.rate(owner, disliked, Rating::new(2), t("2026-09-27T00:00:00Z"))
            .unwrap();
        // 要約前の記事は、軽水炉と関係があるか分からないので出さない（最低点があるときと同じ）
        page_article(&db, "https://e.com/raw", "2026-09-26T00:00:00.000Z");

        let ids: Vec<i64> = db
            .list_articles(ListQuery {
                profile_hash: None,
                min_score: None,
                ..list_query(&db, false)
            })
            .unwrap()
            .into_iter()
            .map(|i| i.article_id)
            .collect();
        assert_eq!(ids, [newer, older]);
    }

    /// 未読だけの一覧は、件数の上限より前に既読を除く（上位が既読で埋まっても、下の未読が出る）。
    #[test]
    fn unread_list_filters_before_the_limit() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let read = scored_article(
            &db,
            "https://e.com/read",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            95,
        );
        let unread = scored_article(
            &db,
            "https://e.com/unread",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            80,
        );
        db.set_read(owner, read, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let q = |unread: bool| ListQuery {
            limit: 1,
            read: unread.then_some(false),
            ..list_query(&db, false)
        };
        let ids = |q| {
            db.list_articles(q)
                .unwrap()
                .into_iter()
                .map(|i| i.article_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(q(false)), [read]);
        assert_eq!(ids(q(true)), [unread]);
        // 既読だけ・ブックマークの有無でも、上限より前に絞る
        let only = |read: Option<bool>, bookmarked: Option<bool>| ListQuery {
            read,
            bookmarked,
            ..list_query(&db, false)
        };
        assert_eq!(ids(only(Some(true), None)), [read]);
        db.set_bookmark(owner, unread, true, t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert_eq!(ids(only(None, Some(true))), [unread]);
        assert_eq!(ids(only(None, Some(false))), [read]);
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
        db.record_open(owner, a, OpenKind::Detail, t("2026-09-27T00:00:00Z"))
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
        assert_eq!(item.read_at.as_deref(), Some("2026-09-27T00:00:00.000Z"));
        assert!(item.translation_requested);
        assert!(!item.has_translation);
        assert_eq!(item.locked_by, ["日本原子力学会"]);
    }

    /// 原文へ移るときは、記事の URL だけを読む（無い記事は None）。
    #[test]
    fn article_url_reads_only_the_url() {
        let db = Db::open_in_memory().unwrap();
        let id = page_article(&db, "https://e.com/a?x=1", "2026-09-26T00:00:00.000Z");
        assert_eq!(
            db.article_url(id).unwrap().as_deref(),
            Some("https://e.com/a?x=1")
        );
        assert_eq!(db.article_url(id + 1).unwrap(), None);
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
        db.rate(owner, liked, Rating::new(4), t("2026-09-27T00:00:00Z"))
            .unwrap();
        let read = scored_article(
            &db,
            "https://e.com/read",
            Lang::En,
            "2026-09-03T00:00:00.000Z",
            70,
        );
        db.record_open(owner, read, OpenKind::Detail, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let disliked = scored_article(
            &db,
            "https://e.com/down",
            Lang::En,
            "2026-09-04T00:00:00.000Z",
            95,
        );
        db.rate(owner, disliked, Rating::new(2), t("2026-09-27T00:00:00Z"))
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
                rating: RatingFilter::AtLeast(Rating::new(4).unwrap()),
                ..search_query(&db)
            }),
            [liked]
        );
        // 評価の無い記事だけ
        assert_eq!(
            with(SearchQuery {
                rating: RatingFilter::Unrated,
                ..search_query(&db)
            }),
            [unscored, read, translated]
        );
        assert_eq!(
            with(SearchQuery {
                read: Some(false),
                ..search_query(&db)
            }),
            // 評価しただけの記事は未読
            [unscored, disliked, liked, translated]
        );
        // 既読だけ
        assert_eq!(
            with(SearchQuery {
                read: Some(true),
                ..search_query(&db)
            }),
            [read]
        );
        assert_eq!(
            with(SearchQuery {
                min_score: Some(60),
                ..search_query(&db)
            }),
            [disliked, read, translated]
        );
        // 一覧の既定と同じく隠す：評価 1〜2・未採点・閾値未満
        assert_eq!(
            with(SearchQuery {
                hide: true,
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
    fn add_title(db: &Db, article_id: i64, title_ja: &str) {
        db.insert_artifact(
            &NewArtifact {
                article_id,
                kind: ArtifactKind::Title,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &serde_json::json!({ "title_ja": title_ja }),
                inputs: &[],
                glossary_at: None,
            },
            t("2026-09-26T03:00:00Z"),
        )
        .unwrap();
    }

    /// 要約の無い記事は見出しの和訳を見出しに使い、要約ができれば要約の見出しを使う。
    /// 見出しの和訳も検索に当たる。
    #[test]
    fn titles_fall_back_to_the_title_translation() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let only_title = dated_article(&db, "https://e.com/t", "IAEA News", "2026-09-20T00:00:00Z");
        add_title(&db, only_title, "見出しの和訳");
        let digested = dated_article(&db, "https://e.com/d", "WNN", "2026-09-19T00:00:00Z");
        add_title(&db, digested, "先に訳した見出し");
        add_digest(
            &db,
            digested,
            "sonnet",
            "要約の見出し",
            false,
            "2026-09-27T00:00:00Z",
        );
        let items = db.search_articles(&search_query(&db)).unwrap();
        let title_of = |id| {
            items
                .iter()
                .find(|i| i.article_id == id)
                .and_then(|i| i.title_ja.clone())
        };
        assert_eq!(title_of(only_title).as_deref(), Some("見出しの和訳"));
        assert_eq!(title_of(digested).as_deref(), Some("要約の見出し"));
        let detail = db
            .article_detail(owner, Some("h1"), only_title)
            .unwrap()
            .unwrap();
        assert_eq!(detail.item.title_ja.as_deref(), Some("見出しの和訳"));
        assert_eq!(search_ids(&db, &["見出しの和訳"]), [only_title]);
    }

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

    /// 点数が当たったプロファイルの語を、一覧の項目に付ける。
    #[test]
    fn list_carries_the_terms_the_score_matched() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let d = add_digest(&db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        db.insert_score_with_matches(
            score_key(&db),
            d,
            90,
            Some("理由"),
            ScoreMatches {
                interests: &["燃料".into(), "規制・審査".into()],
                excludes: &["核融合".into()],
            },
            t("2026-09-26T02:00:00Z"),
        )
        .unwrap();
        let item = &db.list_articles(list_query(&db, true)).unwrap()[0];
        let mut matched = item.matched.clone();
        matched.sort();
        assert_eq!(matched, ["燃料", "規制・審査"]);
        assert_eq!(item.excluded, ["核融合"]);
        // 未採点なら空
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        add_digest(&db, b, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        let items = db.list_articles(list_query(&db, true)).unwrap();
        let unscored = items.iter().find(|i| i.article_id == b).unwrap();
        assert!(unscored.matched.is_empty() && unscored.excluded.is_empty());
    }

    /// 確認枠は閾値未満・軽水炉・採点済み・未評価・未読・未選択の記事から選び、同じ日は同じ記事を返す。
    #[test]
    fn explore_picks_below_threshold_articles_once() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let low: Vec<i64> = (0..3)
            .map(|i| {
                scored_article(
                    &db,
                    &format!("https://e.com/low{i}"),
                    Lang::En,
                    "2026-09-26T00:00:00.000Z",
                    20,
                )
            })
            .collect();
        // 閾値以上、評価済み、既読の記事は選ばない
        scored_article(
            &db,
            "https://e.com/high",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let reacted = scored_article(
            &db,
            "https://e.com/r",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            20,
        );
        db.set_read(owner, reacted, true, t("2026-09-26T05:00:00Z"))
            .unwrap();
        let rated = scored_article(
            &db,
            "https://e.com/rated",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            20,
        );
        db.rate(owner, rated, Rating::new(3), t("2026-09-26T05:00:00Z"))
            .unwrap();
        let q = list_query(&db, false);
        let ids = |items: Vec<ListItem>| -> Vec<i64> {
            let mut ids: Vec<i64> = items.into_iter().map(|i| i.article_id).collect();
            ids.sort();
            ids
        };

        let first = ids(db.explore(q, 2, "2026-09-27").unwrap());
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|id| low.contains(id)), "{first:?}");
        // 同じ日は同じ記事
        assert_eq!(ids(db.explore(q, 2, "2026-09-27").unwrap()), first);
        // 次の日は、まだ選んでいない記事だけから選ぶ（残りは 1 件）
        let second = ids(db.explore(q, 2, "2026-09-28").unwrap());
        assert_eq!(second.len(), 1);
        assert!(!first.contains(&second[0]));
        // 評価が付いた記事は枠から消える（ブックマークは評価ではないので残る）
        db.set_bookmark(owner, first[1], true, t("2026-09-27T06:00:00Z"))
            .unwrap();
        db.rate(owner, first[0], Rating::new(2), t("2026-09-27T06:00:00Z"))
            .unwrap();
        assert_eq!(ids(db.explore(q, 2, "2026-09-27").unwrap()), [first[1]]);
        // 0 件なら選ばない
        assert!(db.explore(q, 0, "2026-09-29").unwrap().is_empty());
    }

    /// 選んだ後に採点し直されて閾値以上になった記事は、確認枠から外す（一覧と二重に出さない）。
    #[test]
    fn explore_drops_picks_that_no_longer_qualify() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            20,
        );
        let q = list_query(&db, false);
        let picked: Vec<i64> = db
            .explore(q, 1, "2026-09-27")
            .unwrap()
            .into_iter()
            .map(|i| i.article_id)
            .collect();
        assert_eq!(picked, [a]);
        rescore_with_version(&db, a, 2, 90);
        assert!(db.explore(q, 1, "2026-09-27").unwrap().is_empty());
    }
}
