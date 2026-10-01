//! 好みの文の embedding を作り、利用者ごとに embedding の点数を付ける（計画 010）。`embed` ステージの中で、
//! 要約のベクトルを作った後に動く。
//!
//! 好みの文（関心分野ごとの「名前と note」、推薦しない話題）は、文そのものをキーにベクトルを持つ。点数は
//! `embed_score` の式で求め、採点した時点の直近の要約の中での百分位にして、LLM の採点と同じ `scores` 表に保存する。

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::Cancel;
use super::embed::{EmbedStageError, check_space};
use crate::config::EmbeddingConfig;
use crate::db::{
    CandidateFilter, Db, EMBED_BACKEND, EmbeddingScore, EmbeddingSpace, ScoreKey, ScoringProfile,
};
use crate::embed_score::{self, Formula, Preference, REFERENCE_LIMIT, SCORE_VERSION};
use crate::embedding::{Embedder, FINGERPRINT_TEXTS, Role, fingerprint_inputs, input};
use crate::profile::Interest;

/// 採点で一度に読む要約のベクトルの数（全期間を採点し直すときに、メモリに持つ量を抑える）。
const PAGE: usize = 1000;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProfileSummary {
    /// 新しく作った好みの文のベクトル
    pub embedded: usize,
    /// 採点した利用者
    pub users: usize,
    /// 付けた点数
    pub scored: usize,
    pub calls: usize,
}

/// 関心分野の文：名前と、あれば補足（note）を改行でつなぐ。
pub fn interest_text(interest: &Interest) -> String {
    match interest.note.as_deref().map(str::trim) {
        Some(note) if !note.is_empty() => format!("{}\n{note}", interest.topic),
        _ => interest.topic.clone(),
    }
}

