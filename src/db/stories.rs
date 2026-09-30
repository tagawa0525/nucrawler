//! 同じ報道の判定（story）：判定する記事、比べる記事のプール、判定の保存とグループの作り直し。

use super::*;
use crate::story::{Doc, Edge, Stories};

/// 判定した組の関係。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryRelation {
    /// 同じ出来事の報道
    Same,
    /// 同じ案件の別の出来事（続報など）
    Related,
    /// 候補にしたが、どちらでもない
    Unrelated,
}

impl StoryRelation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Related => "related",
            Self::Unrelated => "unrelated",
        }
    }
}

/// 判定した組（判定した記事から見た相手）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StoryLink {
    pub other_id: i64,
    pub relation: StoryRelation,
    pub similarity: f64,
}

/// 詳細に並べる、同じ報道・関連の記事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryArticle {
    pub article_id: i64,
    pub source_id: String,
    /// 公開（無ければ取得）の日時
    pub at: String,
    /// 原題
    pub title: String,
    /// 最新の要約の見出し、無ければ見出しの和訳
    pub title_ja: Option<String>,
    /// 同じグループのほかの記事の数（関連の記事をグループごとにまとめたとき）
    pub others: usize,
}

/// 判定する記事。
#[derive(Debug, Clone, PartialEq)]
pub struct StoryPending {
    pub article_id: i64,
    /// 公開（無ければ取得）の日時
    pub at: chrono::DateTime<chrono::Utc>,
}

/// 記事（別名 `a`）の比べる文。最新の公開の要約の見出しと要約、無ければ最新の見出しの和訳、
/// 無ければ日本語の原題（どれも無ければ NULL）。判定は入力の無い公開の成果物として残すので、
/// 会員限定の本文から作った要約は使わない。
const STORY_TEXT: &str = "coalesce(
       (SELECT concat_ws(char(10), r.title_ja, r.summary_ja) FROM artifacts AS r
        WHERE r.article_id = a.id AND r.kind = 'digest' AND r.input_scope = 'public'
        ORDER BY r.created_at DESC, r.id DESC LIMIT 1),
       (SELECT r.title_ja FROM artifacts AS r
        WHERE r.article_id = a.id AND r.kind = 'title'
        ORDER BY r.created_at DESC, r.id DESC LIMIT 1),
       CASE WHEN a.lang = 'ja' THEN a.title END)";

/// `id, source_id, at, text` の行。
struct DocRow(i64, String, String, String);

