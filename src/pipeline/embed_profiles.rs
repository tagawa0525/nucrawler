//! 好みの文の embedding を作り、利用者ごとに embedding の点数を付ける（計画 010）。`embed` ステージの中で、
//! 要約のベクトルを作った後に動く。
//!
//! 好みの文（関心分野ごとの「名前と note」、推薦しない話題）は、文そのものをキーにベクトルを持つ。点数は
//! `embed_score` の式で求め、採点した時点の直近の要約の中での百分位にして、`scores` 表に保存する（一覧などはこの点数で並べる。計画 017）。

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::Cancel;
use super::embed::{Checked, EmbedStageError, check_space, embed_checked};
use crate::config::EmbeddingConfig;
use crate::db::{
    CandidateFilter, Db, EMBED_BACKEND, EmbeddingScore, EmbeddingSpace, EvalKey, LabeledScore,
    Rating, ScoreKey, ScoringProfile, VersionStats, timestamp,
};
use crate::embed_score::{self, Formula, Preference, REFERENCE_LIMIT, SCORE_VERSION, Scorer};
use crate::embedding::{Embedder, FINGERPRINT_TEXTS, Role, input};
use crate::eval::{TRIAL_BACKEND, TRIAL_FORMULAS, embedding_trial};
use crate::profile::{Interest, Profile};

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
    fn new(cfg: &EmbeddingConfig, profile: &'a Profile) -> Self {
        Self {
            interests: profile
                .interests
                .iter()
                .filter(|i| i.weight > 0.0)
                .map(|i| (i, input(cfg, Role::Query, &interest_text(i))))
                .collect(),
            excludes: profile
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

/// `profile`（保存していない候補でもよい）の好みのベクトル。無い文はその場で作って保存する（文がキーなので、
/// 本番のプロファイルには影響しない）。中断されたか、`space` が消えていたら `None`。
async fn preference_of(
    db: &Db,
    embedder: &impl Embedder,
    cfg: &EmbeddingConfig,
    space: &EmbeddingSpace,
    profile: &Profile,
    cancel: &Cancel,
) -> Result<Option<Preference>, EmbedStageError> {
    let texts = ProfileTexts::new(cfg, profile);
    let all: Vec<String> = texts.all().cloned().collect();
    let mut summary = ProfileSummary::default();
    let mut vectors = db.text_embeddings(space.id, &all)?;
    if !embed_texts(
        db,
        embedder,
        cfg,
        space,
        &mut vectors,
        &all,
        cancel,
        &mut summary,
    )
    .await?
    {
        return Ok(None);
    }
    Ok(texts.preference(&vectors))
}

/// 所有者が評価した記事を、今のプロファイルでは `eval::TRIAL_FORMULAS` の式ごとに、候補のプロファイル
/// （`candidate`）では今の式で、その場で採点する（保存しない）。百分位の基準は、どれも今の時点の一覧の期間
/// （`list_days`）の要約。好みの文のベクトルが無ければ作る。空間がまだ無ければ何も無く、中断されたか、途中で
/// 空間が作り直されたら、そのプロファイルの分は無い。
pub async fn eval_trials(
    db: &Db,
    embedder: &impl Embedder,
    cfg: &EmbeddingConfig,
    list_days: u32,
    candidate: Option<&Profile>,
    cancel: &Cancel,
    now: DateTime<Utc>,
) -> Result<Vec<LabeledScore>, EmbedStageError> {
    // 空間はここで 1 回だけ読む。要約のベクトルも好みの文のベクトルもこの空間のものだけを使い、
    // 途中で作り直されたら好みの文を保存できずに `None` になるので、世代が混ざらない
    let Some(space) = db.embedding_space()? else {
        return Ok(Vec::new());
    };
    check_space(&space, cfg)?;
    let owner = db.owner_id()?;
    let labeled = db.eval_embedding_inputs(owner, space.id)?;
    let reference = reference_vectors(db, owner, space.id, list_days, now)?;
    let scored_at = timestamp(now);
    let key = |hash: &str, name: &str| EvalKey {
        profile_hash: hash.to_string(),
        backend: TRIAL_BACKEND.into(),
        model: format!("{} {name}", cfg.model),
        prompt_version: SCORE_VERSION,
    };
    let current = db.load_profile(owner)?;
    // 候補が今のプロファイルと同じなら、今のプロファイルの `now` と同じキーになるので並べない
    let candidate = candidate
        .map(|p| (p, crate::profile::hash(p)))
        .filter(|(_, hash)| current.as_ref().map(|(_, h)| h) != Some(hash));
    let mut trials = Vec::new();
    if let Some((current, hash)) = &current
        && scorable(owner, current)
        && let Some(preference) = preference_of(db, embedder, cfg, &space, current, cancel).await?
    {
        for (name, formula) in TRIAL_FORMULAS {
            let scorer = Scorer::new(&preference, formula, &reference);
            trials.extend(embedding_trial(
                &labeled,
                &scorer,
                &key(hash, name),
                &scored_at,
            ));
        }
    }
    if let Some((candidate, hash)) = candidate
        && let Some(preference) =
            preference_of(db, embedder, cfg, &space, candidate, cancel).await?
    {
        let (name, formula) = TRIAL_FORMULAS[0];
        let scorer = Scorer::new(&preference, formula, &reference);
        trials.extend(embedding_trial(
            &labeled,
            &scorer,
            &key(&hash, name),
            &scored_at,
        ));
    }
    Ok(trials)
}

/// 利用者が評価した記事を、今のプロファイル `current` と案 `candidate` の両方で、今の式でその場で採点し、
/// それぞれの一致率を返す（保存しない。計画 017 の見直しの比較）。百分位の基準は、どちらも今の時点の
/// 一覧の期間（`list_days`）の要約。好みの文のベクトルが無ければ作る。空間がまだ無いか、中断されたか、
/// 途中で空間が作り直されたら `None`。
#[expect(
    clippy::too_many_arguments,
    reason = "eval_trials と同じ材料に、利用者と 2 つのプロファイルを足す"
)]
pub async fn compare_profiles(
    db: &Db,
    embedder: &impl Embedder,
    cfg: &EmbeddingConfig,
    list_days: u32,
    user_id: i64,
    current: &Profile,
    candidate: &Profile,
    cancel: &Cancel,
    now: DateTime<Utc>,
) -> Result<Option<(VersionStats, VersionStats)>, EmbedStageError> {
    let Some(space) = db.embedding_space()? else {
        return Ok(None);
    };
    check_space(&space, cfg)?;
    let labeled = db.eval_embedding_inputs(user_id, space.id)?;
    let ratings: HashMap<i64, Rating> = db
        .eval_labels(user_id)?
        .into_iter()
        .map(|l| (l.article_id, l.rating))
        .collect();
    let reference = reference_vectors(db, user_id, space.id, list_days, now)?;
    let mut stats = Vec::with_capacity(2);
    for profile in [current, candidate] {
        let Some(preference) = preference_of(db, embedder, cfg, &space, profile, cancel).await?
        else {
            return Ok(None);
        };
        let scorer = Scorer::new(&preference, Formula::default(), &reference);
        let pairs: Vec<(u8, Rating)> = labeled
            .iter()
            .filter_map(|l| {
                let rating = *ratings.get(&l.article_id)?;
                Some((scorer.score(&l.vector).score, rating))
            })
            .collect();
        stats.push(VersionStats {
            rated: pairs.len(),
            concordance: crate::eval::concordance(&pairs),
        });
    }
    Ok(Some((stats[0], stats[1])))
}

