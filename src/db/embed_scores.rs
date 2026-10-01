//! 好みの文の embedding と、embedding の点数（計画 010）。点数は LLM の採点と同じ `scores` 表に、
//! `backend = 'embedding'` で保存する。

use std::collections::HashMap;

use super::*;
use crate::profile::Profile;

/// 採点する利用者のプロファイル。
#[derive(Debug, Clone, PartialEq)]
pub struct ScoringProfile {
    pub user_id: i64,
    pub profile: Profile,
    pub hash: String,
}

/// 採点の対象（利用者が閲覧できる、記事の最新の要約で、軽水炉に関係し、ベクトルがあるもの）。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub article_id: i64,
    pub artifact_id: i64,
    pub vector: Vec<f32>,
}

/// 対象の絞り方。
#[derive(Debug, Clone, Copy, Default)]
pub struct CandidateFilter<'a> {
    /// この時刻以降に公開（無ければ取得）された記事だけ
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// このキーの点数がまだ無い要約だけ
    pub unscored: Option<ScoreKey<'a>>,
}

/// 保存する embedding の点数と、補正の特徴にする関心分野・推薦しない話題。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingScore {
    pub artifact_id: i64,
    pub score: u8,
    pub interest: Option<String>,
    pub exclude: Option<String>,
}

/// 評価した記事の、`eval` でその場で採点するための材料（利用者が閲覧できる最新の要約のベクトルと特徴）。
#[derive(Debug, Clone, PartialEq)]
pub struct LabeledVector {
    pub article_id: i64,
    pub source_id: String,
    pub topics: Vec<String>,
    pub vector: Vec<f32>,
}

