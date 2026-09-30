//! 同じ報道の判定（story）：判定する記事、比べる記事のプール、判定の保存とグループの作り直し。

use std::collections::HashMap;

use super::*;
use crate::story::{Doc, Edge};

/// 判定した組の関係。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryRelation {
    /// 同じ出来事の報道
    Same,
    /// 同じ案件の別の出来事（続報など）
    Related,
}

impl StoryRelation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Related => "related",
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

/// 判定する記事。
#[derive(Debug, Clone, PartialEq)]
pub struct StoryPending {
    pub article_id: i64,
    /// 公開（無ければ取得）の日時
    pub at: chrono::DateTime<chrono::Utc>,
}

/// 記事（別名 `a`）の比べる文。最新の要約の見出しと要約、無ければ最新の見出しの和訳、
/// 無ければ日本語の原題（どれも無ければ NULL）。
const STORY_TEXT: &str = "coalesce(
       (SELECT concat_ws(char(10), r.title_ja, r.summary_ja) FROM artifacts AS r
        WHERE r.article_id = a.id AND r.kind = 'digest'
        ORDER BY r.created_at DESC, r.id DESC LIMIT 1),
       (SELECT r.title_ja FROM artifacts AS r
        WHERE r.article_id = a.id AND r.kind = 'title'
        ORDER BY r.created_at DESC, r.id DESC LIMIT 1),
       CASE WHEN a.lang = 'ja' THEN a.title END)";

fn parse_at(at: String) -> Result<chrono::DateTime<chrono::Utc>, DbError> {
    chrono::DateTime::parse_from_rfc3339(&at)
        .map(|t| t.to_utc())
        .map_err(|_| DbError::UnexpectedValue(format!("article time {at:?}")))
}

impl Db {
    /// 同じ報道を判定する記事（新しい順）：`cutoff` 以降の記事で、story がまだ無く、比べる文があるもの。
    /// 比べる文は要約か、要約されない記事（公開の本文が無い）の見出しの和訳か日本語の原題。
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
                   SELECT 1 FROM artifacts AS r WHERE r.article_id = a.id AND r.kind = 'digest')
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
        let rows = stmt.query_map([timestamp(from), timestamp(to)], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (article_id, source_id, at, text) = row?;
            Ok(Doc {
                article_id,
                source_id,
                at: parse_at(at)?,
                text,
            })
        })
        .collect()
    }

    /// 記事からグループの ID への対応。
    pub fn story_ids(&self) -> Result<HashMap<i64, i64>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT article_id, story_id FROM article_stories")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
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
    /// グループが大きくなりすぎるので捨てた組を返す。
    pub fn rebuild_stories(&self) -> Result<Vec<Edge>, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        let edges: Vec<Edge> = {
            let mut stmt = tx.prepare(
                "SELECT r.article_id, l.other_id, l.similarity
                 FROM story_links AS l
                 JOIN artifacts AS r ON r.id = l.artifact_id
                 WHERE l.relation = 'same'
                   AND r.id = (
                     SELECT r2.id FROM artifacts AS r2
                     WHERE r2.article_id = r.article_id AND r2.kind = 'story'
                     ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1)",
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
        tx.execute("DELETE FROM article_stories", [])?;
        for (article_id, story_id) in stories {
            tx.execute(
                "INSERT INTO article_stories (article_id, story_id) VALUES (?1, ?2)",
                [article_id, story_id],
            )?;
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
        let mut v: Vec<_> = db.story_ids().unwrap().into_iter().collect();
        v.sort_unstable();
        v
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
