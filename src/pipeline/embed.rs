//! 要約の embedding を作る（計画 010）。LLM を使わないので、LLM が使えない実行でも動く。
//!
//! 期間は区切らず、ベクトルの無い要約を新しい順に作る。呼び出しのたびにモデルの指紋の試験文も一緒に送り、
//! 返った指紋が保存した空間のものと一致したときだけ保存する。空間の設定・入力の版・指紋のどれかが違えば、
//! 止めて `nucrawler embed rebuild` を案内する（違う空間のベクトルを混ぜない）。

use chrono::{DateTime, Utc};

use super::Cancel;
use crate::config::EmbeddingConfig;
use crate::db::{
    ClaimKey, Db, DbError, EMBED_BACKEND, EmbedInput, EmbeddingSpace, StageKey, embed_claim_stage,
    embed_failure_stage,
};
use crate::embedding::{
    EmbedError, Embedder, FINGERPRINT_TEXTS, Fingerprint, INPUT_VERSION, Role, fingerprint_inputs,
    input, space_name,
};

#[derive(Debug, thiserror::Error)]
pub enum EmbedStageError {
    #[error(transparent)]
    Db(#[from] DbError),
    /// 保存した空間と、今の設定やモデルが違う
    #[error(
        "the embedding space changed ({0}); run `nucrawler embed rebuild` to embed every digest again"
    )]
    SpaceChanged(String),
    /// サービスの側の失敗（止まっている・認証・上限・応答の誤り）。記事の失敗としては数えない
    #[error("embedding api failed")]
    Api(#[source] EmbedError),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EmbedSummary {
    pub embedded: usize,
    /// 文のせいで作れなかった要約（再試行を待つ）
    pub failed: usize,
    pub calls: usize,
}

/// 要約を embedding にする文：見出しの和訳・要約・要点を改行でつなぐ。
/// つなぎ方を変えたら `embedding::INPUT_VERSION` を上げる。
pub fn document_text(digest: &EmbedInput) -> String {
    todo!()
}