fn doc_row(r: &rusqlite::Row) -> rusqlite::Result<DocRow> {
    Ok(DocRow(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
}

impl TryFrom<DocRow> for Doc {
    type Error = DbError;

    fn try_from(DocRow(article_id, source_id, at, text): DocRow) -> Result<Doc, DbError> {
        Ok(Doc {
            article_id,
            source_id,
            at: parse_at(at)?,
            text,
        })
    }
}

fn parse_at(at: String) -> Result<chrono::DateTime<chrono::Utc>, DbError> {
    chrono::DateTime::parse_from_rfc3339(&at)
        .map(|t| t.to_utc())
        .map_err(|_| DbError::UnexpectedValue(format!("article time {at:?}")))
}

impl Db {
    /// 同じ報道を判定する記事（新しい順）：`cutoff` 以降の記事で、story がまだ無く、比べる文があるもの。
    /// 比べる文は公開の要約か、要約されない記事（公開の本文が無い）の見出しの和訳か日本語の原題。
    /// 要約を待っている記事は、要約ができてから判定する。
    /// `backend`/`model` の story の失敗で再試行待ち・断念済みの記事と、予約済みの記事は含めない。
    pub fn pending_stories(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        backend: &str,
        model: &str,
        limit: usize,
    ) -> Result<Vec<StoryPending>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, coalesce(a.published_at, a.fetched_at) FROM articles AS a
             WHERE coalesce(a.published_at, a.fetched_at) >= ?1
               AND NOT EXISTS (
                 SELECT 1 FROM artifacts AS r WHERE r.article_id = a.id AND r.kind = 'story')
               AND (
                 EXISTS (
                   SELECT 1 FROM artifacts AS r
                   WHERE r.article_id = a.id AND r.kind = 'digest' AND r.input_scope = 'public')
                 OR (
                   NOT EXISTS (
                     SELECT 1 FROM contents AS c
                     WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext')
                       AND c.access_membership_id IS NULL)
                   AND (a.lang = 'ja' OR EXISTS (
                     SELECT 1 FROM artifacts AS r
                     WHERE r.article_id = a.id AND r.kind = 'title'))))
               AND NOT EXISTS (
                 SELECT 1 FROM stage_errors AS e
                 WHERE e.article_id = a.id AND e.stage = 'story'
                   AND e.backend = ?3 AND e.model = ?4
                   AND (e.attempts >= ?2 OR e.next_retry_at > ?5))
               AND NOT EXISTS (
                 SELECT 1 FROM work_claims AS w
                 WHERE w.article_id = a.id AND w.stage = 'story'
                   AND w.backend = ?3 AND w.model = ?4)
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                timestamp(cutoff),
                MAX_ATTEMPTS,
                backend,
                model,
                timestamp(now),
                i64::try_from(limit).unwrap_or(i64::MAX),
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?;
        rows.map(|row| {
            let (article_id, at) = row?;
            Ok(StoryPending {
                article_id,
                at: parse_at(at)?,
            })
        })
        .collect()
    }

    /// `from` から `to` までの記事のうち、比べる文があるもの（新しい順）。
    /// 文は最新の要約の見出しと要約、無ければ最新の見出しの和訳、無ければ日本語の原題。
    pub fn story_pool(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Doc>, DbError> {
        let sql = format!(
            "SELECT id, source_id, at, text FROM (
               SELECT a.id, a.source_id, coalesce(a.published_at, a.fetched_at) AS at,
                      {STORY_TEXT} AS text
               FROM articles AS a
               WHERE coalesce(a.published_at, a.fetched_at) BETWEEN ?1 AND ?2)
             WHERE text IS NOT NULL
             ORDER BY at DESC, id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([timestamp(from), timestamp(to)], doc_row)?;
        rows.map(|row| row?.try_into()).collect()
    }

    /// 指定した記事の比べる文（[`Db::story_pool`] と同じ文。文の無い記事は含めない）。
    pub fn story_docs(&self, ids: &[i64]) -> Result<Vec<Doc>, DbError> {
        let sql = format!(
            "SELECT id, source_id, at, text FROM (
               SELECT a.id, a.source_id, coalesce(a.published_at, a.fetched_at) AS at,
                      {STORY_TEXT} AS text
               FROM articles AS a
               WHERE a.id IN (SELECT value FROM json_each(?1)))
             WHERE text IS NOT NULL
             ORDER BY at DESC, id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([serde_json::to_string(ids)?], doc_row)?;
        rows.map(|row| row?.try_into()).collect()
    }

    /// 記事と同じ報道のグループのほかの記事（日時の順）。
    pub fn story_members(
        &self,
        user_id: i64,
        article_id: i64,
    ) -> Result<Vec<StoryArticle>, DbError> {
        self.story_articles(
            user_id,
            article_id,
            "SELECT s2.article_id FROM article_stories AS s1
             JOIN article_stories AS s2
               ON s2.story_id = s1.story_id AND s2.article_id <> s1.article_id
             WHERE s1.article_id = :article",
            "at ASC, id ASC",
        )
    }

    /// 記事の関連記事（新しい順）：記事のグループの誰かの最新の判定が same か related とした記事と、
    /// その逆向きのもの。同じグループの記事は除く。関連の記事がグループに入っていれば、グループごとに
    /// そのグループで最も新しい 1 件にまとめる（判定に出た記事でなくてもよい）。
    pub fn related_articles(
        &self,
        user_id: i64,
        article_id: i64,
    ) -> Result<Vec<StoryArticle>, DbError> {
        let linked = self.story_articles(
            user_id,
            article_id,
            "WITH mine AS (
               SELECT s2.article_id AS id FROM article_stories AS s1
               JOIN article_stories AS s2 ON s2.story_id = s1.story_id
               WHERE s1.article_id = :article
               UNION SELECT :article),
             latest AS (
               SELECT r.id, r.article_id FROM artifacts AS r
               WHERE r.kind = 'story' AND r.id = (
                 SELECT r2.id FROM artifacts AS r2
                 WHERE r2.article_id = r.article_id AND r2.kind = 'story'
                 ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1)),
             linked AS (
               SELECT l.other_id AS id FROM latest AS x
               JOIN story_links AS l ON l.artifact_id = x.id
               WHERE x.article_id IN (SELECT id FROM mine) AND l.relation IN ('same', 'related')
               UNION
               SELECT x.article_id FROM latest AS x
               JOIN story_links AS l ON l.artifact_id = x.id
               WHERE l.other_id IN (SELECT id FROM mine) AND l.relation IN ('same', 'related')),
             -- 関連の記事がグループに入っていれば、代表を選べるようグループ全員に広げる
             expanded AS (
               SELECT id FROM linked
               UNION
               SELECT s2.article_id FROM linked
               JOIN article_stories AS s1 ON s1.article_id = linked.id
               JOIN article_stories AS s2 ON s2.story_id = s1.story_id)
             SELECT id FROM expanded WHERE id NOT IN (SELECT id FROM mine)",
            "at DESC, id DESC",
        )?;
        // 関連の記事がグループに入っていれば、グループごとに新しい 1 件にまとめる
        let stories = self.stories()?;
        let mut seen = std::collections::HashSet::new();
        Ok(linked
            .into_iter()
            .filter(|a| seen.insert(stories.story_of(a.article_id)))
            .map(|a| StoryArticle {
                others: stories.members(stories.story_of(a.article_id)).len() - 1,
                ..a
            })
            .collect())
    }

    /// `ids_sql`（`:article` を使う、記事 ID の列を返す SQL）の記事を `order` の順に読む。
    fn story_articles(
        &self,
        user_id: i64,
        article_id: i64,
        ids_sql: &str,
        order: &str,
    ) -> Result<Vec<StoryArticle>, DbError> {
        let sql = format!(
            "SELECT id, source_id, at, title, title_ja FROM (
               SELECT a.id, a.source_id, coalesce(a.published_at, a.fetched_at) AS at, a.title,
                      coalesce(
                        nullif(trim((SELECT r.title_ja FROM artifacts AS r
                                     WHERE r.article_id = a.id AND r.kind = 'digest' AND {viewable}
                                     ORDER BY r.created_at DESC, r.id DESC LIMIT 1)), ''),
                        (SELECT r.title_ja FROM artifacts AS r
                         WHERE r.article_id = a.id AND r.kind = 'title'
                         ORDER BY r.created_at DESC, r.id DESC LIMIT 1)) AS title_ja
               FROM articles AS a
               WHERE a.id IN ({ids_sql}))
             ORDER BY {order}",
            viewable = super::read::viewable("r"),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {":user": user_id, ":article": article_id},
            |r| {
                Ok(StoryArticle {
                    article_id: r.get(0)?,
                    source_id: r.get(1)?,
                    at: r.get(2)?,
                    title: r.get(3)?,
                    title_ja: r.get(4)?,
                    others: 0,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 記事からグループの ID への対応（2 件以上のグループの記事だけを部分索引で読む）。
    pub fn stories(&self) -> Result<Stories, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT article_id, story_id FROM article_stories WHERE story_id <> article_id
             UNION
             SELECT story_id, story_id FROM article_stories WHERE story_id <> article_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(Stories::new(rows.collect::<Result<Vec<_>, _>>()?))
    }

    /// 判定（kind = story）と、その組を 1 つのトランザクションで登録する。
    pub fn insert_story(
        &self,
        artifact: &NewArtifact,
        links: &[StoryLink],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        let id = super::artifacts::write_artifact(&tx, artifact, now)?;
        for link in links {
            tx.execute(
                "INSERT INTO story_links (artifact_id, other_id, relation, similarity)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, link.other_id, link.relation.as_str(), link.similarity],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// 記事ごとの最新の判定の same の組をつないで、グループ（`article_stories`）を作り直す。
    /// 今のグループと比べ、グループの ID が変わる記事の行だけを書く（外れた記事は自分の ID に戻す）。
    /// 相手の最新の判定が同じ組を same 以外（related・unrelated）にしていれば、判定が割れたので
    /// つながない。グループが大きくなりすぎるので捨てた組を返す。
    pub fn rebuild_stories(&self) -> Result<Vec<Edge>, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        let edges: Vec<Edge> = {
            let mut stmt = tx.prepare(
                "WITH latest AS (
                   SELECT r.article_id, l.other_id, l.relation, l.similarity
                   FROM story_links AS l
                   JOIN artifacts AS r ON r.id = l.artifact_id
                   WHERE r.id = (
                     SELECT r2.id FROM artifacts AS r2
                     WHERE r2.article_id = r.article_id AND r2.kind = 'story'
                     ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1))
                 SELECT x.article_id, x.other_id, x.similarity FROM latest AS x
                 WHERE x.relation = 'same'
                   AND NOT EXISTS (
                     SELECT 1 FROM latest AS y
                     WHERE y.article_id = x.other_id AND y.other_id = x.article_id
                       AND y.relation <> 'same')",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(Edge {
                    a: r.get(0)?,
                    b: r.get(1)?,
                    similarity: r.get(2)?,
                })
            })?;
            rows.collect::<Result<_, _>>()?
        };
        let (stories, rejected) = crate::story::components(&edges, crate::story::MAX_STORY_SIZE);
        let current = self.stories()?;
        let touched: std::collections::BTreeSet<i64> = current
            .grouped()
            .into_iter()
            .chain(stories.grouped())
            .map(|(id, _)| id)
            .collect();
        for id in touched {
            let new = stories.story_of(id);
            if current.story_of(id) != new {
                tx.execute(
                    "UPDATE article_stories SET story_id = ?2 WHERE article_id = ?1",
                    [id, new],
                )?;
            }
        }
        tx.commit()?;
        Ok(rejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    const CUTOFF: &str = "2026-09-01T00:00:00Z";
    const NOW: &str = "2026-09-30T00:00:00Z";

    fn pending(db: &Db) -> Vec<i64> {
        db.pending_stories(t(CUTOFF), t(NOW), "claude-cli", "sonnet", 100)
            .unwrap()
            .into_iter()
            .map(|p| p.article_id)
            .collect()
    }

    fn ja_article(db: &Db, url: &str, published: &str) -> i64 {
        db.insert_article(&NewArticle {
            lang: Lang::Ja,
            title: "日本語の原題",
            published_at: Some(published),
            ..article(url)
        })
        .unwrap()
        .unwrap()
    }

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
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();
    }

    fn story(db: &Db, article_id: i64, links: &[StoryLink], at: &str) -> i64 {
        story_version(db, article_id, links, at, 1)
    }

    fn story_version(
        db: &Db,
        article_id: i64,
        links: &[StoryLink],
        at: &str,
        prompt_version: i64,
    ) -> i64 {
        db.insert_story(
            &NewArtifact {
                article_id,
                kind: ArtifactKind::Story,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version,
                payload: &serde_json::json!({"candidates": [], "same": [], "related": []}),
                inputs: &[],
                glossary_at: None,
            },
            links,
            t(at),
        )
        .unwrap()
    }

    fn same(other_id: i64) -> StoryLink {
        StoryLink {
            other_id,
            relation: StoryRelation::Same,
            similarity: 0.5,
        }
    }

    fn stories(db: &Db) -> Vec<(i64, i64)> {
        db.stories().unwrap().grouped()
    }

    #[test]
    fn pending_stories_waits_for_text_to_compare() {
        let db = Db::open_in_memory().unwrap();
        let digested = page_article(&db, "https://e.com/digested", "2026-09-26T00:00:00.000Z");
        add_digest(
            &db,
            digested,
            "sonnet",
            "要約の見出し",
            true,
            "2026-09-27T00:00:00Z",
        );
        // 本文があり、要約を待っている
        let waiting = page_article(&db, "https://e.com/waiting", "2026-09-26T00:00:00.000Z");
        db.insert_content(waiting, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        // 本文が無い英語の記事：見出しの和訳があれば判定し、無ければ待つ
        let titled = page_article(&db, "https://e.com/titled", "2026-09-25T00:00:00.000Z");
        add_title(&db, titled, "見出しの和訳");
        let _untitled = page_article(&db, "https://e.com/untitled", "2026-09-25T00:00:00.000Z");
        // 本文が無い日本語の記事は原題で判定する
        let ja = ja_article(&db, "https://e.com/ja", "2026-09-24T00:00:00.000Z");
        // 期間より古い
        let old = page_article(&db, "https://e.com/old", "2026-08-01T00:00:00.000Z");
        add_digest(&db, old, "sonnet", "古い", true, "2026-09-27T00:00:00Z");
        assert_eq!(pending(&db), [digested, titled, ja]);

        story(&db, digested, &[], "2026-09-28T00:00:00Z");
        assert_eq!(pending(&db), [titled, ja]);
    }

    #[test]
    fn pending_stories_skips_backoff_and_claims() {
        let db = Db::open_in_memory().unwrap();
        let a = ja_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let b = ja_article(&db, "https://e.com/b", "2026-09-25T00:00:00.000Z");
        let key = StageKey {
            article_id: a,
            stage: "story",
            backend: "claude-cli",
            model: "sonnet",
        };
        db.record_stage_failure(key, "bad output", t("2026-09-29T23:30:00Z"), false)
            .unwrap();
        let _claim = db
            .claim(
                ClaimKey {
                    stage: "story",
                    backend: "claude-cli",
                    model: "sonnet",
                },
                &[b],
                t(NOW),
                chrono::Duration::minutes(10),
            )
            .unwrap();
        assert!(pending(&db).is_empty());
    }

    #[test]
    fn story_pool_reads_the_text_to_compare() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(&db, a, "sonnet", "古い要約", true, "2026-09-27T00:00:00Z");
        add_digest(&db, a, "opus", "新しい要約", true, "2026-09-28T00:00:00Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-25T00:00:00.000Z");
        add_title(&db, b, "見出しの和訳");
        let c = ja_article(&db, "https://e.com/c", "2026-09-24T00:00:00.000Z");
        // 比べる文が無い
        page_article(&db, "https://e.com/none", "2026-09-24T00:00:00.000Z");
        // 期間の外
        ja_article(&db, "https://e.com/out", "2026-09-01T00:00:00.000Z");
        let pool = db
            .story_pool(t("2026-09-20T00:00:00Z"), t("2026-09-30T00:00:00Z"))
            .unwrap();
        let got: Vec<(i64, &str, &str)> = pool
            .iter()
            .map(|d| (d.article_id, d.source_id.as_str(), d.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (a, "s", "新しい要約\n新しい要約の要約"),
                (b, "s", "見出しの和訳"),
                (c, "s", "日本語の原題"),
            ]
        );
        assert_eq!(pool[0].at, t("2026-09-26T00:00:00Z"));
    }

    /// 記事は追加したときから自分の ID のグループに入っている。記事からグループへの対応には、
    /// 2 件以上のグループの記事だけを載せる（ほかは自分のグループ）。
    #[test]
    fn every_article_starts_in_its_own_story() {
        let db = Db::open_in_memory().unwrap();
        let a = ja_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        assert_eq!(
            db.query_strings("SELECT article_id || '|' || story_id FROM article_stories")
                .unwrap(),
            [format!("{a}|{a}")]
        );
        assert!(db.stories().unwrap().grouped().is_empty());
    }

    /// 作り直しは変わった行だけを書き、グループから外れた記事は自分の ID に戻す。
    #[test]
    fn rebuild_stories_writes_only_what_changed() {
        let db = Db::open_in_memory().unwrap();
        let ids: Vec<i64> = (0..10)
            .map(|n| {
                ja_article(
                    &db,
                    &format!("https://e.com/{n}"),
                    "2026-09-26T00:00:00.000Z",
                )
            })
            .collect();
        let (a, b) = (ids[0], ids[1]);
        story(&db, a, &[same(b)], "2026-09-27T00:00:00Z");
        story(&db, b, &[same(a)], "2026-09-27T00:00:00Z");
        db.rebuild_stories().unwrap();
        let before = db.conn().total_changes();
        db.rebuild_stories().unwrap();
        assert_eq!(db.conn().total_changes(), before, "nothing changed");
        // b の新しい判定で割れたので、a と b はそれぞれ自分のグループに戻る
        story_version(
            &db,
            b,
            &[StoryLink {
                relation: StoryRelation::Related,
                ..same(a)
            }],
            "2026-09-28T00:00:00Z",
            2,
        );
        let before = db.conn().total_changes();
        db.rebuild_stories().unwrap();
        assert_eq!(db.conn().total_changes() - before, 1, "only b moves");
        assert_eq!(
            db.query_strings(&format!(
                "SELECT article_id || '|' || story_id FROM article_stories
                 WHERE article_id IN ({a}, {b}) ORDER BY article_id"
            ))
            .unwrap(),
            [format!("{a}|{a}"), format!("{b}|{b}")]
        );
    }

    /// 会員限定の本文から作った要約は、比べる文にも判定の条件にも使わない（判定は公開の成果物として
    /// 残すので）。
    #[test]
    fn stories_ignore_gated_digests() {
        let db = Db::open_in_memory().unwrap();
        let m = insert_membership(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let gated = insert_content(&db, a, Some(m));
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &serde_json::json!({"title_ja": "会員限定の要約", "summary_ja": "本文"}),
                inputs: &[gated],
                glossary_at: None,
            },
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();
        assert!(pending(&db).is_empty());
        let pool = db
            .story_pool(t("2026-09-20T00:00:00Z"), t("2026-09-30T00:00:00Z"))
            .unwrap();
        assert!(pool.is_empty(), "{pool:?}");
        assert!(db.story_docs(&[a]).unwrap().is_empty());
    }

    /// 指定した記事の比べる文（プールと同じ文。文の無い記事は含めない）。
    #[test]
    fn story_docs_reads_the_given_articles() {
        let db = Db::open_in_memory().unwrap();
        let a = ja_article(&db, "https://e.com/a", "2026-08-01T00:00:00.000Z");
        let none = page_article(&db, "https://e.com/none", "2026-08-01T00:00:00.000Z");
        let _other = ja_article(&db, "https://e.com/other", "2026-08-01T00:00:00.000Z");
        let docs = db.story_docs(&[a, none]).unwrap();
        let got: Vec<(i64, &str)> = docs
            .iter()
            .map(|d| (d.article_id, d.text.as_str()))
            .collect();
        assert_eq!(got, [(a, "日本語の原題")]);
    }

    fn article_on(db: &Db, n: i64, day: u32) -> i64 {
        ja_article(
            db,
            &format!("https://e.com/n{n}"),
            &format!("2026-09-{day:02}T00:00:00.000Z"),
        )
    }

    fn rel(other_id: i64) -> StoryLink {
        StoryLink {
            relation: StoryRelation::Related,
            ..same(other_id)
        }
    }

    fn ids_of(v: &[StoryArticle]) -> Vec<(i64, usize)> {
        v.iter().map(|a| (a.article_id, a.others)).collect()
    }

    #[test]
    fn story_members_lists_the_rest_of_the_story() {
        let db = Db::open_in_memory().unwrap();
        let a = article_on(&db, 1, 25);
        let b = article_on(&db, 2, 24);
        let c = article_on(&db, 3, 26);
        let alone = article_on(&db, 4, 26);
        story(&db, a, &[same(b), same(c)], "2026-09-27T00:00:00Z");
        story(&db, b, &[same(a)], "2026-09-27T00:00:00Z");
        story(&db, c, &[same(a)], "2026-09-27T00:00:00Z");
        db.rebuild_stories().unwrap();
        let user = db.owner_id().unwrap();
        let members = db.story_members(user, a).unwrap();
        assert_eq!(ids_of(&members), [(b, 0), (c, 0)]);
        assert_eq!(members[0].title, "日本語の原題");
        assert!(db.story_members(user, alone).unwrap().is_empty());
    }

    /// 関連のグループの代表は、判定に出た記事ではなく、そのグループで最も新しい記事にする。
    #[test]
    fn related_story_is_shown_by_its_newest_article() {
        let db = Db::open_in_memory().unwrap();
        let a = article_on(&db, 1, 20);
        let old = article_on(&db, 2, 21);
        let new = article_on(&db, 3, 25);
        for (p, q) in [(old, new), (new, old)] {
            story(&db, p, &[same(q)], "2026-09-27T00:00:00Z");
        }
        // 逆向きの関連は古い記事からだけ
        story_version(&db, old, &[same(new), rel(a)], "2026-09-28T00:00:00Z", 2);
        db.rebuild_stories().unwrap();
        let user = db.owner_id().unwrap();
        assert_eq!(ids_of(&db.related_articles(user, a).unwrap()), [(new, 1)]);
    }

    /// グループの誰かの判定の関連と、逆向きの関連を合わせる。グループにつながらなかった same の組
    /// （判定が割れた・上限を超えた）も関連として出し、無関係の組は出さない。関連がグループなら
    /// 1 件にまとめる。
    #[test]
    fn related_articles_gather_links_of_the_whole_story() {
        let db = Db::open_in_memory().unwrap();
        let a = article_on(&db, 1, 20);
        let b = article_on(&db, 2, 20);
        let x = article_on(&db, 3, 21);
        let y = article_on(&db, 4, 22);
        let z = article_on(&db, 5, 23);
        let w = article_on(&db, 6, 24);
        let v = article_on(&db, 7, 25);
        let unrelated = article_on(&db, 8, 26);
        // a と b が同じ報道、x と y が同じ報道
        for (p, q) in [(x, y), (y, x)] {
            story(&db, p, &[same(q)], "2026-09-27T00:00:00Z");
        }
        let unrelated_v = StoryLink {
            relation: StoryRelation::Unrelated,
            ..same(v)
        };
        story(
            &db,
            a,
            &[same(b), rel(x), rel(y), unrelated_v],
            "2026-09-27T00:00:00Z",
        );
        story(&db, b, &[same(a), rel(z)], "2026-09-27T00:00:00Z");
        // 逆向き：w が a を関連と判定した
        story(&db, w, &[rel(a)], "2026-09-27T00:00:00Z");
        // v は a を same としたが、a は v を無関係とした（判定が割れてつながらない）
        story(&db, v, &[same(a)], "2026-09-27T00:00:00Z");
        story(
            &db,
            unrelated,
            &[StoryLink {
                relation: StoryRelation::Unrelated,
                ..same(b)
            }],
            "2026-09-27T00:00:00Z",
        );
        db.rebuild_stories().unwrap();
        let user = db.owner_id().unwrap();
        // x・y は新しい y の 1 件にまとめる
        assert_eq!(
            ids_of(&db.related_articles(user, a).unwrap()),
            [(v, 0), (w, 0), (z, 0), (y, 1)]
        );
        // a と b は同じ日時なので、ID の大きい b が代表
        assert_eq!(ids_of(&db.related_articles(user, x).unwrap()), [(b, 1)]);
    }

    /// same の組を推移的につなぐ。記事ごとに最新の判定だけを使い、related はつながない。
    #[test]
    fn rebuild_stories_joins_the_latest_same_links() {
        let db = Db::open_in_memory().unwrap();
        let ids: Vec<i64> = (0..5)
            .map(|n| {
                ja_article(
                    &db,
                    &format!("https://e.com/{n}"),
                    "2026-09-26T00:00:00.000Z",
                )
            })
            .collect();
        let [a, b, c, d, e] = ids[..] else {
            unreachable!()
        };
        story(&db, a, &[same(b)], "2026-09-27T00:00:00Z");
        story(&db, c, &[same(b)], "2026-09-27T00:00:00Z");
        // d の古い判定は e と same だったが、新しいプロンプトの版の判定では related
        story(&db, d, &[same(e)], "2026-09-27T00:00:00Z");
        let related = StoryLink {
            relation: StoryRelation::Related,
            ..same(e)
        };
        let id = story_version(&db, d, &[related], "2026-09-28T00:00:00Z", 2);
        assert!(db.rebuild_stories().unwrap().is_empty());
        assert_eq!(stories(&db), [(a, a), (b, a), (c, a)]);
        assert_eq!(
            db.query_strings(&format!(
                "SELECT relation FROM story_links WHERE artifact_id = {id}"
            ))
            .unwrap(),
            ["related"]
        );

        // 作り直すと、消えた組のグループも消える
        db.conn()
            .execute("DELETE FROM articles WHERE id = ?1", [b])
            .unwrap();
        db.rebuild_stories().unwrap();
        assert!(stories(&db).is_empty());
    }

    /// 両方の向きで判定した組は、相手の最新の判定も same でなければつながない（判定が割れたら
    /// まとめない）。相手が判定していない（候補にしていない）ときは、片方の same でつなぐ。
    #[test]
    fn rebuild_stories_requires_both_directions_to_agree() {
        let db = Db::open_in_memory().unwrap();
        let ids: Vec<i64> = (0..6)
            .map(|n| {
                ja_article(
                    &db,
                    &format!("https://e.com/{n}"),
                    "2026-09-26T00:00:00.000Z",
                )
            })
            .collect();
        let [a, b, c, d, e, f] = ids[..] else {
            unreachable!()
        };
        let with = |other_id, relation| StoryLink {
            relation,
            ..same(other_id)
        };
        story(&db, a, &[same(b)], "2026-09-27T00:00:00Z");
        story(
            &db,
            b,
            &[with(a, StoryRelation::Related)],
            "2026-09-27T00:00:00Z",
        );
        story(&db, c, &[same(d)], "2026-09-27T00:00:00Z");
        story(
            &db,
            d,
            &[with(c, StoryRelation::Unrelated)],
            "2026-09-27T00:00:00Z",
        );
        story(&db, e, &[same(f)], "2026-09-27T00:00:00Z");
        story(&db, f, &[same(e)], "2026-09-27T00:00:00Z");
        db.rebuild_stories().unwrap();
        assert_eq!(stories(&db), [(e, e), (f, e)]);
    }

    #[test]
    fn rebuild_stories_returns_links_beyond_the_size_limit() {
        let db = Db::open_in_memory().unwrap();
        let n = crate::story::MAX_STORY_SIZE as i64 + 1;
        let ids: Vec<i64> = (0..n)
            .map(|i| {
                ja_article(
                    &db,
                    &format!("https://e.com/{i}"),
                    "2026-09-26T00:00:00.000Z",
                )
            })
            .collect();
        for w in ids.windows(2) {
            story(&db, w[1], &[same(w[0])], "2026-09-27T00:00:00Z");
        }
        let rejected = db.rebuild_stories().unwrap();
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert_eq!(stories(&db).len(), crate::story::MAX_STORY_SIZE);
    }
}
