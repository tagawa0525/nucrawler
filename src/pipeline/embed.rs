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
    let mut parts = vec![digest.title_ja.as_str(), digest.summary_ja.as_str()];
    parts.extend(digest.points_ja.iter().map(String::as_str));
    parts.join("\n")
}

/// 1 回の呼び出しの結果。
enum Outcome {
    Saved,
    /// 文のせいの失敗（まとめて送ったなら、1 件ずつ送り直す）
    InputError(EmbedError),
    Cancelled,
    /// 空間が消えた（`rebuild` された）ので、このステージは何も保存せずに終える
    Rebuilt,
}

struct Run<'a, E> {
    db: &'a Db,
    embedder: &'a E,
    cfg: &'a EmbeddingConfig,
    cancel: &'a Cancel,
    clock: &'a dyn Fn() -> DateTime<Utc>,
    space: EmbeddingSpace,
    summary: EmbedSummary,
}

impl<E: Embedder> Run<'_, E> {
    /// `digests` を 1 回で送り、指紋が空間と一致すれば保存する。
    async fn embed(&mut self, digests: &[EmbedInput]) -> Result<Outcome, EmbedStageError> {
        let mut inputs = fingerprint_inputs(self.cfg, Role::Document);
        inputs.extend(
            digests
                .iter()
                .map(|d| input(self.cfg, Role::Document, &document_text(d))),
        );
        let result = tokio::select! {
            r = self.embedder.embed(&inputs) => r,
            () = self.cancel.requested() => return Ok(Outcome::Cancelled),
        };
        self.summary.calls += 1;
        let vectors = match result {
            Ok(vectors) => vectors,
            Err(e) if e.is_input_error() => return Ok(Outcome::InputError(e)),
            Err(e) => return Err(EmbedStageError::Api(e)),
        };
        let (fingerprint, vectors) = vectors.split_at(FINGERPRINT_TEXTS);
        if !self.space.fingerprint.matches(Role::Document, fingerprint) {
            return Err(EmbedStageError::SpaceChanged(
                "the model behind the same settings returns different vectors".into(),
            ));
        }
        let pairs: Vec<(i64, Vec<f32>)> = digests
            .iter()
            .map(|d| d.artifact_id)
            .zip(vectors.iter().cloned())
            .collect();
        if !self.db.save_article_embeddings(self.space.id, &pairs)? {
            return Ok(Outcome::Rebuilt);
        }
        for d in digests {
            self.db.clear_stage_failure(
                self.failure_key(d, &embed_failure_stage(self.space.id, d.artifact_id)),
            )?;
        }
        self.summary.embedded += digests.len();
        Ok(Outcome::Saved)
    }

    /// 指紋の試験文だけで呼び、失敗すればサービスの側の失敗にする。
    /// 中断が要求されれば `false`。
    async fn probe(&mut self) -> Result<bool, EmbedStageError> {
        let inputs = fingerprint_inputs(self.cfg, Role::Document);
        let result = tokio::select! {
            r = self.embedder.embed(&inputs) => r,
            () = self.cancel.requested() => return Ok(false),
        };
        self.summary.calls += 1;
        result.map(|_| true).map_err(EmbedStageError::Api)
    }

    fn failure_key<'k>(&self, digest: &EmbedInput, stage: &'k str) -> StageKey<'k> {
        StageKey {
            article_id: digest.article_id,
            stage,
            backend: EMBED_BACKEND,
            model: "",
        }
    }

    /// 文のせいで作れなかった要約を記録する（間を空けて再試行する）。
    fn record_failure(&mut self, digest: &EmbedInput, error: &EmbedError) -> Result<(), DbError> {
        let stage = embed_failure_stage(self.space.id, digest.artifact_id);
        tracing::warn!(
            article_id = digest.article_id,
            "failed to embed the digest: {error}"
        );
        self.db.record_stage_failure(
            self.failure_key(digest, &stage),
            &error.to_string(),
            (self.clock)(),
            false,
        )?;
        self.summary.failed += 1;
        Ok(())
    }
}