/// 前の版で取り込んだプロファイルは、今の条件（件数・長さ・重みが正の関心分野）を満たさないことがある。
/// 取り込み直すまで embedding では採点しない。
fn scorable(user_id: i64, profile: &Profile) -> bool {
    match crate::profile::validate(profile) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                user_id,
                "not scoring with embeddings: {e}; import the profile again"
            );
            false
        }
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
    let profiles: Vec<ScoringProfile> = db
        .scoring_profiles()?
        .into_iter()
        .filter(|p| scorable(p.user_id, &p.profile))
        .collect();
    let texts: Vec<ProfileTexts> = profiles
        .iter()
        .map(|p| ProfileTexts::new(cfg, &p.profile))
        .collect();
    let mut seen = HashSet::new();
    let all: Vec<String> = texts
        .iter()
        .flat_map(ProfileTexts::all)
        .filter(|t| seen.insert(*t))
        .cloned()
        .collect();
    let mut vectors = db.text_embeddings(space.id, &all)?;
    let failure = match embed_texts(
        db,
        embedder,
        cfg,
        &space,
        &mut vectors,
        &all,
        cancel,
        &mut summary,
    )
    .await
    {
        Ok(true) => None,
        // 中断されたか、空間が消えた
        Ok(false) => return Ok(summary),
        Err(e) => Some(e),
    };
    db.prune_text_embeddings(space.id, &all)?;
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

