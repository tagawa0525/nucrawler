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

/// 利用者（`:user`）の記事ごとの正解ラベル：残っている明示的な反応のうち最後のもの。
fn labels() -> String {
    format!(
        "SELECT e.id, e.article_id, e.kind, e.created_at FROM events AS e
         WHERE e.user_id = :user AND e.kind IN {EXPLICIT}
           AND NOT EXISTS (
             SELECT 1 FROM events AS f
             WHERE f.user_id = e.user_id AND f.article_id = e.article_id
               AND f.kind IN {EXPLICIT}
               AND (f.created_at > e.created_at
                    OR (f.created_at = e.created_at AND f.id > e.id)))"
    )
}

/// プロファイルの見直しの根拠：ラベルの付いた記事の見出しとトピック。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub article_id: i64,
    /// 関心（up・bookmark）なら真、不要（down・dismiss）なら偽
    pub positive: bool,
    pub title_ja: String,
    pub topics: Vec<String>,
    /// ラベルを決めた反応の時刻
    pub at: String,
}

/// 確認枠（閾値未満から無作為に選んだ記事）の反応の内訳。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExploreStats {
    /// 確認枠に選んだ記事の数
    pub picked: usize,
    /// そのうち関心（up・bookmark）が最後の反応の記事
    pub positive: usize,
    /// そのうち不要（down・dismiss）が最後の反応の記事
    pub negative: usize,
}

