//! 要約・翻訳の作り直しの対象。

use super::*;

/// `redo` で対象を絞る条件。どれも省略できる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedoFilter {
    pub source_id: Option<String>,
    /// この時刻以降に公開（無ければ取得）された記事
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// 利用者が閲覧できる最新の digest の、現在のプロファイルでの点数がこれ以上
    pub min_score: Option<u8>,
    pub ids: Vec<i64>,
}

/// `redo` の対象を選ぶキー：どの成果物を、どのバックエンド・モデル・プロンプト版で作り直すか。
#[derive(Debug, Clone, Copy)]
pub struct RedoKey<'a> {
    pub user_id: i64,
    pub profile_hash: Option<&'a str>,
    pub backend: &'a str,
    pub model: &'a str,
    pub prompt_version: i64,
}

impl Db {
    /// 条件に合い、公開の本文（概要を含む）がある記事のうち、このキーの digest がまだ無いものを
    /// 新しい順に返す。同じ条件で再実行すれば続きから処理できる。
    /// このモデルの digest の失敗で再試行待ち・断念済みの記事は含めない。
    pub fn redo_digest(
        &self,
        key: RedoKey,
        filter: &RedoFilter,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<DigestInput>, DbError> {
        let redo_filter = redo_filter();
        let sql = format!(
            "SELECT a.id, a.source_id, a.title, a.lang FROM articles AS a
             WHERE EXISTS (
                 SELECT 1 FROM contents AS c
                 WHERE c.article_id = a.id AND c.access_membership_id IS NULL)
               AND NOT EXISTS (
                 SELECT 1 FROM artifacts AS r
                 WHERE r.article_id = a.id AND r.kind = 'digest' AND r.backend = :backend
                   AND r.model = :model AND r.prompt_version = :version)
               AND {REDO_AVAILABLE}
               AND {redo_filter}
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT :limit"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let articles = stmt
            .query_map(&*redo_params(key, "digest", filter, now, limit)?, |r| {
                Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<Vec<(i64, String, String, String)>, _>>()?;
        articles
            .into_iter()
            .map(|(article_id, source_id, title, lang)| {
                Ok(DigestInput {
                    article_id,
                    source_id,
                    title,
                    lang,
                    contents: self.public_contents(article_id, ContentSet::All)?,
                })
            })
            .collect()
    }

    /// 条件に合い、公開の本文があり、このキーの要約がある記事と、その最新の版を作ったときの訳語集の
    /// 時点を新しい順に返す。
    /// 訳語集の変更による作り直しの候補。このモデルの要約の失敗で再試行待ち・断念済みの記事は含めない。
    pub fn redo_digest_existing(
        &self,
        key: RedoKey,
        filter: &RedoFilter,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<(DigestInput, Option<String>)>, DbError> {
        let redo_filter = redo_filter();
        let sql = format!(
            "SELECT a.id, a.source_id, a.title, a.lang, latest.glossary_at
             FROM articles AS a
             JOIN artifacts AS latest ON latest.id = (
               SELECT r.id FROM artifacts AS r
               WHERE r.article_id = a.id AND r.kind = 'digest' AND r.backend = :backend
                 AND r.model = :model AND r.prompt_version = :version
               ORDER BY r.created_at DESC, r.id DESC LIMIT 1)
             WHERE EXISTS (
                 SELECT 1 FROM contents AS c
                 WHERE c.article_id = a.id AND c.access_membership_id IS NULL)
               AND {REDO_AVAILABLE}
               AND {redo_filter}
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT :limit"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let articles = stmt
            .query_map(
                &*redo_params(key, "digest", filter, now, usize::MAX)?,
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<(i64, String, String, String, Option<String>)>, _>>()?;
        articles
            .into_iter()
            .map(|(article_id, source_id, title, lang, glossary_at)| {
                let input = DigestInput {
                    article_id,
                    source_id,
                    title,
                    lang,
                    contents: self.public_contents(article_id, ContentSet::All)?,
                };
                Ok((input, glossary_at))
            })
            .collect()
    }

    /// 条件に合い、公開の本文（body/fulltext）があり、このキーの和訳がある英語の記事と、その最新の版を
    /// 作ったときの訳語集の時点を新しい順に返す。訳語集の変更による作り直しの候補。
    pub fn redo_translate_existing(
        &self,
        key: RedoKey,
        filter: &RedoFilter,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<(TranslateInput, Option<String>)>, DbError> {
        let redo_filter = redo_filter();
        let sql = format!(
            "SELECT a.id, a.title, latest.glossary_at
             FROM articles AS a
             JOIN artifacts AS latest ON latest.id = (
               SELECT r.id FROM artifacts AS r
               WHERE r.article_id = a.id AND r.kind = 'translation' AND r.backend = :backend
                 AND r.model = :model AND r.prompt_version = :version
               ORDER BY r.created_at DESC, r.id DESC LIMIT 1)
             WHERE a.lang = 'en'
               AND EXISTS (
                 SELECT 1 FROM contents AS c
                 WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext')
                   AND c.access_membership_id IS NULL)
               AND {REDO_AVAILABLE}
               AND {redo_filter}
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT :limit"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let articles = stmt
            .query_map(
                &*redo_params(key, "translate", filter, now, usize::MAX)?,
                |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect::<Result<Vec<(i64, String, Option<String>)>, _>>()?;
        articles
            .into_iter()
            .map(|(article_id, title, glossary_at)| {
                let input = TranslateInput {
                    article_id,
                    title,
                    contents: self.public_contents(article_id, ContentSet::Body)?,
                };
                Ok((input, glossary_at))
            })
            .collect()
    }

    /// 条件に合い、公開の本文（body/fulltext）がある英語の記事のうち、このキーの和訳がまだ無い
    /// ものを新しい順に返す。このモデルの和訳の失敗で再試行待ち・断念済みの記事は含めない。
    pub fn redo_translate(
        &self,
        key: RedoKey,
        filter: &RedoFilter,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<TranslateInput>, DbError> {
        let redo_filter = redo_filter();
        let sql = format!(
            "SELECT a.id, a.title FROM articles AS a
             WHERE a.lang = 'en'
               AND EXISTS (
                 SELECT 1 FROM contents AS c
                 WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext')
                   AND c.access_membership_id IS NULL)
               AND NOT EXISTS (
                 SELECT 1 FROM artifacts AS r
                 WHERE r.article_id = a.id AND r.kind = 'translation' AND r.backend = :backend
                   AND r.model = :model AND r.prompt_version = :version)
               AND {REDO_AVAILABLE}
               AND {redo_filter}
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT :limit"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let articles = stmt
            .query_map(&*redo_params(key, "translate", filter, now, limit)?, |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        articles
            .into_iter()
            .map(|(article_id, title)| {
                Ok(TranslateInput {
                    article_id,
                    title,
                    contents: self.public_contents(article_id, ContentSet::Body)?,
                })
            })
            .collect()
    }
}

/// `redo` の対象から、このモデルの失敗で再試行待ち・断念済みの記事と、ほかの実行が予約している
/// 記事を除く条件。
const REDO_AVAILABLE: &str = "NOT EXISTS (
    SELECT 1 FROM stage_errors AS e
    WHERE e.article_id = a.id AND e.stage = :stage AND e.backend = :backend
      AND e.model = :model AND (e.attempts >= :max_attempts OR e.next_retry_at > :now))
    AND NOT EXISTS (
    SELECT 1 FROM work_claims AS w
    WHERE w.article_id = a.id AND w.stage = :stage AND w.backend = :backend
      AND w.model = :model)";

/// `RedoFilter` の条件。省略した条件は常に真になる。点数は、利用者が閲覧できる最新の digest に
/// 付いた、現在のプロファイルの採点のうち、採点のプロンプトの最新の版の最高点で判定する。
fn redo_filter() -> String {
    format!(
        "(:source IS NULL OR a.source_id = :source)
    AND (:since IS NULL OR coalesce(a.published_at, a.fetched_at) >= :since)
    AND (:ids = '[]' OR a.id IN (SELECT value FROM json_each(:ids)))
    AND (:min_score IS NULL OR (
      SELECT s.score FROM scores AS s
      WHERE s.user_id = :user AND s.profile_hash = :profile
        -- embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）使わない
        AND s.backend <> 'embedding'
        AND s.artifact_id = {digest_id}
      ORDER BY s.prompt_version DESC, s.score DESC LIMIT 1) >= :min_score)",
        digest_id = super::read::latest_digest("id", "a.id", ":user"),
    )
}

/// 名前付きパラメータ（名前と値）の並び。
type NamedParams = Vec<(&'static str, Box<dyn rusqlite::ToSql>)>;

/// `redo_digest` / `redo_translate` の名前付きパラメータ。
fn redo_params(
    key: RedoKey,
    stage: &'static str,
    filter: &RedoFilter,
    now: chrono::DateTime<chrono::Utc>,
    limit: usize,
) -> Result<NamedParams, DbError> {
    Ok(vec![
        (":backend", Box::new(key.backend.to_string())),
        (":model", Box::new(key.model.to_string())),
        (":version", Box::new(key.prompt_version)),
        (":stage", Box::new(stage)),
        (":max_attempts", Box::new(MAX_ATTEMPTS)),
        (":now", Box::new(timestamp(now))),
        (":source", Box::new(filter.source_id.clone())),
        (":since", Box::new(filter.since.map(timestamp))),
        (":ids", Box::new(serde_json::to_string(&filter.ids)?)),
        (":min_score", Box::new(filter.min_score)),
        (":user", Box::new(key.user_id)),
        (":profile", Box::new(key.profile_hash.map(str::to_string))),
        (":limit", Box::new(i64::try_from(limit).unwrap_or(i64::MAX))),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    fn redo_key<'a>(db: &Db, model: &'a str) -> RedoKey<'a> {
        RedoKey {
            user_id: db.owner_id().unwrap(),
            profile_hash: Some("h1"),
            backend: "claude-cli",
            model,
            prompt_version: 1,
        }
    }

    fn redo_digest_ids(db: &Db, model: &str, filter: &RedoFilter) -> Vec<i64> {
        db.redo_digest(redo_key(db, model), filter, t("2026-09-27T00:00:00Z"), 10)
            .unwrap()
            .into_iter()
            .map(|d| d.article_id)
            .collect()
    }

    /// embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）`--min-score` に使わない。
    #[test]
    fn redo_min_score_ignores_embedding_scores_for_now() {
        let db = Db::open_in_memory().unwrap();
        embedding_scored_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z", 95);
        let filter = RedoFilter {
            min_score: Some(50),
            ..RedoFilter::default()
        };
        assert!(redo_digest_ids(&db, "haiku", &filter).is_empty());
    }

    #[test]
    fn redo_digest_selects_articles_missing_this_model() {
        let db = Db::open_in_memory().unwrap();
        // scored_article は sonnet の digest と採点つき
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let b = scored_article(
            &db,
            "https://e.com/b",
            Lang::En,
            "2026-09-25T00:00:00.000Z",
            40,
        );
        let other = db
            .insert_article(&NewArticle {
                source_id: "other",
                ..article("https://e.com/c")
            })
            .unwrap()
            .unwrap();
        db.insert_content(other, ContentKind::Lead, ContentOrigin::Feed, "lead")
            .unwrap();
        let no_content = page_article(&db, "https://e.com/d", "2026-09-26T00:00:00.000Z");
        let _ = no_content;

        // sonnet の digest があるので、sonnet ではやり直さない
        assert_eq!(
            redo_digest_ids(&db, "sonnet", &RedoFilter::default()),
            [other]
        );
        // opus は無いので対象（本文が無い記事は除く）
        let all = redo_digest_ids(&db, "opus", &RedoFilter::default());
        assert_eq!(all.len(), 3);
        assert!(all.contains(&a) && all.contains(&b) && all.contains(&other));

        let by_source = RedoFilter {
            source_id: Some("other".into()),
            ..RedoFilter::default()
        };
        assert_eq!(redo_digest_ids(&db, "opus", &by_source), [other]);
        let by_score = RedoFilter {
            min_score: Some(80),
            ..RedoFilter::default()
        };
        assert_eq!(redo_digest_ids(&db, "opus", &by_score), [a]);
        let by_since = RedoFilter {
            since: Some(t("2026-09-25T12:00:00Z")),
            ..RedoFilter::default()
        };
        assert!(!redo_digest_ids(&db, "opus", &by_since).contains(&b));
        let by_ids = RedoFilter {
            ids: vec![b],
            ..RedoFilter::default()
        };
        assert_eq!(redo_digest_ids(&db, "opus", &by_ids), [b]);

        let inputs = db
            .redo_digest(
                redo_key(&db, "opus"),
                &by_ids,
                t("2026-09-27T00:00:00Z"),
                10,
            )
            .unwrap();
        assert_eq!(inputs[0].contents.len(), 1);
    }

    /// 点数の条件は、採点のプロンプトの最新の版の点数で判定する。
    #[test]
    fn redo_min_score_uses_latest_score_prompt_version() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let by_score = RedoFilter {
            min_score: Some(80),
            ..RedoFilter::default()
        };
        assert_eq!(redo_digest_ids(&db, "opus", &by_score), [a]);
        rescore_with_version(&db, a, 2, 40);
        assert!(redo_digest_ids(&db, "opus", &by_score).is_empty());
    }

    #[test]
    fn redo_digest_skips_articles_backing_off_for_this_model() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.record_stage_failure(
            StageKey {
                article_id: a,
                stage: "digest",
                backend: "claude-cli",
                model: "opus",
            },
            "x",
            t("2026-09-27T00:00:00Z"),
            false,
        )
        .unwrap();
        assert!(redo_digest_ids(&db, "opus", &RedoFilter::default()).is_empty());
    }

    /// ほかの実行が予約している記事は、作り直しの対象にも選ばない。
    #[test]
    fn redo_skips_claimed_articles() {
        let db = Db::open_in_memory().unwrap();
        let now = t("2026-09-27T00:00:00Z");
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let key = |stage| ClaimKey {
            stage,
            backend: "claude-cli",
            model: "opus",
        };
        let translate_ids = |db: &Db| -> Vec<i64> {
            db.redo_translate(redo_key(db, "opus"), &RedoFilter::default(), now, 10)
                .unwrap()
                .into_iter()
                .map(|i| i.article_id)
                .collect()
        };
        assert_eq!(redo_digest_ids(&db, "opus", &RedoFilter::default()), [a]);
        assert_eq!(translate_ids(&db), [a]);
        let _digest = db
            .claim(key("digest"), &[a], now, chrono::Duration::minutes(10))
            .unwrap();
        let _translate = db
            .claim(key("translate"), &[a], now, chrono::Duration::minutes(10))
            .unwrap();
        assert!(redo_digest_ids(&db, "opus", &RedoFilter::default()).is_empty());
        assert!(translate_ids(&db).is_empty());
    }

    #[test]
    fn redo_translate_selects_english_articles_missing_this_model() {
        let db = Db::open_in_memory().unwrap();
        let en = scored_article(
            &db,
            "https://e.com/en",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let _ja = scored_article(
            &db,
            "https://e.com/ja",
            Lang::Ja,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let body: i64 = db
            .conn()
            .query_row("SELECT id FROM contents WHERE article_id = ?1", [en], |r| {
                r.get(0)
            })
            .unwrap();
        let payload = serde_json::json!({"body_ja": "和訳"});
        db.insert_translation(
            &NewArtifact {
                article_id: en,
                kind: ArtifactKind::Translation,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &payload,
                inputs: &[body],
                glossary_at: None,
            },
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();
        let ids = |model| -> Vec<i64> {
            db.redo_translate(
                redo_key(&db, model),
                &RedoFilter::default(),
                t("2026-09-27T00:00:00Z"),
                10,
            )
            .unwrap()
            .into_iter()
            .map(|i| i.article_id)
            .collect()
        };
        assert!(ids("sonnet").is_empty());
        assert_eq!(ids("opus"), [en]);
    }

    /// 訳語集による作り直しの候補は、このモデルの和訳がある英語の記事と、その最新の版の時点。
    #[test]
    fn redo_translate_existing_returns_the_latest_glossary_time() {
        let db = Db::open_in_memory().unwrap();
        let en = scored_article(
            &db,
            "https://e.com/en",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let _untranslated = scored_article(
            &db,
            "https://e.com/en2",
            Lang::En,
            "2026-09-25T00:00:00.000Z",
            90,
        );
        let body: i64 = db
            .conn()
            .query_row("SELECT id FROM contents WHERE article_id = ?1", [en], |r| {
                r.get(0)
            })
            .unwrap();
        let payload = serde_json::json!({"body_ja": "和訳"});
        for (glossary_at, at) in [
            (None, "2026-09-27T00:00:00Z"),
            (Some("2026-09-27T05:00:00.000Z"), "2026-09-27T06:00:00Z"),
        ] {
            db.insert_translation(
                &NewArtifact {
                    article_id: en,
                    kind: ArtifactKind::Translation,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[body],
                    glossary_at,
                },
                t(at),
            )
            .unwrap();
        }
        let existing = |model| -> Vec<(i64, Option<String>)> {
            db.redo_translate_existing(
                redo_key(&db, model),
                &RedoFilter::default(),
                t("2026-09-27T07:00:00Z"),
            )
            .unwrap()
            .into_iter()
            .map(|(i, at)| (i.article_id, at))
            .collect()
        };
        assert_eq!(
            existing("sonnet"),
            [(en, Some("2026-09-27T05:00:00.000Z".to_string()))]
        );
        assert!(existing("opus").is_empty());
    }

    /// 訳語集による作り直しの候補は、このモデルの要約がある記事と、その最新の版の時点。
    #[test]
    fn redo_digest_existing_returns_articles_with_this_model() {
        let db = Db::open_in_memory().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let _undigested = page_article(&db, "https://e.com/d", "2026-09-26T00:00:00.000Z");
        let existing = |model| -> Vec<(i64, Option<String>)> {
            db.redo_digest_existing(
                redo_key(&db, model),
                &RedoFilter::default(),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap()
            .into_iter()
            .map(|(d, at)| (d.article_id, at))
            .collect()
        };
        assert_eq!(existing("sonnet"), [(a, None)]);
        assert!(existing("opus").is_empty());
    }

    /// 会員限定の本文からしか作れない記事は、公開の入力が無いので作り直しの候補にしない。
    #[test]
    fn redo_existing_skips_articles_without_public_contents() {
        let db = Db::open_in_memory().unwrap();
        let m = insert_membership(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let gated = insert_content(&db, a, Some(m));
        for (kind, payload) in [
            (ArtifactKind::Digest, serde_json::json!({"title_ja": "題"})),
            (
                ArtifactKind::Translation,
                serde_json::json!({"body_ja": "和訳"}),
            ),
        ] {
            db.insert_artifact(
                &NewArtifact {
                    article_id: a,
                    kind,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[gated],
                    glossary_at: None,
                },
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        }
        let now = t("2026-09-27T01:00:00Z");
        let key = redo_key(&db, "sonnet");
        assert!(
            db.redo_digest_existing(key, &RedoFilter::default(), now)
                .unwrap()
                .is_empty()
        );
        assert!(
            db.redo_translate_existing(key, &RedoFilter::default(), now)
                .unwrap()
                .is_empty()
        );
    }
}