/// `vectors`（DB から読んだ分）に無い文を作り、保存して `vectors` にも足す。作り終えれば `true`、中断されたか
/// 空間が消えたら `false`。呼び出しが失敗したら、残りは呼ばずに返す。読み直さずに `vectors` を使うので、
/// 重なって動くほかの処理（`embed` の後片付けや `eval --profile`）が文の行を消しても、この計算は影響を受けない。
#[allow(clippy::too_many_arguments)]
async fn embed_texts(
    db: &Db,
    embedder: &impl Embedder,
    cfg: &EmbeddingConfig,
    space: &EmbeddingSpace,
    vectors: &mut HashMap<String, Vec<f32>>,
    all: &[String],
    cancel: &Cancel,
    summary: &mut ProfileSummary,
) -> Result<bool, EmbedStageError> {
    let missing: Vec<String> = all
        .iter()
        .filter(|t| !vectors.contains_key(*t))
        .cloned()
        .collect();
    for chunk in missing.chunks(cfg.batch_size - FINGERPRINT_TEXTS) {
        let returned = match embed_checked(
            embedder,
            cfg,
            space,
            Role::Query,
            chunk.iter().cloned(),
            cancel,
            &mut summary.calls,
        )
        .await?
        {
            Checked::Vectors(vectors) => vectors,
            Checked::Failed(e) => return Err(EmbedStageError::Api(e)),
            Checked::Cancelled => return Ok(false),
        };
        let pairs: Vec<(String, Vec<f32>)> = chunk.iter().cloned().zip(returned).collect();
        if !db.save_text_embeddings(space.id, &pairs)? {
            tracing::warn!("the embedding space was rebuilt during this run; stopping");
            return Ok(false);
        }
        summary.embedded += chunk.len();
        vectors.extend(pairs);
    }
    Ok(true)
}