impl Db {
    /// 利用者の反応から決めた正解ラベル（article_id 順）。
    pub fn eval_labels(&self, user_id: i64) -> Result<Vec<Label>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT article_id, kind, created_at FROM ({labels}) ORDER BY article_id",
            labels = labels(),
        ))?;
        let rows = stmt.query_map(rusqlite::named_params! {":user": user_id}, |r| {
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

    /// ラベルの付いた記事に、利用者が閲覧できる最新の digest の見出しとトピックを付けて、反応の
    /// 新しい順に返す。digest の無い記事は含めない。
    pub fn label_evidence(&self, user_id: i64) -> Result<Vec<Evidence>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH labels AS ({labels}),
             digests AS (
               SELECT l.id AS event_id, l.article_id, l.kind, l.created_at,
                      (SELECT r.id FROM artifacts AS r
                       WHERE r.article_id = l.article_id AND r.kind = 'digest' AND {viewable}
                       ORDER BY r.created_at DESC, r.id DESC LIMIT 1) AS digest_id
               FROM labels AS l)
             SELECT d.article_id, d.kind, d.created_at, r.title_ja, {topics}
             FROM digests AS d
             JOIN artifacts AS r ON r.id = d.digest_id
             ORDER BY d.created_at DESC, d.event_id DESC",
            labels = labels(),
            viewable = super::read::viewable("r"),
            topics = super::read::linked_topics("r"),
        ))?;
        let rows = stmt.query_map(rusqlite::named_params! {":user": user_id}, |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (article_id, kind, at, title_ja, topics) = row?;
            let kind = SignalKind::parse(&kind)?;
            Ok(Evidence {
                article_id,
                positive: matches!(kind, SignalKind::Up | SignalKind::Bookmark),
                title_ja,
                topics: serde_json::from_str(&topics)?,
                at,
            })
        })
        .collect()
    }

    /// 確認枠に選んだ記事と、そのラベルの内訳。
    pub fn explore_stats(&self, user_id: i64) -> Result<ExploreStats, DbError> {
        let picked: std::collections::HashSet<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT article_id FROM explore_picks WHERE user_id = ?1")?;
            let rows = stmt.query_map([user_id], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut stats = ExploreStats {
            picked: picked.len(),
            ..ExploreStats::default()
        };
        for label in self.eval_labels(user_id)? {
            if !picked.contains(&label.article_id) {
                continue;
            }
            if label.positive() {
                stats.positive += 1;
            } else {
                stats.negative += 1;
            }
        }
        Ok(stats)
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
               JOIN artifacts AS r ON r.id = s.artifact_id AND r.kind = 'digest'
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
    fn evidence_carries_the_latest_digest_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(&db, a, "haiku", "古い見出し", true, "2026-09-26T01:00:00Z");
        add_digest(
            &db,
            a,
            "sonnet",
            "新しい見出し",
            true,
            "2026-09-26T02:00:00Z",
        );
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        add_digest(
            &db,
            b,
            "sonnet",
            "見送った記事",
            true,
            "2026-09-26T01:00:00Z",
        );
        // digest の無い記事は根拠にできない
        let bare = page_article(&db, "https://e.com/bare", "2026-09-26T00:00:00.000Z");
        let event = |article, kind, at| db.record_event(owner, article, kind, t(at)).unwrap();
        event(a, SignalKind::Up, "2026-09-27T00:00:00Z");
        event(b, SignalKind::Dismiss, "2026-09-27T01:00:00Z");
        event(bare, SignalKind::Up, "2026-09-27T02:00:00Z");

        let evidence = db.label_evidence(owner).unwrap();
        assert_eq!(
            evidence,
            [
                Evidence {
                    article_id: b,
                    positive: false,
                    title_ja: "見送った記事".into(),
                    topics: vec!["規制・審査".into()],
                    at: "2026-09-27T01:00:00.000Z".into(),
                },
                Evidence {
                    article_id: a,
                    positive: true,
                    title_ja: "新しい見出し".into(),
                    topics: vec!["規制・審査".into()],
                    at: "2026-09-27T00:00:00.000Z".into(),
                },
            ]
        );
    }

    /// 会員限定の本文から作った digest は、会員でない利用者の根拠にしない（見出しを LLM に渡すため）。
    #[test]
    fn evidence_skips_digests_the_user_cannot_view() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let gated_digest = |article_id: i64, title: &str, at: &str| {
            let gated = insert_content(&db, article_id, Some(aesj));
            let payload = serde_json::json!({
                "title_ja": title, "summary_ja": "s", "points_ja": ["p"],
                "implications_ja": "", "lwr_relevant": true, "topics": ["燃料"],
            });
            db.insert_artifact(
                &NewArtifact {
                    article_id,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[gated],
                    glossary_at: None,
                },
                t(at),
            )
            .unwrap();
        };
        // 公開の digest より新しい会員限定の digest があっても、公開の方を使う
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(
            &db,
            a,
            "sonnet",
            "公開の見出し",
            true,
            "2026-09-26T01:00:00Z",
        );
        gated_digest(a, "会員限定の見出し", "2026-09-26T02:00:00Z");
        // 会員限定の digest しか無い記事は根拠にしない
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        gated_digest(b, "会員限定だけ", "2026-09-26T01:00:00Z");
        for article in [a, b] {
            db.record_event(owner, article, SignalKind::Up, t("2026-09-27T00:00:00Z"))
                .unwrap();
        }
        let titles: Vec<String> = db
            .label_evidence(owner)
            .unwrap()
            .into_iter()
            .map(|e| e.title_ja)
            .collect();
        assert_eq!(titles, ["公開の見出し"]);
    }

    #[test]
    fn explore_stats_count_reactions_to_picks() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let ids: Vec<i64> = (0..4)
            .map(|i| {
                page_article(
                    &db,
                    &format!("https://e.com/{i}"),
                    "2026-09-26T00:00:00.000Z",
                )
            })
            .collect();
        for &id in &ids[..3] {
            db.conn()
                .execute(
                    "INSERT INTO explore_picks VALUES (?1, ?2, '2026-09-27')",
                    [owner, id],
                )
                .unwrap();
        }
        let event = |article, kind| {
            db.record_event(owner, article, kind, t("2026-09-27T01:00:00Z"))
                .unwrap()
        };
        event(ids[0], SignalKind::Bookmark);
        event(ids[1], SignalKind::Dismiss);
        // 確認枠に選んでいない記事の反応は数えない
        event(ids[3], SignalKind::Up);
        assert_eq!(
            db.explore_stats(owner).unwrap(),
            ExploreStats {
                picked: 3,
                positive: 1,
                negative: 1,
            }
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
        // digest 以外の成果物に付いた点数は使わない（より新しくても）
        let c = insert_content(&db, a, None);
        let translation = db
            .insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind: ArtifactKind::Translation,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &serde_json::json!({ "body_ja": "和訳" }),
                    inputs: &[c],
                    glossary_at: None,
                },
                t("2026-09-26T06:00:00Z"),
            )
            .unwrap();
        db.insert_score(key, translation, 5, None, t("2026-09-26T07:00:00Z"))
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
