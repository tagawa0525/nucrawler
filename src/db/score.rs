//! 採点待ちの要約と、採点の登録。

use super::*;

/// 採点の対象を特定するキー（誰の・どのプロファイルで・どのモデルと版のプロンプトで）。
#[derive(Debug, Clone, Copy)]
pub struct ScoreKey<'a> {
    pub user_id: i64,
    pub profile_hash: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
    pub prompt_version: i64,
}

/// 採点の失敗を記録するステージ名。`stage_errors` の主キーは記事・ステージ・バックエンド・
/// モデルで、利用者・プロファイル・プロンプトの版を持たないので、ステージ名にそれらを含めて範囲を
/// 区別する。古い版で断念した記事も、版を上げれば採点し直す（プロンプトが変われば成功しうる）。
pub fn score_stage(key: ScoreKey) -> String {
    format!(
        "score:{}:{}:v{}",
        key.user_id, key.profile_hash, key.prompt_version
    )
}

/// 採点を待つ記事の範囲。
#[derive(Debug, Clone, Copy)]
pub enum ScoreScope<'a> {
    /// この時刻以降に公開（公開日時が無ければ取得）された記事
    Since(chrono::DateTime<chrono::Utc>),
    /// 指定した記事（公開の時期は問わない）
    Articles(&'a [i64]),
}

/// 採点が当たったプロファイルの語。
#[derive(Debug, Clone, Copy, Default)]
pub struct ScoreMatches<'a> {
    /// 当たった関心分野（interest の topic）
    pub interests: &'a [String],
    /// 当たった推薦しない話題（exclude）
    pub excludes: &'a [String],
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