/// 百分位の基準にする要約のベクトル：利用者の採点の対象のうち、一覧の期間（`list_days`）の新しいものから
/// `REFERENCE_LIMIT` 件まで。
fn reference_vectors(
    db: &Db,
    user_id: i64,
    space_id: i64,
    list_days: u32,
    now: DateTime<Utc>,
) -> Result<Vec<Vec<f32>>, EmbedStageError> {
    let recent = CandidateFilter {
        since: Some(now - chrono::Duration::days(list_days.into())),
        unscored: None,
    };
    Ok(db
        .embedding_candidates(user_id, space_id, recent, 0, REFERENCE_LIMIT)?
        .into_iter()
        .map(|c| c.vector)
        .collect())
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
    let reference = reference_vectors(db, profile.user_id, space.id, list_days, now)?;
    let scorer = Scorer::new(preference, Formula::default(), &reference);
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
            let scored = scorer.score(&c.vector);
            scores.push(EmbeddingScore {
                artifact_id: c.artifact_id,
                score: scored.score,
                interest: scored.interest,
                exclude: scored.exclude,
            });
        }
        if page.len() < PAGE {
            break;
        }
    }
    // 点数が無くても保存を呼び、古いプロファイルの点数を消す
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
        ArtifactKind, ContentKind, ContentOrigin, EmbedInput, NewArticle, NewArtifact, Rating,
    };
    use crate::embedding::EmbedError;
    use crate::embedding::fake::{FakeEmbedder, cfg};
    use crate::embedding::fingerprint_inputs;
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
        // 類似度は基準の 3 件での平均と標準偏差で標準化する。heavy は「重い」、light は「軽い」で平均を上回り、
        // avoid はどの関心分野でも平均以下で、「避けたい」で平均を上回るので負
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
                format!("{avoid}:exclude:避けたい"),
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

    /// 候補のプロファイルの好みのベクトルを、本番のプロファイルを変えずに作る。
    #[tokio::test]
    async fn builds_a_preference_for_a_candidate_profile() {
        let db = Db::open_in_memory().unwrap();
        let candidate = Profile {
            interests: vec![interest("候補", 0.7, Some("補足"))],
            exclude: vec!["避けたい".into()],
        };
        let embedder = FakeEmbedder::default();
        let cfg = config();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        embed_articles(&db, &embedder, &cfg, &Cancel::default(), &now)
            .await
            .unwrap();
        let space = db.embedding_space().unwrap().unwrap();
        let preference =
            preference_of(&db, &embedder, &cfg, &space, &candidate, &Cancel::default())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(preference.interests.len(), 1);
        assert_eq!(preference.interests[0].topic, "候補");
        assert!((preference.interests[0].weight - 0.7).abs() < 1e-6);
        assert_eq!(
            preference.interests[0].vector,
            FakeEmbedder::vector("", &query("候補\n補足"))
        );
        assert_eq!(preference.excludes[0].topic, "避けたい");
        assert_eq!(db.profile_hash(db.owner_id().unwrap()).unwrap(), None);
    }

    /// 前の版で取り込んだ、今の条件を満たさないプロファイル（重みが正の関心分野が無いなど）では採点しない。
    #[tokio::test]
    async fn skips_profiles_that_are_no_longer_valid() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.save_profile(
            owner,
            &Profile {
                interests: vec![interest("なし", 0.0, None)],
                exclude: vec![],
            },
            now(),
        )
        .unwrap();
        digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let summary = run(&db, &FakeEmbedder::default()).await.unwrap();
        assert_eq!(summary.users, 0);
        assert!(scores(&db, owner).is_empty());
    }

    /// プロファイルを変えた後は、採点する要約が無くても、古いプロファイルの点数を消す。
    #[tokio::test]
    async fn drops_old_scores_even_without_candidates() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.save_profile(
            owner,
            &Profile {
                interests: vec![interest("一", 1.0, None)],
                exclude: vec![],
            },
            now(),
        )
        .unwrap();
        let (a, _) = digest(&db, "a", "2026-09-30T00:00:00.000Z");
        let embedder = FakeEmbedder::default();
        run(&db, &embedder).await.unwrap();
        assert_eq!(scores(&db, owner).len(), 1);
        // 要約のベクトルが無くなる（採点の対象が無い）状態で、プロファイルを変える
        db.conn()
            .execute("DELETE FROM article_embeddings WHERE artifact_id = ?1", [a])
            .unwrap();
        db.save_profile(
            owner,
            &Profile {
                interests: vec![interest("二", 1.0, None)],
                exclude: vec![],
            },
            now(),
        )
        .unwrap();
        embed_profiles(&db, &embedder, &config(), 7, &Cancel::default(), &now)
            .await
            .unwrap();
        assert!(scores(&db, owner).is_empty());
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

    /// 所有者が評価した、一覧の期間の要約を作る。記事の id と、要約の文書としての入力を返す。
    fn rated(db: &Db, title: &str, rating: u8) -> (i64, String) {
        let (artifact_id, text) = digest(db, title, "2026-09-30T00:00:00.000Z");
        let article_id = db
            .query_i64(&format!(
                "SELECT article_id FROM artifacts WHERE id = {artifact_id}"
            ))
            .unwrap();
        db.rate(
            db.owner_id().unwrap(),
            article_id,
            Rating::new(rating),
            now(),
        )
        .unwrap();
        (article_id, text)
    }

    async fn trials(
        db: &Db,
        embedder: &FakeEmbedder,
        candidate: Option<&Profile>,
    ) -> Vec<crate::db::LabeledScore> {
        eval_trials(
            db,
            embedder,
            &config(),
            7,
            candidate,
            &Cancel::default(),
            now(),
        )
        .await
        .unwrap()
    }

    /// 採点のキー（プロファイルと式の名前）ごとの、記事の点数。
    fn by_key(trials: &[crate::db::LabeledScore]) -> HashMap<(String, String), Vec<(i64, u8)>> {
        let mut got: HashMap<(String, String), Vec<(i64, u8)>> = HashMap::new();
        for t in trials {
            assert_eq!(
                (t.key.backend.as_str(), t.key.prompt_version),
                (TRIAL_BACKEND, SCORE_VERSION)
            );
            got.entry((t.key.profile_hash.clone(), t.key.model.clone()))
                .or_default()
                .push((t.article_id, t.score));
        }
        got
    }

    fn model(name: &str) -> String {
        format!("{} {name}", config().model)
    }

    /// 見直しの比較：評価した記事を、今のプロファイルと案の両方でその場で採点し、それぞれの一致率を返す。
    /// 点数は保存しない。
    #[tokio::test]
    async fn compares_two_profiles_on_rated_articles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let current = Profile {
            interests: vec![interest("関心", 1.0, None)],
            exclude: vec![],
        };
        db.save_profile(owner, &current, now()).unwrap();
        let candidate = Profile {
            interests: vec![interest("候補", 1.0, None)],
            exclude: vec![],
        };
        let (_, liked_text) = rated(&db, "liked", 5);
        let (_, disliked_text) = rated(&db, "disliked", 1);
        let embedder = FakeEmbedder::default();
        {
            let mut fixed = embedder.fixed.lock().unwrap();
            fixed.insert(query("関心"), at(0.0));
            fixed.insert(query("候補"), at(90.0));
            fixed.insert(liked_text, at(10.0));
            fixed.insert(disliked_text, at(80.0));
        }
        embed_articles(&db, &embedder, &config(), &Cancel::default(), &now)
            .await
            .unwrap();
        let (now_stats, candidate_stats) = compare_profiles(
            &db,
            &embedder,
            &config(),
            7,
            owner,
            &current,
            &candidate,
            &Cancel::default(),
            now(),
        )
        .await
        .unwrap()
        .unwrap();
        let stats = |c: f64| VersionStats {
            rated: 2,
            concordance: Some(c),
        };
        assert_eq!((now_stats, candidate_stats), (stats(1.0), stats(0.0)));
        assert_eq!(db.query_i64("SELECT count(*) FROM scores").unwrap(), 0);
    }

    /// 評価した記事を、今のプロファイルでは式の候補ごとに、候補のプロファイルでは今の式で採点する。
    /// 基準は一覧の期間の要約で、点数は保存しない。
    #[tokio::test]
    async fn eval_trials_score_the_current_and_candidate_profiles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let current = Profile {
            interests: vec![interest("関心", 1.0, None)],
            exclude: vec![],
        };
        db.save_profile(owner, &current, now()).unwrap();
        let candidate = Profile {
            interests: vec![interest("候補", 1.0, None)],
            exclude: vec![],
        };
        let (liked, liked_text) = rated(&db, "liked", 5);
        let (disliked, disliked_text) = rated(&db, "disliked", 1);
        let embedder = FakeEmbedder::default();
        {
            let mut fixed = embedder.fixed.lock().unwrap();
            fixed.insert(query("関心"), at(0.0));
            fixed.insert(query("候補"), at(90.0));
            fixed.insert(liked_text, at(10.0));
            fixed.insert(disliked_text, at(80.0));
        }
        embed_articles(&db, &embedder, &config(), &Cancel::default(), &now)
            .await
            .unwrap();
        let got = by_key(&trials(&db, &embedder, Some(&candidate)).await);
        let hash = crate::profile::hash(&current);
        let mut keys: Vec<&(String, String)> = got.keys().collect();
        keys.sort();
        let mut expected: Vec<(String, String)> = TRIAL_FORMULAS
            .iter()
            .map(|(name, _)| (hash.clone(), model(name)))
            .chain([(crate::profile::hash(&candidate), model("now"))])
            .collect();
        expected.sort();
        assert_eq!(keys, expected.iter().collect::<Vec<_>>());
        // 基準は 2 件：liked（cos 10°）は関心分野との類似度が平均を上回り、disliked（cos 80°）は下回るので 0 点。
        // 候補では逆になる
        assert_eq!(got[&(hash, model("now"))], [(liked, 75), (disliked, 0)]);
        assert_eq!(
            got[&(crate::profile::hash(&candidate), model("now"))],
            [(liked, 0), (disliked, 75)]
        );
        assert_eq!(db.query_i64("SELECT count(*) FROM scores").unwrap(), 0);
    }

    /// 候補が今のプロファイルと同じなら、今のプロファイルの `now` と同じキーになるので、候補の分は並べない。
    #[tokio::test]
    async fn eval_trials_skip_a_candidate_same_as_the_current_profile() {
        let db = Db::open_in_memory().unwrap();
        let current = Profile {
            interests: vec![interest("関心", 1.0, None)],
            exclude: vec![],
        };
        db.save_profile(db.owner_id().unwrap(), &current, now())
            .unwrap();
        rated(&db, "a", 5);
        let embedder = FakeEmbedder::default();
        embed_articles(&db, &embedder, &config(), &Cancel::default(), &now)
            .await
            .unwrap();
        // 評価した記事は 1 件なので、式ごとに 1 件ずつ
        let got = trials(&db, &embedder, Some(&current)).await;
        assert_eq!(got.len(), TRIAL_FORMULAS.len());
    }

    /// 前の版で取り込んだ、今の条件を満たさないプロファイルでは採点しない（embed ステージと同じ）。候補は採点する。
    #[tokio::test]
    async fn eval_trials_skip_a_current_profile_that_is_no_longer_valid() {
        let db = Db::open_in_memory().unwrap();
        db.save_profile(
            db.owner_id().unwrap(),
            &Profile {
                interests: vec![interest("なし", 0.0, None)],
                exclude: vec![],
            },
            now(),
        )
        .unwrap();
        let candidate = Profile {
            interests: vec![interest("候補", 1.0, None)],
            exclude: vec![],
        };
        rated(&db, "a", 5);
        let embedder = FakeEmbedder::default();
        embed_articles(&db, &embedder, &config(), &Cancel::default(), &now)
            .await
            .unwrap();
        let got = by_key(&trials(&db, &embedder, Some(&candidate)).await);
        let keys: Vec<&(String, String)> = got.keys().collect();
        assert_eq!(keys, [&(crate::profile::hash(&candidate), model("now"))]);
    }

    /// 空間がまだ無ければ（要約のベクトルをまだ作っていない）、何も採点せず、好みの文も作らない。
    #[tokio::test]
    async fn eval_trials_need_a_space() {
        let db = Db::open_in_memory().unwrap();
        db.save_profile(
            db.owner_id().unwrap(),
            &Profile {
                interests: vec![interest("関心", 1.0, None)],
                exclude: vec![],
            },
            now(),
        )
        .unwrap();
        rated(&db, "a", 5);
        let embedder = FakeEmbedder::default();
        assert!(trials(&db, &embedder, None).await.is_empty());
        assert!(embedder.calls().is_empty());
    }
}