/// プロファイルの好みの文（接頭辞を付けた入力）。重みが 0 の関心分野は点数に効かないので含めない。
struct ProfileTexts<'a> {
    interests: Vec<(&'a Interest, String)>,
    excludes: Vec<(&'a str, String)>,
}

impl<'a> ProfileTexts<'a> {
    fn new(cfg: &EmbeddingConfig, profile: &'a ScoringProfile) -> Self {
        Self {
            interests: profile
                .profile
                .interests
                .iter()
                .filter(|i| i.weight > 0.0)
                .map(|i| (i, input(cfg, Role::Query, &interest_text(i))))
                .collect(),
            excludes: profile
                .profile
                .exclude
                .iter()
                .map(|e| (e.as_str(), input(cfg, Role::Query, e)))
                .collect(),
        }
    }

    fn all(&self) -> impl Iterator<Item = &String> {
        self.interests
            .iter()
            .map(|(_, t)| t)
            .chain(self.excludes.iter().map(|(_, t)| t))
    }

    /// すべての文のベクトルがそろっていれば、好みのベクトル。
    fn preference(&self, vectors: &HashMap<String, Vec<f32>>) -> Option<Preference> {
        Some(Preference {
            interests: self
                .interests
                .iter()
                .map(|(i, t)| {
                    Some(embed_score::Interest {
                        topic: i.topic.clone(),
                        weight: i.weight as f32,
                        vector: vectors.get(t)?.clone(),
                    })
                })
                .collect::<Option<_>>()?,
            excludes: self
                .excludes
                .iter()
                .map(|(e, t)| {
                    Some(embed_score::Exclude {
                        topic: (*e).to_string(),
                        vector: vectors.get(t)?.clone(),
                    })
                })
                .collect::<Option<_>>()?,
        })
    }
}

/// 好みの文のベクトルを作り、プロファイルのある全員を採点する。好みの文の呼び出しが失敗したら、残りの文は
/// 呼ばず、ベクトルのそろっている利用者だけを採点してから、その失敗を返す。
pub async fn embed_profiles(
    db: &Db,
    embedder: &impl Embedder,
    cfg: &EmbeddingConfig,
    list_days: u32,
    cancel: &Cancel,
    clock: &dyn Fn() -> DateTime<Utc>,
) -> Result<ProfileSummary, EmbedStageError> {
    let mut summary = ProfileSummary::default();
    // 空間は要約のベクトルを作るときに作る
    let Some(space) = db.embedding_space()? else {
        return Ok(summary);
    };
    check_space(&space, cfg)?;
    let profiles = db.scoring_profiles()?;
    let texts: Vec<ProfileTexts> = profiles.iter().map(|p| ProfileTexts::new(cfg, p)).collect();
    let mut seen = HashSet::new();
    let all: Vec<String> = texts
        .iter()
        .flat_map(ProfileTexts::all)
        .filter(|t| seen.insert(*t))
        .cloned()
        .collect();
    let failure = match embed_texts(db, embedder, cfg, &space, &all, cancel, &mut summary).await {
        Ok(true) => None,
        // 中断されたか、空間が消えた
        Ok(false) => return Ok(summary),
        Err(e) => Some(e),
    };
    db.prune_text_embeddings(&all)?;
    let vectors = db.text_embeddings(space.id, &all)?;
    for (profile, texts) in profiles.iter().zip(&texts) {
        if cancel.is_requested() {
            break;
        }
        // ベクトルのそろわない利用者は、点数が付かない（次の実行でまた作る）
        if let Some(preference) = texts.preference(&vectors) {
            let scored = score_user(db, cfg, &space, profile, &preference, list_days, clock())?;
            summary.users += 1;
            summary.scored += scored;
        }
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(summary),
    }
}

/// ベクトルの無い文を作る。作り終えれば `true`、中断されたか空間が消えたら `false`。呼び出しが失敗したら、
/// 残りは呼ばずに返す。
async fn embed_texts(
    db: &Db,
    embedder: &impl Embedder,
    cfg: &EmbeddingConfig,
    space: &EmbeddingSpace,
    all: &[String],
    cancel: &Cancel,
    summary: &mut ProfileSummary,
) -> Result<bool, EmbedStageError> {
    let have = db.text_embeddings(space.id, all)?;
    let missing: Vec<&String> = all.iter().filter(|t| !have.contains_key(*t)).collect();
    for chunk in missing.chunks(cfg.batch_size - FINGERPRINT_TEXTS) {
        let mut inputs = fingerprint_inputs(cfg, Role::Query);
        inputs.extend(chunk.iter().map(|t| (*t).clone()));
        let result = tokio::select! {
            r = embedder.embed(&inputs) => r,
            () = cancel.requested() => return Ok(false),
        };
        summary.calls += 1;
        let vectors = result.map_err(EmbedStageError::Api)?;
        let (fingerprint, vectors) = vectors.split_at(FINGERPRINT_TEXTS);
        if !space.fingerprint.matches(Role::Query, fingerprint) {
            return Err(EmbedStageError::SpaceChanged(
                "the model behind the same settings returns different vectors".into(),
            ));
        }
        let pairs: Vec<(String, Vec<f32>)> = chunk
            .iter()
            .map(|t| (*t).clone())
            .zip(vectors.iter().cloned())
            .collect();
        if !db.save_text_embeddings(space.id, &pairs)? {
            tracing::warn!("the embedding space was rebuilt during this run; stopping");
            return Ok(false);
        }
        summary.embedded += chunk.len();
    }
    Ok(true)
}

/// 利用者の、今のプロファイルの点数がまだ無い要約を採点し、まとめて保存する。付けた点数の数を返す。
fn score_user(
    db: &Db,
    cfg: &EmbeddingConfig,
    space: &EmbeddingSpace,
    profile: &ScoringProfile,
    preference: &Preference,
    list_days: u32,
    now: DateTime<Utc>,
) -> Result<usize, EmbedStageError> {
    let key = ScoreKey {
        user_id: profile.user_id,
        profile_hash: &profile.hash,
        backend: EMBED_BACKEND,
        model: &cfg.model,
        prompt_version: SCORE_VERSION,
    };
    let formula = Formula::default();
    let recent = CandidateFilter {
        since: Some(now - chrono::Duration::days(list_days.into())),
        unscored: None,
    };
    let reference: Vec<f32> = db
        .embedding_candidates(profile.user_id, space.id, recent, 0, REFERENCE_LIMIT)?
        .iter()
        .map(|c| embed_score::raw(preference, &c.vector, formula).value)
        .collect();
    let unscored = CandidateFilter {
        since: None,
        unscored: Some(key),
    };
    let mut scores = Vec::new();
    // 保存はまとめて最後に行うので、まだ点数の無いものは読んでいる間に変わらない。ページは件数で進める
    loop {
        let page =
            db.embedding_candidates(profile.user_id, space.id, unscored, scores.len(), PAGE)?;
        for c in &page {
            let raw = embed_score::raw(preference, &c.vector, formula);
            scores.push(EmbeddingScore {
                artifact_id: c.artifact_id,
                score: embed_score::percentile(raw.value, &reference),
                interest: raw.interest.map(|i| preference.interests[i].topic.clone()),
                exclude: raw.exclude.map(|j| preference.excludes[j].topic.clone()),
            });
        }
        if page.len() < PAGE {
            break;
        }
    }
    if scores.is_empty() {
        return Ok(0);
    }
    if !db.save_embedding_scores(space.id, key, &scores, now)? {
        tracing::info!(
            user_id = profile.user_id,
            "the profile or the embedding space changed while scoring; scoring again next run"
        );
        return Ok(0);
    }
    Ok(scores.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Lang;
    use crate::db::{
        ArtifactKind, ContentKind, ContentOrigin, EmbedInput, NewArticle, NewArtifact,
    };
    use crate::embedding::EmbedError;
    use crate::embedding::fake::{FakeEmbedder, cfg};
    use crate::pipeline::embed::{document_text, embed_articles};
    use crate::profile::Profile;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn config() -> EmbeddingConfig {
        EmbeddingConfig {
            batch_size: 4,
            ..cfg("http://unused/e")
        }
    }

    /// 記事と要約を作り、要約の id と、その要約の文書としての入力を返す。
    fn digest(db: &Db, title: &str, published: &str) -> (i64, String) {
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
            "title_ja": title, "summary_ja": "要約", "points_ja": [],
            "implications_ja": "", "lwr_relevant": true, "topics": [],
        });
        let artifact_id = db
            .insert_artifact(
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
            .unwrap();
        let text = document_text(&EmbedInput {
            article_id: id,
            artifact_id,
            title_ja: title.into(),
            summary_ja: "要約".into(),
            points_ja: vec![],
        });
        (artifact_id, input(&config(), Role::Document, &text))
    }

    fn interest(topic: &str, weight: f64, note: Option<&str>) -> Interest {
        Interest {
            topic: topic.into(),
            weight,
            note: note.map(Into::into),
        }
    }

    /// 2 次元の単位ベクトル（角度 θ 度）を 16 次元に埋めたもの。
    fn at(degrees: f32) -> Vec<f32> {
        let r = degrees.to_radians();
        let mut v = vec![0.0; 16];
        v[0] = r.cos();
        v[1] = r.sin();
        v
    }

    fn query(text: &str) -> String {
        input(&config(), Role::Query, text)
    }

    async fn run(db: &Db, embedder: &FakeEmbedder) -> Result<ProfileSummary, EmbedStageError> {
        let cfg = config();
        embed_articles(db, embedder, &cfg, &Cancel::default(), &now)
            .await
            .unwrap();
        embed_profiles(db, embedder, &cfg, 7, &Cancel::default(), &now).await
    }

    /// 利用者の embedding の点数（要約の id 順）。
    fn scores(db: &Db, user_id: i64) -> Vec<(i64, i64)> {
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT artifact_id, score FROM scores
                 WHERE user_id = ?1 AND backend = 'embedding' ORDER BY artifact_id",
            )
            .unwrap();
        stmt.query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn joins_the_topic_and_note() {
        assert_eq!(
            interest_text(&interest("燃料", 1.0, Some("ATF"))),
            "燃料\nATF"
        );
        assert_eq!(interest_text(&interest("燃料", 1.0, None)), "燃料");
        assert_eq!(interest_text(&interest("燃料", 1.0, Some(" "))), "燃料");
    }

    /// 好みの文をクエリの接頭辞を付けて作る。同じ文は利用者をまたいで 1 回だけ作り、重みが 0 の関心分野は作らない。
    /// 呼び出しには毎回、クエリの指紋の試験文を先頭に入れる。使われなくなった文は消す。
    #[tokio::test]
    async fn embeds_profile_texts_once() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        let shared = Profile {
            interests: vec![
                interest("燃料", 1.0, Some("ATF")),
                interest("無関心", 0.0, None),
            ],
            exclude: vec!["核融合".into()],
        };
        db.save_profile(owner, &shared, now()).unwrap();
        db.save_profile(other, &shared, now()).unwrap();
        let embedder = FakeEmbedder::default();
        let summary = run(&db, &embedder).await.unwrap();
        assert_eq!(summary.embedded, 2);
        let calls = embedder.calls();
        let profile_calls: Vec<&Vec<String>> = calls
            .iter()
            .filter(|c| c.iter().any(|t| t.contains("燃料")))
            .collect();
        assert_eq!(profile_calls.len(), 1);
        let call = profile_calls[0];
        assert_eq!(
            call[..FINGERPRINT_TEXTS],
            fingerprint_inputs(&config(), Role::Query)[..]
        );
        assert_eq!(
            call[FINGERPRINT_TEXTS..],
            [query("燃料\nATF"), query("核融合")]
        );
        // 作ったものは作り直さない
        assert_eq!(run(&db, &embedder).await.unwrap().embedded, 0);
        // 使われなくなった文は消す
        let changed = Profile {
            interests: vec![interest("規制", 1.0, None)],
            exclude: vec![],
        };
        db.save_profile(owner, &changed, now()).unwrap();
        db.save_profile(other, &changed, now()).unwrap();
        run(&db, &embedder).await.unwrap();
        assert_eq!(
            db.query_strings("SELECT text FROM text_embeddings")
                .unwrap(),
            [query("規制")]
        );
    }

    /// 重い関心分野に近い記事が上、推薦しない話題に近い記事が下。点数は直近の記事の中での百分位で、
    /// 補正の特徴も保存する。
    #[tokio::test]
    async fn scores_users_by_their_profiles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.save_profile(
            owner,
            &Profile {
                interests: vec![interest("重い", 1.0, None), interest("軽い", 0.4, None)],
                exclude: vec!["避けたい".into()],
            },
            now(),
        )
        .unwrap();
        let (heavy, heavy_text) = digest(&db, "heavy", "2026-09-30T00:00:00.000Z");
        let (light, light_text) = digest(&db, "light", "2026-09-30T00:00:00.000Z");
        let (avoid, avoid_text) = digest(&db, "avoid", "2026-09-30T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        {
            let mut fixed = embedder.fixed.lock().unwrap();
            fixed.insert(query("重い"), at(0.0));
            fixed.insert(query("軽い"), at(90.0));
            fixed.insert(query("避けたい"), at(180.0));
            fixed.insert(heavy_text, at(10.0));
            fixed.insert(light_text, at(80.0));
            fixed.insert(avoid_text, at(170.0));
        }
        let summary = run(&db, &embedder).await.unwrap();
        assert_eq!((summary.users, summary.scored), (1, 3));
        let got: HashMap<i64, i64> = scores(&db, owner).into_iter().collect();
        // 生の値：heavy cos10° ≈ 0.98、light 0.4 × cos10° ≈ 0.39、avoid は減点が上回り負
        assert_eq!(got[&heavy], 83); // (2 + 0.5) / 3
        assert_eq!(got[&light], 50); // (1 + 0.5) / 3
        assert_eq!(got[&avoid], 0);
        assert_eq!(
            db.query_strings(
                "SELECT s.artifact_id || ':' || m.kind || ':' || m.topic FROM score_matches AS m
                 JOIN scores AS s ON s.id = m.score_id ORDER BY s.artifact_id, m.kind"
            )
            .unwrap(),
            [
                format!("{heavy}:interest:重い"),
                format!("{light}:interest:軽い"),
                // 関心も少しはあるが、減点が上回る
                format!("{avoid}:exclude:避けたい"),
                format!("{avoid}:interest:軽い"),
            ]
        );
        let (model, version): (String, i64) = db
            .conn()
            .query_row(
                "SELECT DISTINCT model, prompt_version FROM scores",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (model.as_str(), version),
            (config().model.as_str(), SCORE_VERSION)
        );
    }

    /// 百分位の基準は一覧の期間の記事。期間より古い記事も採点し、その基準で点数にする。
    #[tokio::test]
    async fn scores_old_articles_against_recent_ones() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.save_profile(
            owner,
            &Profile {
                interests: vec![interest("関心", 1.0, None)],
                exclude: vec![],
            },
            now(),
        )
        .unwrap();
        let (recent, recent_text) = digest(&db, "recent", "2026-09-30T00:00:00.000Z");
        let (old, old_text) = digest(&db, "old", "2020-01-01T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        {
            let mut fixed = embedder.fixed.lock().unwrap();
            fixed.insert(query("関心"), at(0.0));
            fixed.insert(recent_text, at(60.0));
            fixed.insert(old_text, at(0.0));
        }
        run(&db, &embedder).await.unwrap();
        let got: HashMap<i64, i64> = scores(&db, owner).into_iter().collect();
        // 基準は recent だけ。old は基準のどれより近いので 100 点、recent は中央で 50 点
        assert_eq!((got[&old], got[&recent]), (100, 50));
    }

    /// プロファイルを変えれば、新しいプロファイルで全部を採点し直し、古いプロファイルの点数は消える。
    #[tokio::test]
    async fn rescores_after_a_profile_change() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let first = Profile {
            interests: vec![interest("一", 1.0, None)],
            exclude: vec![],
        };
        db.save_profile(owner, &first, now()).unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        digest(&db, "b", "2020-01-01T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        run(&db, &embedder).await.unwrap();
        assert_eq!(scores(&db, owner).len(), 2);
        assert_eq!(run(&db, &embedder).await.unwrap().scored, 0);
        let second = Profile {
            interests: vec![interest("二", 1.0, None)],
            exclude: vec![],
        };
        db.save_profile(owner, &second, now()).unwrap();
        assert_eq!(run(&db, &embedder).await.unwrap().scored, 2);
        let hashes = db
            .query_strings("SELECT DISTINCT profile_hash FROM scores")
            .unwrap();
        assert_eq!(hashes, [crate::profile::hash(&second)]);
    }

    /// 好みの文の呼び出しが失敗すれば、残りの文は呼ばず、ベクトルのそろった利用者だけを採点して、失敗を返す。
    #[tokio::test]
    async fn scores_users_with_vectors_when_a_call_fails() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        let ready = Profile {
            interests: vec![interest("済み", 1.0, None)],
            exclude: vec![],
        };
        db.save_profile(owner, &ready, now()).unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        run(&db, &embedder).await.unwrap();
        // 別の利用者の文は 2 回に分かれる（1 回に 2 件まで）。最初の呼び出しが失敗する
        db.save_profile(
            other,
            &Profile {
                interests: vec![
                    interest("x1", 1.0, None),
                    interest("x2", 1.0, None),
                    interest("x3", 1.0, None),
                ],
                exclude: vec![],
            },
            now(),
        )
        .unwrap();
        // 次に要約を足し、両方とも採点を待つ状態にする
        digest(&db, "b", "2026-09-30T00:00:00.000Z");
        embed_articles(&db, &embedder, &config(), &Cancel::default(), &now)
            .await
            .unwrap();
        let calls = embedder.calls().len();
        embedder
            .errors
            .lock()
            .unwrap()
            .push_back(EmbedError::Status {
                status: 503,
                body: "down".into(),
            });
        let err = embed_profiles(&db, &embedder, &config(), 7, &Cancel::default(), &now)
            .await
            .unwrap_err();
        assert!(matches!(err, EmbedStageError::Api(_)), "{err:?}");
        assert_eq!(
            embedder.calls().len(),
            calls + 1,
            "stops after the first failure"
        );
        assert_eq!(scores(&db, owner).len(), 2);
        assert!(scores(&db, other).is_empty());
    }

    /// 空間がまだ無ければ（要約のベクトルをまだ作っていない）、何もしない。
    #[tokio::test]
    async fn does_nothing_without_a_space() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.save_profile(
            owner,
            &Profile {
                interests: vec![interest("a", 1.0, None)],
                exclude: vec![],
            },
            now(),
        )
        .unwrap();
        let embedder = FakeEmbedder::default();
        let summary = embed_profiles(&db, &embedder, &config(), 7, &Cancel::default(), &now)
            .await
            .unwrap();
        assert_eq!(summary, ProfileSummary::default());
        assert!(embedder.calls().is_empty());
    }
}
