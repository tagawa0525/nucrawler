//! 記事の embedding とその空間（計画 010）。

use super::*;
use crate::embedding::Fingerprint;

/// 今のベクトルの空間（`embedding_space` の 1 行）。
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingSpace {
    /// 世代。`rebuild` の後は新しい値になる
    pub id: i64,
    pub name: String,
    pub input_version: i64,
    pub fingerprint: Fingerprint,
}

/// `embed` ステージの作業の予約と失敗の記録に使う backend。
pub const EMBED_BACKEND: &str = "embedding";

/// 作業の予約のステージ名。世代を含め、`rebuild` の前の予約と混ざらないようにする。
pub fn embed_claim_stage(space_id: i64) -> String {
    format!("embed:{space_id}")
}

/// 失敗の記録のステージ名。`stage_errors` の主キーは記事の ID までなので、要約（artifact）と世代を名前に含め、
/// 同じ記事の古い要約や、`rebuild` の前の世代の失敗が、今の要約を止めないようにする。
pub fn embed_failure_stage(space_id: i64, artifact_id: i64) -> String {
    format!("embed:{space_id}:{artifact_id}")
}

/// embedding にする要約。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedInput {
    pub article_id: i64,
    pub artifact_id: i64,
    pub title_ja: String,
    pub summary_ja: String,
    pub points_ja: Vec<String>,
}

impl Db {
    /// 今の空間。まだ作っていなければ `None`。
    pub fn embedding_space(&self) -> Result<Option<EmbeddingSpace>, DbError> {
        todo!()
    }