/// ベクトルの無い要約を、無くなるか中断されるまで作る。
pub async fn embed_articles(
    db: &Db,
    embedder: &impl Embedder,
    cfg: &EmbeddingConfig,
    cancel: &Cancel,
    clock: &dyn Fn() -> DateTime<Utc>,
) -> Result<EmbedSummary, EmbedStageError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{ArtifactKind, ContentKind, ContentOrigin, NewArticle, NewArtifact};
    use crate::embedding::fake::{FakeEmbedder, cfg};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn small_batches() -> EmbeddingConfig {
        // 試験文 2 件と要約 2 件
        EmbeddingConfig {
            batch_size: 4,
            ..cfg("http://unused/e")
        }
    }

    /// `published` に公開された記事と、その要約（見出しは `title`）を作り、要約の id を返す。
    fn digest(db: &Db, title: &str, published: &str) -> i64 {
        let url = format!("https://e.com/{title}");
        let id = db
            .insert_article(&NewArticle {
                source_id: "s",
                url: &url,
                title: "t",
                lang: Lang::En,
                published_at: Some(published),
            })
            .unwrap()
            .unwrap();
        let c = db
            .insert_content(id, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let payload = serde_json::json!({
            "title_ja": title, "summary_ja": format!("{title}の要約"), "points_ja": ["点1", "点2"],
            "implications_ja": "", "lwr_relevant": true, "topics": [],
        });
        db.insert_artifact(
            &NewArtifact {
                article_id: id,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &payload,
                inputs: &[c],
                glossary_at: None,
            },
            now(),
        )
        .unwrap()
    }

    fn saved(db: &Db, artifact_id: i64) -> Option<Vec<f32>> {
        use rusqlite::OptionalExtension;
        db.conn()
            .query_row(
                "SELECT vector FROM article_embeddings WHERE artifact_id = ?1",
                [artifact_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .unwrap()
            .map(|b| crate::embedding::decode(&b).unwrap())
    }

    async fn run(
        db: &Db,
        embedder: &FakeEmbedder,
        cfg: &EmbeddingConfig,
    ) -> Result<EmbedSummary, EmbedStageError> {
        embed_articles(db, embedder, cfg, &Cancel::default(), &now).await
    }

    #[test]
    fn joins_title_summary_and_points() {
        let digest = EmbedInput {
            article_id: 1,
            artifact_id: 2,
            title_ja: "見出し".into(),
            summary_ja: "要約".into(),
            points_ja: vec!["点1".into(), "点2".into()],
        };
        assert_eq!(document_text(&digest), "見出し\n要約\n点1\n点2");
    }

    /// 初回は空間（指紋）を作り、期間によらずすべての要約を、文書の接頭辞を付けて作る。呼び出しには毎回、
    /// 指紋の試験文を先頭に入れる。作ったものは次の実行で作り直さない。
    #[tokio::test]
    async fn embeds_every_digest_once() {
        let db = Db::open_in_memory().unwrap();
        let old = digest(&db, "古い", "2020-01-01T00:00:00.000Z");
        let a = digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let b = digest(&db, "b", "2026-09-29T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        let cfg = small_batches();
        let summary = run(&db, &embedder, &cfg).await.unwrap();
        // 指紋（クエリと文書）で 2 回、要約 3 件を 2 件ずつで 2 回
        assert_eq!(
            summary,
            EmbedSummary {
                embedded: 3,
                failed: 0,
                calls: 4
            }
        );
        let calls = embedder.calls();
        let fingerprint = fingerprint_inputs(&cfg, Role::Document);
        for call in &calls[2..] {
            assert_eq!(call[..FINGERPRINT_TEXTS], fingerprint[..]);
        }
        assert_eq!(calls[2][2], "検索文書: a\naの要約\n点1\n点2");
        let space = db.embedding_space().unwrap().unwrap();
        assert_eq!(space.name, space_name(&cfg));
        assert_eq!(space.input_version, INPUT_VERSION);
        assert_eq!(
            saved(&db, a),
            Some(FakeEmbedder::vector("", "検索文書: a\naの要約\n点1\n点2"))
        );
        assert!(saved(&db, b).is_some() && saved(&db, old).is_some());
        let again = run(&db, &embedder, &cfg).await.unwrap();
        assert_eq!(again, EmbedSummary::default());
        assert_eq!(embedder.calls().len(), 4);
    }

    /// 設定（空間の名前）が変われば、何も呼ばずに止まり、作り直しを案内する。
    #[tokio::test]
    async fn stops_when_the_settings_change() {
        let db = Db::open_in_memory().unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        run(&db, &embedder, &small_batches()).await.unwrap();
        let b = digest(&db, "b", "2026-09-30T00:00:00.000Z");
        let changed = EmbeddingConfig {
            model: "m2".into(),
            ..small_batches()
        };
        let calls = embedder.calls().len();
        let err = run(&db, &embedder, &changed).await.unwrap_err();
        assert!(matches!(err, EmbedStageError::SpaceChanged(_)), "{err:?}");
        assert!(err.to_string().contains("nucrawler embed rebuild"), "{err}");
        assert_eq!(embedder.calls().len(), calls);
        assert_eq!(saved(&db, b), None);
    }

    /// 設定が同じでも、サーバーの中身のモデルが替われば（指紋が違う）、その呼び出しの結果を保存せずに止まる。
    #[tokio::test]
    async fn stops_when_the_model_changes_under_the_same_name() {
        let db = Db::open_in_memory().unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        run(&db, &embedder, &small_batches()).await.unwrap();
        let b = digest(&db, "b", "2026-09-30T00:00:00.000Z");
        *embedder.model.lock().unwrap() = "other".into();
        let err = run(&db, &embedder, &small_batches()).await.unwrap_err();
        assert!(matches!(err, EmbedStageError::SpaceChanged(_)), "{err:?}");
        assert_eq!(saved(&db, b), None);
    }

    /// 文のせいの失敗は、まとめて送った要約を 1 件ずつ送り直し、失敗した要約だけを記録して後で再試行する。
    #[tokio::test]
    async fn fails_only_the_digest_that_cannot_be_embedded() {
        let db = Db::open_in_memory().unwrap();
        let good = digest(&db, "good", "2026-09-30T00:00:00.000Z");
        let bad = digest(&db, "bad", "2026-09-29T00:00:00.000Z");
        let embedder = FakeEmbedder {
            bad: Some("bad".into()),
            ..FakeEmbedder::default()
        };
        let summary = run(&db, &embedder, &small_batches()).await.unwrap();
        assert_eq!((summary.embedded, summary.failed), (1, 1));
        assert!(saved(&db, good).is_some());
        assert_eq!(saved(&db, bad), None);
        let space = db.embedding_space().unwrap().unwrap();
        assert_eq!(
            db.query_strings("SELECT stage FROM stage_errors").unwrap(),
            [embed_failure_stage(space.id, bad)]
        );
        // 再試行の時刻までは呼ばない
        let calls = embedder.calls().len();
        assert_eq!(
            run(&db, &embedder, &small_batches()).await.unwrap(),
            EmbedSummary::default()
        );
        assert_eq!(embedder.calls().len(), calls);
    }

    /// サービスの側の失敗では、要約の失敗を記録せずに止める（止まっている間に再試行の上限を使い切らない）。
    #[tokio::test]
    async fn stops_on_service_errors_without_recording_failures() {
        let db = Db::open_in_memory().unwrap();
        let a = digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        let cfg = small_batches();
        // 空間は作っておき、要約の呼び出しで失敗させる
        let fp = Fingerprint::make(&embedder, &cfg).await.unwrap();
        db.create_embedding_space(&space_name(&cfg), INPUT_VERSION, &fp, now())
            .unwrap();
        embedder
            .errors
            .lock()
            .unwrap()
            .push_back(EmbedError::Status {
                status: 503,
                body: "down".into(),
            });
        let err = run(&db, &embedder, &cfg).await.unwrap_err();
        assert!(matches!(err, EmbedStageError::Api(_)), "{err:?}");
        assert_eq!(saved(&db, a), None);
        assert_eq!(
            db.query_i64("SELECT count(*) FROM stage_errors").unwrap(),
            0
        );
        assert_eq!(db.query_i64("SELECT count(*) FROM work_claims").unwrap(), 0);
    }

    /// 入力の組み立て方の版が違う空間では、止めて作り直しを案内する。
    #[tokio::test]
    async fn stops_when_the_input_version_differs() {
        let db = Db::open_in_memory().unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        let cfg = small_batches();
        let fp = Fingerprint::make(&embedder, &cfg).await.unwrap();
        db.create_embedding_space(&space_name(&cfg), INPUT_VERSION - 1, &fp, now())
            .unwrap();
        let err = run(&db, &embedder, &cfg).await.unwrap_err();
        assert!(matches!(err, EmbedStageError::SpaceChanged(_)), "{err:?}");
    }

    /// 中断が要求されていれば、何も呼ばずに終える。
    #[tokio::test]
    async fn stops_when_cancelled() {
        let db = Db::open_in_memory().unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        let cancel = Cancel::default();
        cancel.request();
        let summary = embed_articles(&db, &embedder, &small_batches(), &cancel, &now)
            .await
            .unwrap();
        assert_eq!(summary, EmbedSummary::default());
        assert!(embedder.calls().is_empty());
    }
}
