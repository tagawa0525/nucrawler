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
        todo!("{cutoff} {now} {backend} {model} {limit}")
    }

    /// `from` から `to` までの記事のうち、比べる文があるもの（[`Db::pending_stories`] と同じ文）。
    /// 文は最新の要約の見出しと要約、無ければ最新の見出しの和訳、無ければ日本語の原題。
    pub fn story_pool(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Doc>, DbError> {
        todo!("{from} {to}")
    }

    /// 記事からグループの ID への対応。
    pub fn story_ids(&self) -> Result<HashMap<i64, i64>, DbError> {
        todo!()
    }

    /// 判定（kind = story）と、その組を 1 つのトランザクションで登録する。
    pub fn insert_story(
        &self,
        artifact: &NewArtifact,
        links: &[StoryLink],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        todo!("{artifact:?} {links:?} {now}")
    }

    /// 記事ごとの最新の判定の same の組をつないで、グループ（`article_stories`）を作り直す。
    /// グループが大きくなりすぎるので捨てた組を返す。
    pub fn rebuild_stories(&self) -> Result<Vec<Edge>, DbError> {
        todo!()
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
        db.insert_story(
            &NewArtifact {
                article_id,
                kind: ArtifactKind::Story,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
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
        // d の古い判定は e と same だったが、新しい判定では related
        story(&db, d, &[same(e)], "2026-09-27T00:00:00Z");
        let related = StoryLink {
            relation: StoryRelation::Related,
            ..same(e)
        };
        let id = story(&db, d, &[related], "2026-09-28T00:00:00Z");
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