    /// 空間を作る。すでにあれば（同時に始まったほかの実行が先に作った）作らずに、その空間を返す。
    pub fn create_embedding_space(
        &self,
        name: &str,
        input_version: i64,
        fingerprint: &Fingerprint,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<EmbeddingSpace, DbError> {
        todo!()
    }

    /// `space_id` の空間にベクトルが無い要約を、記事の新しい順に最大 `limit` 件返す。期間は区切らない。
    /// この世代でこの要約の失敗が再試行待ち・断念済みのものと、ほかの実行が予約している記事は含めない。
    pub fn pending_embeddings(
        &self,
        space_id: i64,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<EmbedInput>, DbError> {
        todo!()
    }

    /// 要約のベクトルを保存する。`space_id` の空間がもう無ければ（`rebuild` で消えた）何も保存せず `false`。
    /// すでにベクトルがある要約は変えない（同じ空間なら同じベクトルになるので、重なった処理が作ったものでよい）。
    pub fn save_article_embeddings(
        &self,
        space_id: i64,
        vectors: &[(i64, Vec<f32>)],
    ) -> Result<bool, DbError> {
        todo!()
    }

    /// 空間を消し、ベクトルと `embed` の失敗の記録・予約もすべて消す。次の `embed` が新しい空間で作り直す。
    pub fn rebuild_embeddings(&self) -> Result<(), DbError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    fn fp(x: f32) -> Fingerprint {
        Fingerprint {
            query: vec![vec![x, 0.0]],
            document: vec![vec![0.0, x]],
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        t("2026-10-01T00:00:00Z")
    }

    /// 同時に始まった実行が別々に空間を作ろうとしても、1 行だけになり、後の側は先の側の空間を受け取る。
    /// `rebuild` の後に作った空間は、前と違う世代になる。
    #[test]
    fn keeps_a_single_space_and_new_generations() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.embedding_space().unwrap(), None);
        let first = db.create_embedding_space("a", 1, &fp(1.0), now()).unwrap();
        let second = db.create_embedding_space("b", 2, &fp(2.0), now()).unwrap();
        assert_eq!(second, first);
        assert_eq!(db.embedding_space().unwrap(), Some(first.clone()));
        assert_eq!((first.name.as_str(), first.input_version), ("a", 1));
        assert_eq!(first.fingerprint, fp(1.0));
        db.rebuild_embeddings().unwrap();
        assert_eq!(db.embedding_space().unwrap(), None);
        let next = db.create_embedding_space("a", 1, &fp(1.0), now()).unwrap();
        assert!(next.id > first.id, "{next:?} {first:?}");
    }

    /// 期間によらず、ベクトルの無い要約を記事の新しい順に返す。古い要約も、同じ記事の新しい要約も対象。
    #[test]
    fn lists_digests_without_vectors() {
        let db = Db::open_in_memory().unwrap();
        let space = db.create_embedding_space("a", 1, &fp(1.0), now()).unwrap();
        let old = page_article(&db, "https://e.com/old", "2020-01-01T00:00:00.000Z");
        let new = page_article(&db, "https://e.com/new", "2026-09-30T00:00:00.000Z");
        let d_old = add_digest(&db, old, "m", "古い", true, "2026-09-30T00:00:00Z");
        let d_new = add_digest(&db, new, "m", "新しい", false, "2026-09-30T00:00:00Z");
        let pending = db.pending_embeddings(space.id, now(), 10).unwrap();
        assert_eq!(
            pending,
            [
                EmbedInput {
                    article_id: new,
                    artifact_id: d_new,
                    title_ja: "新しい".into(),
                    summary_ja: "新しいの要約".into(),
                    points_ja: vec!["点".into()],
                },
                EmbedInput {
                    article_id: old,
                    artifact_id: d_old,
                    title_ja: "古い".into(),
                    summary_ja: "古いの要約".into(),
                    points_ja: vec!["点".into()],
                },
            ]
        );
        assert_eq!(db.pending_embeddings(space.id, now(), 1).unwrap().len(), 1);
        assert!(
            db.save_article_embeddings(space.id, &[(d_new, vec![1.0, 0.0])])
                .unwrap()
        );
        let ids: Vec<i64> = db
            .pending_embeddings(space.id, now(), 10)
            .unwrap()
            .iter()
            .map(|p| p.artifact_id)
            .collect();
        assert_eq!(ids, [d_old]);
        // 要約を作り直せば、新しい要約のベクトルを作る
        let d_redo = add_digest(&db, new, "m2", "作り直し", false, "2026-10-01T00:00:00Z");
        let ids: Vec<i64> = db
            .pending_embeddings(space.id, now(), 10)
            .unwrap()
            .iter()
            .map(|p| p.artifact_id)
            .collect();
        assert_eq!(ids, [d_redo, d_old]);
    }

    /// この世代のこの要約の失敗だけが、その要約を止める。同じ記事のほかの要約や、前の世代の失敗は止めない。
    /// ほかの実行が予約している記事も除く。
    #[test]
    fn skips_failed_and_claimed_digests() {
        let db = Db::open_in_memory().unwrap();
        let space = db.create_embedding_space("a", 1, &fp(1.0), now()).unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-30T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-29T00:00:00.000Z");
        let d_a1 = add_digest(&db, a, "m", "a1", true, "2026-09-30T00:00:00Z");
        let d_b = add_digest(&db, b, "m", "b", true, "2026-09-30T00:00:00Z");
        let fail = |space_id: i64, artifact_id: i64, article_id: i64| {
            let stage = embed_failure_stage(space_id, artifact_id);
            db.record_stage_failure(
                StageKey {
                    article_id,
                    stage: &stage,
                    backend: EMBED_BACKEND,
                    model: "",
                },
                "too long",
                now(),
                true,
            )
            .unwrap();
        };
        fail(space.id, d_a1, a);
        let d_a2 = add_digest(&db, a, "m2", "a2", true, "2026-10-01T00:00:00Z");
        fail(space.id - 1, d_b, b);
        let ids: Vec<i64> = db
            .pending_embeddings(space.id, now(), 10)
            .unwrap()
            .iter()
            .map(|p| p.artifact_id)
            .collect();
        assert_eq!(ids, [d_a2, d_b]);
        let stage = embed_claim_stage(space.id);
        let _claim = db
            .claim(
                ClaimKey {
                    stage: &stage,
                    backend: EMBED_BACKEND,
                    model: "",
                },
                &[b],
                now(),
                chrono::Duration::minutes(10),
            )
            .unwrap();
        let ids: Vec<i64> = db
            .pending_embeddings(space.id, now(), 10)
            .unwrap()
            .iter()
            .map(|p| p.artifact_id)
            .collect();
        assert_eq!(ids, [d_a2]);
    }

    /// 消えた世代への保存は何もしない。同じ要約を 2 度保存しても誤りにならず、先のベクトルが残る。
    #[test]
    fn saves_only_into_the_current_generation() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-30T00:00:00.000Z");
        let d = add_digest(&db, a, "m", "a", true, "2026-09-30T00:00:00Z");
        let old = db.create_embedding_space("a", 1, &fp(1.0), now()).unwrap();
        assert!(
            db.save_article_embeddings(old.id, &[(d, vec![1.0, 0.0])])
                .unwrap()
        );
        assert!(
            db.save_article_embeddings(old.id, &[(d, vec![0.0, 1.0])])
                .unwrap()
        );
        let saved: Vec<u8> = db
            .conn()
            .query_row("SELECT vector FROM article_embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(crate::embedding::decode(&saved), Some(vec![1.0, 0.0]));
        db.rebuild_embeddings().unwrap();
        assert_eq!(
            db.query_i64("SELECT count(*) FROM article_embeddings")
                .unwrap(),
            0
        );
        assert!(
            !db.save_article_embeddings(old.id, &[(d, vec![1.0, 0.0])])
                .unwrap()
        );
        let new = db.create_embedding_space("a", 1, &fp(1.0), now()).unwrap();
        assert!(
            !db.save_article_embeddings(old.id, &[(d, vec![1.0, 0.0])])
                .unwrap()
        );
        assert_eq!(
            db.query_i64("SELECT count(*) FROM article_embeddings")
                .unwrap(),
            0
        );
        assert!(
            db.save_article_embeddings(new.id, &[(d, vec![1.0, 0.0])])
                .unwrap()
        );
    }

    /// `rebuild` は `embed` の失敗の記録と予約も消し、ほかのステージのものは残す。
    #[test]
    fn rebuild_clears_embed_failures_and_claims() {
        let db = Db::open_in_memory().unwrap();
        let space = db.create_embedding_space("a", 1, &fp(1.0), now()).unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-30T00:00:00.000Z");
        let d = add_digest(&db, a, "m", "a", true, "2026-09-30T00:00:00Z");
        let failure = embed_failure_stage(space.id, d);
        for stage in [failure.as_str(), "extract"] {
            db.record_stage_failure(
                StageKey {
                    article_id: a,
                    stage,
                    backend: EMBED_BACKEND,
                    model: "",
                },
                "x",
                now(),
                false,
            )
            .unwrap();
        }
        let claim = embed_claim_stage(space.id);
        std::mem::forget(
            db.claim(
                ClaimKey {
                    stage: &claim,
                    backend: EMBED_BACKEND,
                    model: "",
                },
                &[a],
                now(),
                chrono::Duration::minutes(10),
            )
            .unwrap(),
        );
        db.rebuild_embeddings().unwrap();
        assert_eq!(
            db.query_strings("SELECT stage FROM stage_errors").unwrap(),
            ["extract"]
        );
        assert_eq!(db.query_i64("SELECT count(*) FROM work_claims").unwrap(), 0);
    }
}
