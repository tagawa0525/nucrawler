//! 翻訳の依頼と、翻訳待ちの記事。

use super::*;

/// 和訳の対象を選ぶ条件。
#[derive(Debug, Clone, Copy)]
pub struct TranslateQuery<'a> {
    /// 先回りの和訳で、点数と閲覧できる要約を見る利用者（依頼は誰のものでも拾う）
    pub user_id: i64,
    /// 先回りの和訳に使う、現在のプロファイルのハッシュ（無ければ先回りはしない）
    pub profile_hash: Option<&'a str>,
    /// この点数以上なら、依頼が無くても先回りで和訳する
    pub min_score: u8,
    /// 依頼された記事だけを対象にする（15 分ごとの依頼処理）
    pub requests_only: bool,
    pub backend: &'a str,
    pub model: &'a str,
}

/// 和訳する記事と、その公開の本文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateInput {
    pub article_id: i64,
    pub title: String,
    pub contents: Vec<InputContent>,
}

impl Db {
    /// 和訳の成果物を保存し、その記事への依頼を同じトランザクションで完了にする。
    /// 別々に書くと、間で止まったときに依頼が完了しないまま残る（成果物があるので
    /// 以後は和訳の対象にならず、依頼が永久に残る）。
    pub fn insert_translation(
        &self,
        a: &NewArtifact,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        let id = write_artifact(&tx, a, now)?;
        tx.execute(
            "UPDATE translation_requests SET done_at = ?2
             WHERE article_id = ?1 AND done_at IS NULL",
            rusqlite::params![a.article_id, timestamp(now)],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// 和訳を依頼する。既に和訳があれば完了として登録する。既に依頼していれば、
    /// 和訳があるときだけ完了にし、そうでなければ何もしない。
    pub fn request_translation(
        &self,
        user_id: i64,
        article_id: i64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        // 既に和訳があれば、最初から完了として登録する（判定と登録は 1 文で行い、
        // 和訳の保存と入れ違いになっても依頼が開いたまま残らないようにする）
        self.conn.execute(
            "INSERT INTO translation_requests (user_id, article_id, requested_at, done_at)
             SELECT ?1, ?2, ?3,
                    CASE WHEN EXISTS (
                      SELECT 1 FROM artifacts
                      WHERE article_id = ?2 AND kind = 'translation') THEN ?3 END
             WHERE true
             ON CONFLICT (user_id, article_id) DO UPDATE SET done_at = excluded.done_at
               -- 開いたまま残っていた依頼も、和訳があれば完了にする（無ければ開いたまま）
               WHERE translation_requests.done_at IS NULL AND excluded.done_at IS NOT NULL",
            rusqlite::params![user_id, article_id, timestamp(now)],
        )?;
        Ok(())
    }

    /// 和訳がまだ 1 つも無く、公開の本文（body/fulltext）がある英語の記事のうち、
    /// 誰かが依頼したもの（期間を問わない）と、`requests_only` でなければ現在のプロファイルで
    /// `min_score` 以上に採点されたもの（`cutoff` 以降）を返す。依頼を先（古い順）、次に点数の高い順。
    /// このモデルの和訳の失敗で再試行待ち・断念済みの記事は含めない。
    pub fn pending_translate(
        &self,
        q: TranslateQuery,
        cutoff: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<TranslateInput>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH base AS (
               SELECT a.id, a.title, coalesce(a.published_at, a.fetched_at) AS at,
                      -- 和訳は全員で共有するので、依頼は誰のものでも拾い、最も古い依頼で並べる
                      (SELECT min(tr.requested_at) FROM translation_requests AS tr
                       WHERE tr.article_id = a.id AND tr.done_at IS NULL) AS requested_at,
                      -- 利用者が閲覧できる最新の digest。先回りの判定（点数と lwr_relevant）は
                      -- この版だけで行い、古い版の高得点では先回りしない
                      {digest_id} AS digest_id
               FROM articles AS a
               WHERE a.lang = 'en'
                 AND NOT EXISTS (
                   SELECT 1 FROM artifacts AS r
                   WHERE r.article_id = a.id AND r.kind = 'translation')
                 AND EXISTS (
                   SELECT 1 FROM contents AS c
                   WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext')
                     AND c.access_membership_id IS NULL)
                 AND {available}
             ),
             candidates AS (
               SELECT b.*,
                      -- プロファイルが無い（?2 が NULL）なら点数は付かず、依頼だけが残る
                      -- 採点のプロンプトの最新の版で、モデル間の最高点
                      (SELECT s.score FROM scores AS s
                       WHERE s.user_id = ?1 AND s.profile_hash = ?2
                         AND s.artifact_id = b.digest_id
                         -- embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）使わない
                         AND s.backend <> 'embedding'
                       ORDER BY s.prompt_version DESC, s.score DESC LIMIT 1) AS score,
                      (SELECT json_extract(r.payload, '$.lwr_relevant') FROM artifacts AS r
                       WHERE r.id = b.digest_id) AS relevant
               FROM base AS b
             )
             SELECT id, title FROM candidates
             -- 依頼は明示的なので、軽水炉と無関係な記事でも和訳する
             WHERE requested_at IS NOT NULL
                OR (?7 = 0 AND relevant = 1 AND score >= ?8 AND at >= ?9)
             ORDER BY requested_at IS NULL, requested_at, score DESC, at DESC, id DESC
             LIMIT ?10",
            digest_id = super::read::latest_digest("id", "a.id", "?1"),
            available = super::claims::available(super::claims::Available {
                article: "a.id",
                stage: "'translate'",
                backend: "?3",
                model: "?4",
                max_attempts: "?5",
                now: "?6",
            }),
        ))?;
        let articles = stmt
            .query_map(
                rusqlite::params![
                    q.user_id,
                    q.profile_hash,
                    q.backend,
                    q.model,
                    MAX_ATTEMPTS,
                    timestamp(now),
                    q.requests_only,
                    q.min_score,
                    timestamp(cutoff),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        articles
            .into_iter()
            .map(|(article_id, title)| {
                let contents = self.public_contents(article_id, ContentSet::Body)?;
                Ok(TranslateInput {
                    article_id,
                    title,
                    contents,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    fn translate_query(db: &Db, requests_only: bool) -> TranslateQuery<'static> {
        TranslateQuery {
            user_id: db.owner_id().unwrap(),
            profile_hash: Some("h1"),
            min_score: 80,
            requests_only,
            backend: "claude-cli",
            model: "sonnet",
        }
    }

    fn translate_ids(db: &Db, requests_only: bool, now: &str) -> Vec<i64> {
        db.pending_translate(
            translate_query(db, requests_only),
            t("2026-09-10T00:00:00Z"),
            t(now),
            10,
        )
        .unwrap()
        .into_iter()
        .map(|i| i.article_id)
        .collect()
    }

    /// embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）先回りの和訳に使わない。
    #[test]
    fn pending_translate_ignores_embedding_scores_for_now() {
        let db = Db::open_in_memory().unwrap();
        embedding_scored_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z", 95);
        assert!(translate_ids(&db, false, "2026-09-27T00:00:00Z").is_empty());
    }

    #[test]
    fn pending_translate_picks_requests_then_high_scores() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let high = scored_article(
            &db,
            "https://e.com/high",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let higher = scored_article(
            &db,
            "https://e.com/higher",
            Lang::En,
            "2026-09-25T00:00:00.000Z",
            95,
        );
        let low = scored_article(
            &db,
            "https://e.com/low",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            50,
        );
        let _ja = scored_article(
            &db,
            "https://e.com/ja",
            Lang::Ja,
            "2026-09-26T00:00:00.000Z",
            99,
        );
        // 期間外でも、依頼されていれば和訳する
        let old = scored_article(
            &db,
            "https://e.com/old",
            Lang::En,
            "2026-08-01T00:00:00.000Z",
            10,
        );
        db.request_translation(owner, old, t("2026-09-26T03:00:00Z"))
            .unwrap();
        db.request_translation(owner, low, t("2026-09-26T04:00:00Z"))
            .unwrap();
        db.request_translation(owner, low, t("2026-09-26T05:00:00Z"))
            .unwrap();

        assert_eq!(translate_ids(&db, false, now), [old, low, higher, high]);
        assert_eq!(translate_ids(&db, true, now), [old, low]);

        let inputs = db
            .pending_translate(
                translate_query(&db, true),
                t("2026-09-10T00:00:00Z"),
                t(now),
                1,
            )
            .unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(
            inputs[0]
                .contents
                .iter()
                .map(|c| c.kind.as_str())
                .collect::<Vec<_>>(),
            ["body"]
        );
    }

    #[test]
    fn translated_articles_and_completed_requests_drop_out() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.request_translation(owner, a, t(now)).unwrap();
        let body: i64 = db
            .conn()
            .query_row("SELECT id FROM contents WHERE article_id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        let payload = serde_json::json!({"body_ja": "和訳"});
        db.insert_translation(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Translation,
                backend: "claude-cli",
                model: "haiku",
                prompt_version: 1,
                payload: &payload,
                inputs: &[body],
                glossary_at: None,
            },
            t(now),
        )
        .unwrap();
        assert!(translate_ids(&db, false, now).is_empty());
        assert_eq!(
            db.query_i64("SELECT count(*) FROM translation_requests WHERE done_at IS NOT NULL")
                .unwrap(),
            1
        );
    }

    /// 先回りの判定は、利用者が閲覧できる最新の digest の採点だけで行う（古い版の高得点は使わない）。
    /// ほかの実行が予約している記事は選ばない。
    #[test]
    fn pending_translate_skips_claimed_articles() {
        let db = Db::open_in_memory().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let key = ClaimKey {
            stage: "translate",
            backend: "claude-cli",
            model: "sonnet",
        };
        let held = db
            .claim(key, &[a], t(now), chrono::Duration::minutes(10))
            .unwrap();
        assert!(translate_ids(&db, false, now).is_empty());
        drop(held);
        assert_eq!(translate_ids(&db, false, now), [a]);
    }

    #[test]
    fn pending_translate_uses_score_of_latest_digest() {
        let db = Db::open_in_memory().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        assert_eq!(translate_ids(&db, false, now), [a]);
        // 新しい digest ができ、まだ採点されていない → 先回りしない
        let newer = add_digest(&db, a, "opus", "新版", true, "2026-09-26T05:00:00Z");
        assert!(translate_ids(&db, false, now).is_empty());
        // 新しい digest の採点が閾値未満 → 先回りしない
        db.insert_score(
            ScoreKey {
                user_id: db.owner_id().unwrap(),
                profile_hash: "h1",
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
            },
            newer,
            50,
            None,
            t("2026-09-26T06:00:00Z"),
        )
        .unwrap();
        assert!(translate_ids(&db, false, now).is_empty());
    }

    /// 先回りの判定は、採点のプロンプトの最新の版の点数で行う。
    #[test]
    fn pending_translate_uses_latest_score_prompt_version() {
        let db = Db::open_in_memory().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        assert_eq!(translate_ids(&db, false, now), [a]);
        rescore_with_version(&db, a, 2, 40);
        assert!(translate_ids(&db, false, now).is_empty());
    }

    /// 最新の digest が軽水炉と無関係なら先回りはしない。依頼されれば和訳する（明示的に頼んだので）。
    #[test]
    fn pending_translate_skips_non_lwr_unless_requested() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let unrelated = add_digest(&db, a, "opus", "非軽水炉", false, "2026-09-26T05:00:00Z");
        db.insert_score(
            ScoreKey {
                user_id: owner,
                profile_hash: "h1",
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
            },
            unrelated,
            95,
            None,
            t("2026-09-26T06:00:00Z"),
        )
        .unwrap();
        assert!(translate_ids(&db, false, now).is_empty());
        db.request_translation(owner, a, t(now)).unwrap();
        assert_eq!(translate_ids(&db, false, now), [a]);
    }

    #[test]
    fn insert_translation_completes_requests_atomically() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.request_translation(owner, a, t(now)).unwrap();
        let body: i64 = db
            .conn()
            .query_row("SELECT id FROM contents WHERE article_id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        let payload = serde_json::json!({"body_ja": "和訳"});
        let translation = |inputs| NewArtifact {
            article_id: a,
            kind: ArtifactKind::Translation,
            backend: "claude-cli",
            model: "sonnet",
            prompt_version: 1,
            payload: &payload,
            inputs,
            glossary_at: None,
        };
        // 失敗したら（入力が空）依頼も完了にならない
        assert!(db.insert_translation(&translation(&[]), t(now)).is_err());
        assert_eq!(
            db.query_i64("SELECT count(*) FROM translation_requests WHERE done_at IS NULL")
                .unwrap(),
            1
        );
        db.insert_translation(&translation(&[body]), t(now))
            .unwrap();
        assert_eq!(
            db.query_i64("SELECT count(*) FROM translation_requests WHERE done_at IS NULL")
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_i64("SELECT count(*) FROM artifacts WHERE kind = 'translation'")
                .unwrap(),
            1
        );
    }

    /// 和訳済みの記事への依頼は、最初から完了として登録する（開いたまま残らない）。
    #[test]
    fn requests_for_translated_articles_are_done_immediately() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let now = "2026-09-27T00:00:00Z";
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let body: i64 = db
            .conn()
            .query_row("SELECT id FROM contents WHERE article_id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        let payload = serde_json::json!({"body_ja": "和訳"});
        db.insert_translation(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Translation,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &payload,
                inputs: &[body],
                glossary_at: None,
            },
            t(now),
        )
        .unwrap();
        db.request_translation(owner, a, t("2026-09-27T01:00:00Z"))
            .unwrap();
        assert_eq!(
            db.query_i64("SELECT count(*) FROM translation_requests WHERE done_at IS NULL")
                .unwrap(),
            0
        );
    }

    /// 開いたまま残っていた依頼も、再度依頼すれば和訳の有無に応じて完了になる。
    #[test]
    fn repeated_request_repairs_open_request_for_translated_article() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.request_translation(owner, a, t("2026-09-27T00:00:00Z"))
            .unwrap();
        // 以前の実装で、和訳だけが保存され依頼が開いたまま残った状態を作る
        let body: i64 = db
            .conn()
            .query_row("SELECT id FROM contents WHERE article_id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        let payload = serde_json::json!({"body_ja": "和訳"});
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Translation,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &payload,
                inputs: &[body],
                glossary_at: None,
            },
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        db.request_translation(owner, a, t("2026-09-27T02:00:00Z"))
            .unwrap();
        assert_eq!(
            db.query_i64("SELECT count(*) FROM translation_requests WHERE done_at IS NULL")
                .unwrap(),
            0
        );
    }

    #[test]
    fn pending_translate_without_profile_only_serves_requests() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let q = TranslateQuery {
            profile_hash: None,
            ..translate_query(&db, false)
        };
        let ids = db
            .pending_translate(q, t("2026-09-10T00:00:00Z"), t("2026-09-27T00:00:00Z"), 10)
            .unwrap();
        assert!(ids.is_empty());
        db.request_translation(q.user_id, a, t("2026-09-27T00:00:00Z"))
            .unwrap();
        let ids = db
            .pending_translate(q, t("2026-09-10T00:00:00Z"), t("2026-09-27T00:00:00Z"), 10)
            .unwrap();
        assert_eq!(ids.len(), 1);
    }

    /// 依頼は誰のものでも拾い、記事ごとに最も古い未完了の依頼の時刻で並べる（和訳は全員で共有するので）。
    #[test]
    fn pending_translate_serves_requests_from_every_user() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.conn()
            .execute(
                "INSERT INTO users (id, login, display_name) VALUES (2, 'other', 'other')",
                [],
            )
            .unwrap();
        let theirs = scored_article(
            &db,
            "https://e.com/theirs",
            Lang::En,
            "2026-08-01T00:00:00.000Z",
            10,
        );
        let both = scored_article(
            &db,
            "https://e.com/both",
            Lang::En,
            "2026-08-01T00:00:00.000Z",
            10,
        );
        db.request_translation(2, theirs, t("2026-09-26T02:00:00Z"))
            .unwrap();
        // 所有者の依頼は後でも、ほかの人がそれより前に依頼していれば、その時刻で並ぶ
        db.request_translation(owner, both, t("2026-09-26T03:00:00Z"))
            .unwrap();
        db.request_translation(2, both, t("2026-09-26T01:00:00Z"))
            .unwrap();

        let now = "2026-09-27T00:00:00Z";
        assert_eq!(translate_ids(&db, true, now), [both, theirs]);
        assert_eq!(translate_ids(&db, false, now), [both, theirs]);
    }
}