impl Db {
    /// 利用者が評価した記事のうち、採点の対象（`embedding_candidates` と同じ条件）のもの（記事の id 順）。
    pub fn eval_embedding_inputs(
        &self,
        user_id: i64,
        space_id: i64,
    ) -> Result<Vec<LabeledVector>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH {latest}
             SELECT l.article_id, a.source_id, {topics}, e.vector
             FROM latest AS l
             JOIN articles AS a ON a.id = l.article_id
             JOIN article_embeddings AS e ON e.artifact_id = l.id AND e.space_id = ?2
             WHERE json_extract(l.payload, '$.lwr_relevant') = 1
               AND l.article_id IN (SELECT article_id FROM ratings WHERE user_id = ?1)
             ORDER BY l.article_id",
            latest = super::read::latest_digests("?1"),
            topics = super::read::linked_topics("l"),
        ))?;
        let rows = stmt.query_map(rusqlite::params![user_id, space_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (article_id, source_id, topics, bytes) = row?;
            let vector = crate::embedding::decode(&bytes).ok_or_else(|| {
                DbError::UnexpectedValue(format!("vector of article {article_id}"))
            })?;
            Ok(LabeledVector {
                article_id,
                source_id,
                topics: serde_json::from_str(&topics)?,
                vector,
            })
        })
        .collect()
    }

    /// プロファイルのある利用者（利用者の id 順）。
    pub fn scoring_profiles(&self) -> Result<Vec<ScoringProfile>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT user_id, interests, excludes, hash FROM profiles ORDER BY user_id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (user_id, interests, excludes, hash) = row?;
            Ok(ScoringProfile {
                user_id,
                profile: Profile {
                    interests: serde_json::from_str(&interests)?,
                    exclude: serde_json::from_str(&excludes)?,
                },
                hash,
            })
        })
        .collect()
    }

    /// `texts` のうち、`space_id` の空間にベクトルがある文のベクトル。
    pub fn text_embeddings(
        &self,
        space_id: i64,
        texts: &[String],
    ) -> Result<HashMap<String, Vec<f32>>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT e.text, e.vector FROM text_embeddings AS e
             WHERE e.space_id = ?1 AND e.text IN (SELECT value FROM json_each(?2))",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![space_id, serde_json::to_string(texts)?],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
        )?;
        rows.map(|row| {
            let (text, bytes) = row?;
            let vector = crate::embedding::decode(&bytes)
                .ok_or_else(|| DbError::UnexpectedValue(format!("text vector of {text:?}")))?;
            Ok((text, vector))
        })
        .collect()
    }

    /// 文のベクトルを保存する。`space_id` の空間がもう無ければ何も保存せず `false`。すでにある文は変えない。
    pub fn save_text_embeddings(
        &self,
        space_id: i64,
        vectors: &[(String, Vec<f32>)],
    ) -> Result<bool, DbError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if !self.has_embedding_space(space_id)? {
            return Ok(false);
        }
        let mut stmt = self.conn.prepare(
            "INSERT INTO text_embeddings (text, space_id, vector) VALUES (?1, ?2, ?3)
             ON CONFLICT (text) DO NOTHING",
        )?;
        for (text, vector) in vectors {
            stmt.execute(rusqlite::params![
                text,
                space_id,
                crate::embedding::encode(vector)
            ])?;
        }
        drop(stmt);
        tx.commit()?;
        Ok(true)
    }

    /// `space_id` の空間の、`keep` に無い文のベクトルを消す（今のどのプロファイルにも使われなくなったもの）。
    pub fn prune_text_embeddings(&self, space_id: i64, keep: &[String]) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM text_embeddings
             WHERE space_id = ?1 AND text NOT IN (SELECT value FROM json_each(?2))",
            rusqlite::params![space_id, serde_json::to_string(keep)?],
        )?;
        Ok(())
    }

    /// 利用者の採点の対象を、記事の新しい順に、`offset` 件を飛ばして最大 `limit` 件返す。
    pub fn embedding_candidates(
        &self,
        user_id: i64,
        space_id: i64,
        filter: CandidateFilter<'_>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Candidate>, DbError> {
        let unscored = filter.unscored;
        let mut stmt = self.conn.prepare(&format!(
            "WITH {latest}
             SELECT l.article_id, l.id, e.vector
             FROM latest AS l
             JOIN articles AS a ON a.id = l.article_id
             JOIN article_embeddings AS e ON e.artifact_id = l.id AND e.space_id = ?2
             WHERE json_extract(l.payload, '$.lwr_relevant') = 1
               AND (?3 IS NULL OR coalesce(a.published_at, a.fetched_at) >= ?3)
               AND (?4 IS NULL OR NOT EXISTS (
                 SELECT 1 FROM scores AS s
                 WHERE s.user_id = ?1 AND s.artifact_id = l.id AND s.profile_hash = ?4
                   AND s.backend = ?5 AND s.model = ?6 AND s.prompt_version = ?7))
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?8 OFFSET ?9",
            latest = super::read::latest_digests("?1"),
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![
                user_id,
                space_id,
                filter.since.map(timestamp),
                unscored.map(|k| k.profile_hash),
                unscored.map(|k| k.backend),
                unscored.map(|k| k.model),
                unscored.map(|k| k.prompt_version),
                i64::try_from(limit).unwrap_or(i64::MAX),
                i64::try_from(offset).unwrap_or(i64::MAX),
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (article_id, artifact_id, bytes) = row?;
            let vector = crate::embedding::decode(&bytes).ok_or_else(|| {
                DbError::UnexpectedValue(format!("vector of artifact {artifact_id}"))
            })?;
            Ok(Candidate {
                article_id,
                artifact_id,
                vector,
            })
        })
        .collect()
    }

    /// 利用者の embedding の点数をまとめて保存し、その利用者の今のプロファイル以外の embedding の点数を消す
    /// （LLM の点数は残す）。採点の間に空間が消えたか、プロファイルが替わった（`key.profile_hash` が今のものでない）
    /// なら、何も変えずに `false`。同じ点数がすでにあれば（重なった処理が先に保存した）変えない。
    pub fn save_embedding_scores(
        &self,
        space_id: i64,
        key: ScoreKey<'_>,
        scores: &[EmbeddingScore],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if !self.has_embedding_space(space_id)?
            || self.profile_hash(key.user_id)?.as_deref() != Some(key.profile_hash)
        {
            return Ok(false);
        }
        // プロファイルを変えるたびに全期間の点数が増え続けないよう、今のプロファイル以外の分を消す
        // （score_matches は外部キーで一緒に消える）
        self.conn.execute(
            "DELETE FROM scores WHERE user_id = ?1 AND backend = ?2 AND profile_hash <> ?3",
            rusqlite::params![key.user_id, key.backend, key.profile_hash],
        )?;
        let mut insert = self.conn.prepare(
            "INSERT INTO scores
               (user_id, artifact_id, profile_hash, backend, model, prompt_version, score, reason,
                created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)
             ON CONFLICT DO NOTHING",
        )?;
        let mut matched = self
            .conn
            .prepare("INSERT INTO score_matches (score_id, kind, topic) VALUES (?1, ?2, ?3)")?;
        let created_at = timestamp(now);
        for s in scores {
            let inserted = insert.execute(rusqlite::params![
                key.user_id,
                s.artifact_id,
                key.profile_hash,
                key.backend,
                key.model,
                key.prompt_version,
                s.score,
                created_at,
            ])?;
            if inserted == 0 {
                continue;
            }
            let score_id = self.conn.last_insert_rowid();
            for (kind, topic) in [("interest", &s.interest), ("exclude", &s.exclude)] {
                if let Some(topic) = topic {
                    matched.execute(rusqlite::params![score_id, kind, topic])?;
                }
            }
        }
        drop((insert, matched));
        tx.commit()?;
        Ok(true)
    }

    /// `space_id` の空間（世代）がまだあるか。
    pub(super) fn has_embedding_space(&self, space_id: i64) -> Result<bool, DbError> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM embedding_space WHERE id = ?1",
            [space_id],
            |r| r.get::<_, i64>(0),
        )? == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;
    use crate::embedding::Fingerprint;
    use crate::profile::Interest;

    fn now() -> chrono::DateTime<chrono::Utc> {
        t("2026-10-01T00:00:00Z")
    }

    fn space(db: &Db) -> i64 {
        let fp = Fingerprint {
            query: vec![vec![1.0]],
            document: vec![vec![1.0]],
        };
        db.create_embedding_space("s", 1, &fp, now()).unwrap().id
    }

    fn profile(topic: &str) -> Profile {
        Profile {
            interests: vec![Interest {
                topic: topic.into(),
                weight: 1.0,
                note: None,
            }],
            exclude: vec![],
        }
    }

    fn key<'a>(user_id: i64, hash: &'a str) -> ScoreKey<'a> {
        ScoreKey {
            user_id,
            profile_hash: hash,
            backend: EMBED_BACKEND,
            model: "m",
            prompt_version: 1,
        }
    }

    fn score(artifact_id: i64, score: u8) -> EmbeddingScore {
        EmbeddingScore {
            artifact_id,
            score,
            interest: Some("i".into()),
            exclude: None,
        }
    }

    /// 記事と要約を作り、要約にベクトルを付ける。
    fn embedded(db: &Db, space_id: i64, url: &str, published: &str, relevant: bool) -> (i64, i64) {
        let a = page_article(db, url, published);
        let d = add_digest(db, a, "m", "題", relevant, "2026-09-30T00:00:00Z");
        assert!(
            db.save_article_embeddings(space_id, &[(d, vec![1.0, 0.0])])
                .unwrap()
        );
        (a, d)
    }

    #[test]
    fn lists_every_profile() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        db.save_profile(owner, &profile("a"), now()).unwrap();
        db.save_profile(other, &profile("b"), now()).unwrap();
        let profiles = db.scoring_profiles().unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].user_id, owner);
        assert_eq!(profiles[1].profile, profile("b"));
        assert_eq!(profiles[1].hash, crate::profile::hash(&profile("b")));
    }

    /// 文のベクトルは文そのものをキーにする。消えた空間には保存せず、使われなくなった文は消せる。
    #[test]
    fn keeps_text_vectors_by_text() {
        let db = Db::open_in_memory().unwrap();
        let s = space(&db);
        let texts = ["a".to_string(), "b".to_string()];
        assert!(db.text_embeddings(s, &texts).unwrap().is_empty());
        assert!(
            db.save_text_embeddings(s, &[("a".into(), vec![1.0, 0.0])])
                .unwrap()
        );
        // すでにある文は変えない
        assert!(
            db.save_text_embeddings(s, &[("a".into(), vec![0.0, 1.0])])
                .unwrap()
        );
        let got = db.text_embeddings(s, &texts).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got["a"], vec![1.0, 0.0]);
        assert!(
            !db.save_text_embeddings(s + 1, &[("b".into(), vec![1.0])])
                .unwrap()
        );
        db.save_text_embeddings(s, &[("b".into(), vec![1.0, 0.0])])
            .unwrap();
        // ほかの世代を指定しても消さない
        db.prune_text_embeddings(s + 1, &[]).unwrap();
        assert_eq!(db.text_embeddings(s, &texts).unwrap().len(), 2);
        db.prune_text_embeddings(s, &["b".to_string()]).unwrap();
        assert_eq!(
            db.text_embeddings(s, &texts)
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["b"]
        );
        db.rebuild_embeddings().unwrap();
        assert_eq!(
            db.query_i64("SELECT count(*) FROM text_embeddings")
                .unwrap(),
            0
        );
    }

    /// 対象は、記事の最新の要約で、軽水炉に関係し、ベクトルがあり、利用者が閲覧できるもの。期間と、
    /// 点数の有無で絞れる。
    #[test]
    fn lists_candidates() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let s = space(&db);
        let (new, d_new) = embedded(
            &db,
            s,
            "https://e.com/new",
            "2026-09-30T00:00:00.000Z",
            true,
        );
        let (old, d_old) = embedded(
            &db,
            s,
            "https://e.com/old",
            "2020-01-01T00:00:00.000Z",
            true,
        );
        embedded(&db, s, "https://e.com/x", "2026-09-30T00:00:00.000Z", false);
        // ベクトルの無い要約は対象にしない
        let bare = page_article(&db, "https://e.com/bare", "2026-09-30T00:00:00.000Z");
        add_digest(&db, bare, "m", "題", true, "2026-09-30T00:00:00Z");
        let ids = |filter: CandidateFilter, offset: usize, limit: usize| -> Vec<i64> {
            db.embedding_candidates(owner, s, filter, offset, limit)
                .unwrap()
                .iter()
                .map(|c| c.artifact_id)
                .collect()
        };
        assert_eq!(ids(CandidateFilter::default(), 0, 10), [d_new, d_old]);
        assert_eq!(ids(CandidateFilter::default(), 1, 10), [d_old]);
        assert_eq!(ids(CandidateFilter::default(), 0, 1), [d_new]);
        let recent = CandidateFilter {
            since: Some(t("2026-09-01T00:00:00Z")),
            unscored: None,
        };
        assert_eq!(ids(recent, 0, 10), [d_new]);
        let candidates = db.embedding_candidates(owner, s, recent, 0, 10).unwrap();
        assert_eq!(
            candidates,
            [Candidate {
                article_id: new,
                artifact_id: d_new,
                vector: vec![1.0, 0.0]
            }]
        );
        let k = key(owner, "h");
        assert!(db.save_profile(owner, &profile("a"), now()).is_ok());
        let hash = crate::profile::hash(&profile("a"));
        let k = ScoreKey {
            profile_hash: &hash,
            ..k
        };
        assert!(
            db.save_embedding_scores(s, k, &[score(d_old, 10)], now())
                .unwrap()
        );
        let unscored = CandidateFilter {
            since: None,
            unscored: Some(k),
        };
        assert_eq!(ids(unscored, 0, 10), [d_new]);
        // 要約を作り直せば、新しい要約が対象になる
        let d_redo = add_digest(&db, old, "m2", "題", true, "2026-10-01T00:00:00Z");
        assert!(
            db.save_article_embeddings(s, &[(d_redo, vec![0.0, 1.0])])
                .unwrap()
        );
        assert_eq!(ids(unscored, 0, 10), [d_new, d_redo]);
    }

    /// `eval` の材料は、評価した記事のうち採点の対象になるもの。ソースとトピックも返す。
    #[test]
    fn lists_rated_articles_for_eval() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let s = space(&db);
        let (rated, _) = embedded(&db, s, "https://e.com/a", "2026-09-30T00:00:00.000Z", true);
        embedded(&db, s, "https://e.com/b", "2026-09-30T00:00:00.000Z", true);
        let (unrelated, _) = embedded(&db, s, "https://e.com/c", "2026-09-30T00:00:00.000Z", false);
        for a in [rated, unrelated] {
            db.rate(owner, a, Rating::new(4), now()).unwrap();
        }
        assert_eq!(
            db.eval_embedding_inputs(owner, s).unwrap(),
            [LabeledVector {
                article_id: rated,
                source_id: "s".into(),
                topics: vec!["規制・審査".into()],
                vector: vec![1.0, 0.0],
            }]
        );
    }

    /// 会員限定の本文から作った要約は、資格の無い利用者の対象にしない。
    #[test]
    fn hides_digests_the_user_cannot_view() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let s = space(&db);
        let m = insert_membership(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-30T00:00:00.000Z");
        let gated = insert_content(&db, a, Some(m));
        let payload = serde_json::json!({
            "title_ja": "会員限定", "summary_ja": "s", "points_ja": ["p"],
            "implications_ja": "", "lwr_relevant": true, "topics": [],
        });
        let d = db
            .insert_artifact(
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
                now(),
            )
            .unwrap();
        db.save_article_embeddings(s, &[(d, vec![1.0])]).unwrap();
        let list = |db: &Db| {
            db.embedding_candidates(owner, s, CandidateFilter::default(), 0, 10)
                .unwrap()
                .len()
        };
        assert_eq!(list(&db), 0);
        db.conn()
            .execute("INSERT INTO user_memberships VALUES (?1, ?2)", [owner, m])
            .unwrap();
        assert_eq!(list(&db), 1);
    }

    /// 点数と特徴を保存し、同じ点数は 2 度入れない。今のプロファイル以外の embedding の点数は消すが、
    /// LLM の点数とほかの利用者の点数は残す。
    #[test]
    fn saves_scores_for_the_current_profile() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        let s = space(&db);
        let (_, d) = embedded(&db, s, "https://e.com/a", "2026-09-30T00:00:00.000Z", true);
        let old_hash = crate::profile::hash(&profile("old"));
        let new_hash = crate::profile::hash(&profile("new"));
        db.save_profile(other, &profile("old"), now()).unwrap();
        db.save_profile(owner, &profile("old"), now()).unwrap();
        assert!(
            db.save_embedding_scores(s, key(owner, &old_hash), &[score(d, 10)], now())
                .unwrap()
        );
        assert!(
            db.save_embedding_scores(s, key(other, &old_hash), &[score(d, 20)], now())
                .unwrap()
        );
        db.insert_score(
            ScoreKey {
                backend: "claude-cli",
                ..key(owner, &old_hash)
            },
            d,
            30,
            None,
            now(),
        )
        .unwrap();
        db.save_profile(owner, &profile("new"), now()).unwrap();
        let scores = [EmbeddingScore {
            artifact_id: d,
            score: 70,
            interest: Some("new".into()),
            exclude: Some("x".into()),
        }];
        assert!(
            db.save_embedding_scores(s, key(owner, &new_hash), &scores, now())
                .unwrap()
        );
        assert!(
            db.save_embedding_scores(s, key(owner, &new_hash), &scores, now())
                .unwrap()
        );
        assert_eq!(
            db.query_strings(
                "SELECT user_id || ':' || backend || ':' || score FROM scores ORDER BY user_id, backend, score"
            )
            .unwrap(),
            [
                format!("{owner}:claude-cli:30"),
                format!("{owner}:embedding:70"),
                format!("{other}:embedding:20"),
            ]
        );
        assert_eq!(
            db.query_strings(
                "SELECT m.kind || ':' || m.topic FROM score_matches AS m
                 JOIN scores AS s ON s.id = m.score_id
                 WHERE s.user_id = (SELECT id FROM users WHERE is_owner = 1) AND s.backend = 'embedding'
                 ORDER BY m.kind"
            )
            .unwrap(),
            ["exclude:x", "interest:new"]
        );
        db.rebuild_embeddings().unwrap();
        assert_eq!(
            db.query_i64("SELECT count(*) FROM scores WHERE backend = 'embedding'")
                .unwrap(),
            0
        );
        assert_eq!(db.query_i64("SELECT count(*) FROM scores").unwrap(), 1);
    }

    /// 採点の間にプロファイルが替わったか、空間が消えたなら、何も保存せず、今の点数も消さない。
    #[test]
    fn discards_scores_of_a_stale_profile_or_space() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let s = space(&db);
        let (_, d) = embedded(&db, s, "https://e.com/a", "2026-09-30T00:00:00.000Z", true);
        let old_hash = crate::profile::hash(&profile("old"));
        let new_hash = crate::profile::hash(&profile("new"));
        db.save_profile(owner, &profile("new"), now()).unwrap();
        assert!(
            db.save_embedding_scores(s, key(owner, &new_hash), &[score(d, 70)], now())
                .unwrap()
        );
        assert!(
            !db.save_embedding_scores(s, key(owner, &old_hash), &[score(d, 10)], now())
                .unwrap()
        );
        assert!(
            !db.save_embedding_scores(s + 1, key(owner, &new_hash), &[score(d, 10)], now())
                .unwrap()
        );
        assert_eq!(
            db.query_strings("SELECT profile_hash || ':' || score FROM scores")
                .unwrap(),
            [format!("{new_hash}:70")]
        );
    }
}