/// ベクトルの無い要約を、無くなるか中断されるまで作る。
pub async fn embed_articles(
    db: &Db,
    embedder: &impl Embedder,
    cfg: &EmbeddingConfig,
    cancel: &Cancel,
    clock: &dyn Fn() -> DateTime<Utc>,
) -> Result<EmbedSummary, EmbedStageError> {
    if cancel.is_requested() {
        return Ok(EmbedSummary::default());
    }
    let mut summary = EmbedSummary::default();
    let name = space_name(cfg);
    let space = match db.embedding_space()? {
        Some(space) => space,
        None => {
            let fingerprint = tokio::select! {
                r = Fingerprint::make(embedder, cfg) => r.map_err(EmbedStageError::Api)?,
                () = cancel.requested() => return Ok(summary),
            };
            summary.calls += 2;
            db.create_embedding_space(&name, INPUT_VERSION, &fingerprint, clock())?
        }
    };
    if space.name != name {
        return Err(EmbedStageError::SpaceChanged(format!(
            "the settings changed from {} to {name}",
            space.name
        )));
    }
    if space.input_version != INPUT_VERSION {
        return Err(EmbedStageError::SpaceChanged(format!(
            "the input version changed from {} to {INPUT_VERSION}",
            space.input_version
        )));
    }
    let claim_stage = embed_claim_stage(space.id);
    let key = ClaimKey {
        stage: &claim_stage,
        backend: EMBED_BACKEND,
        model: "",
    };
    // 予約は、まとめた呼び出し・指紋だけの確かめ・1 件ずつの送り直し（最大 batch_size + 2 回）がすべて
    // タイムアウトしても切れない長さにする
    // （設定の検証で、どちらも上限があり、積は i64 に収まる）
    let calls = cfg.batch_size as i64 + 2;
    let ttl = chrono::Duration::seconds(cfg.timeout_secs as i64 * calls);
    let mut run = Run {
        db,
        embedder,
        cfg,
        cancel,
        clock,
        space,
        summary,
    };
    let per_batch = cfg.batch_size - FINGERPRINT_TEXTS;
    while !cancel.is_requested() {
        let now = clock();
        let (digests, _claim) = db.claim_selected(
            key,
            now,
            ttl,
            |db| db.pending_embeddings(run.space.id, now, per_batch),
            |d| d.article_id,
        )?;
        if digests.is_empty() {
            break;
        }
        match run.embed(&digests).await? {
            Outcome::Saved => {}
            Outcome::Cancelled => break,
            Outcome::Rebuilt => {
                tracing::warn!("the embedding space was rebuilt during this run; stopping");
                break;
            }
            Outcome::InputError(e) => {
                // 400 などは、文のせいでなく設定の誤り（モデル名・次元）でも返る。指紋の試験文だけでも失敗するなら、
                // どの要約でも失敗するので、要約の失敗として記録せずに止める
                if !run.probe().await? {
                    break;
                }
                if let [digest] = digests.as_slice() {
                    run.record_failure(digest, &e)?;
                    continue;
                }
                for digest in &digests {
                    match run.embed(std::slice::from_ref(digest)).await? {
                        Outcome::Saved => {}
                        Outcome::InputError(e) => run.record_failure(digest, &e)?,
                        Outcome::Cancelled => return Ok(run.summary),
                        Outcome::Rebuilt => {
                            tracing::warn!(
                                "the embedding space was rebuilt during this run; stopping"
                            );
                            return Ok(run.summary);
                        }
                    }
                }
            }
        }
    }
    Ok(run.summary)
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

    /// 文のせいに見える失敗（400 など）でも、指紋の試験文だけの呼び出しも失敗するなら、設定の誤り（モデル名・
    /// 次元など）やサービスの側の失敗なので、要約の失敗を記録せずに止める（全要約を断念させない）。
    #[tokio::test]
    async fn stops_when_even_the_fingerprint_fails() {
        let db = Db::open_in_memory().unwrap();
        let a = digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let b = digest(&db, "b", "2026-09-29T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        let cfg = small_batches();
        let fp = Fingerprint::make(&embedder, &cfg).await.unwrap();
        db.create_embedding_space(&space_name(&cfg), INPUT_VERSION, &fp, now())
            .unwrap();
        // どの文にも、文のせいに見える失敗を返す
        let broken = FakeEmbedder {
            bad: Some(String::new()),
            ..FakeEmbedder::default()
        };
        let err = run(&db, &broken, &cfg).await.unwrap_err();
        assert!(matches!(err, EmbedStageError::Api(_)), "{err:?}");
        assert_eq!(saved(&db, a), None);
        assert_eq!(saved(&db, b), None);
        assert_eq!(
            db.query_i64("SELECT count(*) FROM stage_errors").unwrap(),
            0
        );
        // 1 件だけの呼び出しでも同じ
        let single = EmbeddingConfig {
            batch_size: FINGERPRINT_TEXTS + 1,
            ..cfg
        };
        let err = run(&db, &broken, &single).await.unwrap_err();
        assert!(matches!(err, EmbedStageError::Api(_)), "{err:?}");
        assert_eq!(
            db.query_i64("SELECT count(*) FROM stage_errors").unwrap(),
            0
        );
    }

    /// 切り分けで返った指紋が空間と違えば（モデルが替わった）、要約の失敗にせず、作り直しを案内して止める。
    #[tokio::test]
    async fn the_probe_checks_the_fingerprint() {
        let db = Db::open_in_memory().unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let cfg = small_batches();
        let fp = Fingerprint::make(&FakeEmbedder::default(), &cfg)
            .await
            .unwrap();
        db.create_embedding_space(&space_name(&cfg), INPUT_VERSION, &fp, now())
            .unwrap();
        let changed = FakeEmbedder {
            model: std::sync::Mutex::new("other".into()),
            errors: std::sync::Mutex::new(
                [EmbedError::Status {
                    status: 400,
                    body: "bad".into(),
                }]
                .into(),
            ),
            ..FakeEmbedder::default()
        };
        let err = run(&db, &changed, &cfg).await.unwrap_err();
        assert!(matches!(err, EmbedStageError::SpaceChanged(_)), "{err:?}");
        assert_eq!(
            db.query_i64("SELECT count(*) FROM stage_errors").unwrap(),
            0
        );
    }

    /// 切り分けの呼び出しと、初回の指紋の作成も、中断が要求されれば待たずに終える（応答しないサーバーで
    /// タイムアウトまで止まらない）。中断で終えたときは、要約の失敗を記録しない。
    #[tokio::test]
    async fn api_calls_stop_on_cancel() {
        let limit = std::time::Duration::from_secs(5);
        let db = Db::open_in_memory().unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        digest(&db, "b", "2026-09-29T00:00:00.000Z");
        let cfg = small_batches();
        // 初回の指紋の作成（0 回目の呼び出し）が応答しない
        let cancel = Cancel::default();
        let hanging = FakeEmbedder {
            hang_from: Some(0),
            cancel_at: Some((0, cancel.clone())),
            ..FakeEmbedder::default()
        };
        let summary =
            tokio::time::timeout(limit, embed_articles(&db, &hanging, &cfg, &cancel, &now))
                .await
                .expect("fingerprinting stops on cancel")
                .unwrap();
        assert_eq!(summary.embedded, 0);
        assert_eq!(db.embedding_space().unwrap(), None);
        // まとめた呼び出しが文のせいに見える失敗で、その後の切り分け（1 回目）が応答しない
        let fp = Fingerprint::make(&FakeEmbedder::default(), &cfg)
            .await
            .unwrap();
        db.create_embedding_space(&space_name(&cfg), INPUT_VERSION, &fp, now())
            .unwrap();
        let cancel = Cancel::default();
        let hanging = FakeEmbedder {
            errors: std::sync::Mutex::new(
                [EmbedError::Status {
                    status: 400,
                    body: "bad".into(),
                }]
                .into(),
            ),
            hang_from: Some(1),
            cancel_at: Some((1, cancel.clone())),
            ..FakeEmbedder::default()
        };
        let summary =
            tokio::time::timeout(limit, embed_articles(&db, &hanging, &cfg, &cancel, &now))
                .await
                .expect("the probe stops on cancel")
                .unwrap();
        assert_eq!((summary.embedded, summary.failed), (0, 0));
        assert_eq!(
            db.query_i64("SELECT count(*) FROM stage_errors").unwrap(),
            0
        );
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