impl Db {
    /// 各記事について利用者が閲覧できる最新の digest のうち、軽水炉に関係し（lwr_relevant）、
    /// `scope` の範囲の記事で、このキー（プロンプトの版を含む）の採点がまだ無いものを新しい順に返す。
    /// このモデルの採点の失敗で再試行待ち・断念済みの記事は含めない。
    pub fn pending_score(
        &self,
        key: ScoreKey,
        scope: ScoreScope<'_>,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<ScoreInput>, DbError> {
        // 使わない方の条件は NULL にして常に真にする
        let (cutoff, ids) = match scope {
            ScoreScope::Since(cutoff) => (Some(timestamp(cutoff)), None),
            ScoreScope::Articles(ids) => (None, Some(serde_json::to_string(ids)?)),
        };
        let mut stmt = self.conn.prepare(&format!(
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
                    {linked}
             FROM latest AS l
             JOIN articles AS a ON a.id = l.article_id
             WHERE json_extract(l.payload, '$.lwr_relevant') = 1
               AND (?2 IS NULL OR coalesce(a.published_at, a.fetched_at) >= ?2)
               AND (?11 IS NULL OR a.id IN (SELECT value FROM json_each(?11)))
               AND NOT EXISTS (
                 SELECT 1 FROM scores AS s
                 WHERE s.user_id = ?1 AND s.artifact_id = l.id AND s.profile_hash = ?3
                   AND s.backend = ?4 AND s.model = ?5 AND s.prompt_version = ?10)
               AND NOT EXISTS (
                 SELECT 1 FROM stage_errors AS e
                 WHERE e.article_id = l.article_id AND e.stage = ?9
                   AND e.backend = ?4 AND e.model = ?5
                   AND (e.attempts >= ?6 OR e.next_retry_at > ?7))
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?8",
            linked = linked_topics("l"),
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![
                key.user_id,
                cutoff,
                key.profile_hash,
                key.backend,
                key.model,
                MAX_ATTEMPTS,
                timestamp(now),
                i64::try_from(limit).unwrap_or(i64::MAX),
                score_stage(key),
                key.prompt_version,
                ids,
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

    /// 当たった語の無い採点を登録する（`insert_score_with_matches`）。
    pub fn insert_score(
        &self,
        key: ScoreKey,
        artifact_id: i64,
        score: u8,
        reason: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        self.insert_score_with_matches(
            key,
            artifact_id,
            score,
            reason,
            ScoreMatches::default(),
            now,
        )
    }

    /// 採点と、当たった関心分野・推薦しない話題を同じトランザクションで登録する。
    pub fn insert_score_with_matches(
        &self,
        key: ScoreKey,
        artifact_id: i64,
        score: u8,
        reason: Option<&str>,
        matches: ScoreMatches<'_>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO scores
               (user_id, artifact_id, profile_hash, backend, model, prompt_version, score, reason,
                created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                key.user_id,
                artifact_id,
                key.profile_hash,
                key.backend,
                key.model,
                key.prompt_version,
                score,
                reason,
                timestamp(now),
            ],
        )?;
        let score_id = tx.last_insert_rowid();
        for (kind, topics) in [
            ("interest", matches.interests),
            ("exclude", matches.excludes),
        ] {
            for topic in topics {
                tx.execute(
                    "INSERT INTO score_matches (score_id, kind, topic) VALUES (?1, ?2, ?3)",
                    rusqlite::params![score_id, kind, topic],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    fn score_ids(db: &Db, key: ScoreKey, now: &str) -> Vec<i64> {
        db.pending_score(
            key,
            ScoreScope::Since(t("2026-09-10T00:00:00Z")),
            t(now),
            10,
        )
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
            .pending_score(
                key,
                ScoreScope::Since(t("2026-09-10T00:00:00Z")),
                t(now),
                10,
            )
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
    fn pending_score_rescores_when_prompt_version_changes() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let now = "2026-09-27T00:00:00Z";
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let d = add_digest(&db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        db.insert_score(key, d, 80, None, t(now)).unwrap();
        assert!(score_ids(&db, key, now).is_empty());
        let next = ScoreKey {
            prompt_version: key.prompt_version + 1,
            ..key
        };
        assert_eq!(score_ids(&db, next, now), [a]);
        db.insert_score(next, d, 60, None, t(now)).unwrap();
        assert!(score_ids(&db, next, now).is_empty());
        assert_eq!(
            db.query_strings("SELECT prompt_version || ':' || score FROM scores ORDER BY id")
                .unwrap(),
            ["1:80", "2:60"]
        );
    }

    /// 候補のプロファイルの評価では、公開の時期によらず、指定した記事だけを採点する。
    #[test]
    fn pending_score_can_be_limited_to_articles() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let now = "2026-09-27T00:00:00Z";
        let old = page_article(&db, "https://e.com/old", "2026-01-01T00:00:00.000Z");
        add_digest(&db, old, "sonnet", "古い", true, "2026-01-01T01:00:00Z");
        let other = page_article(&db, "https://e.com/other", "2026-09-26T00:00:00.000Z");
        add_digest(&db, other, "sonnet", "対象外", true, "2026-09-26T01:00:00Z");
        fn ids(db: &Db, key: ScoreKey, scope: ScoreScope<'_>, now: &str) -> Vec<i64> {
            db.pending_score(key, scope, t(now), 10)
                .unwrap()
                .into_iter()
                .map(|s| s.article_id)
                .collect()
        }
        assert_eq!(ids(&db, key, ScoreScope::Articles(&[old]), now), [old]);
        assert!(ids(&db, key, ScoreScope::Articles(&[]), now).is_empty());
        // 期間で絞れば古い記事は入らない
        assert_eq!(
            ids(&db, key, ScoreScope::Since(t("2026-09-10T00:00:00Z")), now),
            [other]
        );
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
            "implications_ja": "", "lwr_relevant": true, "topics": ["燃料"],
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
                glossary_at: None,
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

    /// 古い版のプロンプトで断念した記事も、版を上げれば採点し直す（プロンプトが変われば成功しうる）。
    #[test]
    fn score_failures_are_scoped_to_prompt_version() {
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
        let next = ScoreKey {
            prompt_version: key.prompt_version + 1,
            ..key
        };
        assert_eq!(score_ids(&db, next, now), [a]);
    }

    #[test]
    fn stores_matches_with_the_score() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let d = add_digest(&db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        db.insert_score_with_matches(
            key,
            d,
            80,
            Some("r"),
            ScoreMatches {
                interests: &["規制・審査".into(), "燃料".into()],
                excludes: &["核融合".into()],
            },
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();
        let rows = || {
            db.query_strings("SELECT kind || ':' || topic FROM score_matches ORDER BY kind, topic")
                .unwrap()
        };
        assert_eq!(
            rows(),
            ["exclude:核融合", "interest:燃料", "interest:規制・審査"]
        );
        // 採点が消えれば当たった語も消える
        db.conn().execute("DELETE FROM scores", []).unwrap();
        assert!(rows().is_empty());
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
}
