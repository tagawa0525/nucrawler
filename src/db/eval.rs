//! オフライン評価（`nucrawler eval`）：明示的な反応を正解ラベルにして、採点と突き合わせる材料。

use super::*;

/// 記事の正解ラベル。残っている明示的な反応（up・down・bookmark・dismiss）のうち最後のもので決める。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub article_id: i64,
    /// ラベルを決めた反応
    pub kind: SignalKind,
    /// その反応の時刻
    pub at: String,
}

impl Label {
    /// up・bookmark は正例、down・dismiss は負例。
    pub fn positive(&self) -> bool {
        matches!(self.kind, SignalKind::Up | SignalKind::Bookmark)
    }
}

/// 採点のキー（どのプロファイル・バックエンド・モデル・プロンプトの版で採点したか）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EvalKey {
    pub profile_hash: String,
    pub backend: String,
    pub model: String,
    pub prompt_version: i64,
}

/// ラベルの付いた記事の、あるキーでの点数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledScore {
    pub key: EvalKey,
    pub article_id: i64,
    pub score: u8,
    /// 採点した時刻
    pub scored_at: String,
}

/// 正解ラベルに使う明示的な反応の種類（SQL の IN 句）
const EXPLICIT: &str = "('up', 'down', 'bookmark', 'dismiss')";

impl Db {
    /// 利用者の反応から決めた正解ラベル（article_id 順）。
    pub fn eval_labels(&self, user_id: i64) -> Result<Vec<Label>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT e.article_id, e.kind, e.created_at FROM events AS e
             WHERE e.user_id = ?1 AND e.kind IN {EXPLICIT}
               AND NOT EXISTS (
                 SELECT 1 FROM events AS f
                 WHERE f.user_id = e.user_id AND f.article_id = e.article_id
                   AND f.kind IN {EXPLICIT}
                   AND (f.created_at > e.created_at
                        OR (f.created_at = e.created_at AND f.id > e.id)))
             ORDER BY e.article_id"
        ))?;
        let rows = stmt.query_map([user_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (article_id, kind, at) = row?;
            Ok(Label {
                article_id,
                kind: SignalKind::parse(&kind)?,
                at,
            })
        })
        .collect()
    }

    /// ラベルの付いた記事の点数。キーごとに、そのキーで採点された最新の digest の点数を使う
    /// （キー・article_id 順）。
    pub fn eval_scores(&self, user_id: i64) -> Result<Vec<LabeledScore>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH labeled AS (
               SELECT DISTINCT article_id FROM events
               WHERE user_id = ?1 AND kind IN {EXPLICIT}),
             ranked AS (
               SELECT s.profile_hash, s.backend, s.model, s.prompt_version, r.article_id,
                      s.score, s.created_at,
                      row_number() OVER (
                        PARTITION BY s.profile_hash, s.backend, s.model, s.prompt_version,
                                     r.article_id
                        ORDER BY r.created_at DESC, r.id DESC) AS rn
               FROM scores AS s
               JOIN artifacts AS r ON r.id = s.artifact_id
               JOIN labeled AS l ON l.article_id = r.article_id
               WHERE s.user_id = ?1)
             SELECT profile_hash, backend, model, prompt_version, article_id, score, created_at
             FROM ranked WHERE rn = 1
             ORDER BY profile_hash, backend, model, prompt_version, article_id"
        ))?;
        let rows = stmt.query_map([user_id], |r| {
            Ok(LabeledScore {
                key: EvalKey {
                    profile_hash: r.get(0)?,
                    backend: r.get(1)?,
                    model: r.get(2)?,
                    prompt_version: r.get(3)?,
                },
                article_id: r.get(4)?,
                score: r.get(5)?,
                scored_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    #[test]
    fn labels_come_from_the_last_explicit_reaction() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        let c = page_article(&db, "https://e.com/c", "2026-09-26T00:00:00.000Z");
        let d = page_article(&db, "https://e.com/d", "2026-09-26T00:00:00.000Z");
        let event = |article, kind, at| db.record_event(owner, article, kind, t(at)).unwrap();
        // 見送った後でブックマークした → 正例
        event(a, SignalKind::Dismiss, "2026-09-27T00:00:00Z");
        event(a, SignalKind::Bookmark, "2026-09-27T01:00:00Z");
        // 開いただけ → ラベルなし
        event(b, SignalKind::OpenDetail, "2026-09-27T00:00:00Z");
        // 👍 を取り消して 👎 → 負例
        event(c, SignalKind::Up, "2026-09-27T00:00:00Z");
        db.undo_event(owner, c, SignalKind::Up).unwrap();
        event(c, SignalKind::Down, "2026-09-27T02:00:00Z");
        // ブックマークを外しても、ブックマークした反応は残る → 正例
        event(d, SignalKind::Bookmark, "2026-09-27T00:00:00Z");
        db.unbookmark(owner, d).unwrap();
        // 別の利用者の反応は使わない
        let other = db
            .conn()
            .query_row(
                "INSERT INTO users (login, display_name) VALUES ('other', 'other') RETURNING id",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        db.record_event(other, b, SignalKind::Up, t("2026-09-27T00:00:00Z"))
            .unwrap();

        let labels = db.eval_labels(owner).unwrap();
        let summary: Vec<_> = labels
            .iter()
            .map(|l| (l.article_id, l.positive(), l.at.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (a, true, "2026-09-27T01:00:00.000Z"),
                (c, false, "2026-09-27T02:00:00.000Z"),
                (d, true, "2026-09-27T00:00:00.000Z"),
            ]
        );
    }

    #[test]
    fn scores_use_the_latest_digest_scored_by_each_key() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let key = score_key(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let old = add_digest(&db, a, "haiku", "古い版", true, "2026-09-26T01:00:00Z");
        let new = add_digest(&db, a, "sonnet", "新しい版", true, "2026-09-26T02:00:00Z");
        db.insert_score(key, old, 30, None, t("2026-09-26T03:00:00Z"))
            .unwrap();
        db.insert_score(key, new, 80, None, t("2026-09-26T04:00:00Z"))
            .unwrap();
        let v2 = ScoreKey {
            prompt_version: 2,
            ..key
        };
        db.insert_score(v2, old, 60, None, t("2026-09-26T05:00:00Z"))
            .unwrap();
        // ラベルの無い記事の点数は返さない
        let unlabeled = page_article(&db, "https://e.com/u", "2026-09-26T00:00:00.000Z");
        let u = add_digest(&db, unlabeled, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        db.insert_score(key, u, 90, None, t("2026-09-26T03:00:00Z"))
            .unwrap();
        db.record_event(owner, a, SignalKind::Up, t("2026-09-27T00:00:00Z"))
            .unwrap();

        let scores = db.eval_scores(owner).unwrap();
        let summary: Vec<_> = scores
            .iter()
            .map(|s| {
                (
                    s.key.prompt_version,
                    s.article_id,
                    s.score,
                    s.scored_at.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (1, a, 80, "2026-09-26T04:00:00.000Z"),
                (2, a, 60, "2026-09-26T05:00:00.000Z"),
            ]
        );
        assert_eq!(
            scores[0].key,
            EvalKey {
                profile_hash: "h1".into(),
                backend: "claude-cli".into(),
                model: "sonnet".into(),
                prompt_version: 1,
            }
        );
    }
}
