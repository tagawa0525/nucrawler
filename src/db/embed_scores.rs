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

impl Db {
    /// プロファイルのある利用者（利用者の id 順）。
    pub fn scoring_profiles(&self) -> Result<Vec<ScoringProfile>, DbError> {
        todo!()
    }

    /// `texts` のうち、`space_id` の空間にベクトルがある文のベクトル。
    pub fn text_embeddings(
        &self,
        space_id: i64,
        texts: &[String],
    ) -> Result<HashMap<String, Vec<f32>>, DbError> {
        todo!()
    }

    /// 文のベクトルを保存する。`space_id` の空間がもう無ければ何も保存せず `false`。すでにある文は変えない。
    pub fn save_text_embeddings(
        &self,
        space_id: i64,
        vectors: &[(String, Vec<f32>)],
    ) -> Result<bool, DbError> {
        todo!()
    }

    /// `keep` に無い文のベクトルを消す（今のどのプロファイルにも使われなくなったもの）。
    pub fn prune_text_embeddings(&self, keep: &[String]) -> Result<(), DbError> {
        todo!()
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
        todo!()
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
        todo!()
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
        db.prune_text_embeddings(&["b".to_string()]).unwrap();
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
