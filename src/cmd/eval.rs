//! `eval`：採点が利用者の反応とどれだけ合っているかを表示する。`--profile` なら、候補の
//! プロファイルでラベルの付いた記事を採点してから、現行と並べる。

use std::path::PathBuf;

use nucrawler::cli::EvalArgs;
use nucrawler::config;
use nucrawler::db::{self, CandidateFilter, Db, EvalKey, LabeledScore};
use nucrawler::embed_score;
use nucrawler::embedding::Client;
use nucrawler::eval;
use nucrawler::llm::Backends;
use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::embed_profiles::preference_of;
use nucrawler::pipeline::lock::{self, LockKind};
use nucrawler::pipeline::run::{self, RunEnv};
use nucrawler::profile;
use nucrawler::prompt;
use nucrawler::quota::Quota;

use crate::{Error, config_dir, data_dir};

use super::{finish, spawn_signal_handler};

pub(crate) async fn eval(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    args: EvalArgs,
) -> Result<(), Error> {
    // 候補のファイルと設定の誤りは、ロックを取る前に知らせる
    let candidate = args.profile.as_deref().map(read_profile).transpose()?;
    let (config, _) = config::load(&config_dir(config)?)?;
    let data = data_dir(data)?;
    // 候補で採点するときは LLM を呼んで DB に書くので、redo と同じく DB を開く前にロックを取る
    let _lock = candidate
        .is_some()
        .then(|| lock::acquire(&data, LockKind::Llm))
        .transpose()?;
    let db = Db::open(&data.join("nucrawler.db"))?;
    let owner = db.owner_id()?;
    // 候補の採点と embedding の計算で、中断の要求を 1 つに共有する
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    if let Some(candidate) = &candidate {
        score_candidate(&config, &data, &db, candidate, args.max_llm_calls, &cancel).await?;
    }
    let mut scores = db.eval_scores(owner)?;
    if let Some(cfg) = &config.embedding {
        scores
            .extend(embedding_trials(&config, cfg, &db, owner, candidate.as_ref(), &cancel).await?);
    }
    // 中断されたら、途中までの結果を出さずに、候補の採点の中断と同じく中断として終える
    if cancel.is_requested() {
        return Err(Error::Interrupted);
    }
    let candidate = candidate.as_ref().map(profile::hash);
    let current = db.profile_hash(owner)?;
    print!(
        "{}",
        eval::render(
            &db.eval_labels(owner)?,
            &scores,
            current.as_deref(),
            candidate.as_deref(),
            prompt::score::PROMPT_VERSION,
            args.all,
            config.recommend.prior_strength,
        )
    );
    print!("{}", eval::render_explore(db.explore_stats(owner)?));
    Ok(())
}

/// 評価した記事を、今のプロファイルでは式の候補ごとに、候補のプロファイルでは今の式で、その場で採点する
/// （保存しない）。百分位の基準は、どれも今の時点の一覧の期間の要約。好みの文のベクトルが無ければ作る。
async fn embedding_trials(
    config: &config::Config,
    cfg: &config::EmbeddingConfig,
    db: &Db,
    owner: i64,
    candidate: Option<&profile::Profile>,
    cancel: &Cancel,
) -> Result<Vec<LabeledScore>, Error> {
    let Some(space) = db.embedding_space()? else {
        return Ok(Vec::new());
    };
    let client = Client::from_config(cfg, |name| std::env::var(name).ok())?;
    let now = chrono::Utc::now();
    let labeled = db.eval_embedding_inputs(owner, space.id)?;
    let recent = CandidateFilter {
        since: Some(now - chrono::Duration::days(config.web.list_days.into())),
        unscored: None,
    };
    let reference: Vec<Vec<f32>> = db
        .embedding_candidates(owner, space.id, recent, 0, embed_score::REFERENCE_LIMIT)?
        .into_iter()
        .map(|c| c.vector)
        .collect();
    let scored_at = db::timestamp(now);
    let key = |hash: &str, name: &str| EvalKey {
        profile_hash: hash.to_string(),
        backend: eval::TRIAL_BACKEND.into(),
        model: format!("{} {name}", cfg.model),
        prompt_version: embed_score::SCORE_VERSION,
    };
    let mut trials = Vec::new();
    // 前の版で取り込んだプロファイルは、今の条件を満たさないことがある（embed ステージも採点しない）
    let current = db.load_profile(owner)?.filter(|(current, _)| {
        profile::validate(current)
            .inspect_err(|e| {
                tracing::warn!("no embedding trials for the current profile: {e}; import it again")
            })
            .is_ok()
    });
    if let Some((current, hash)) = current
        && let Some(preference) = preference_of(db, &client, cfg, &current, cancel).await?
    {
        for (name, formula) in eval::TRIAL_FORMULAS {
            let scorer = embed_score::Scorer::new(&preference, formula, &reference);
            trials.extend(eval::embedding_trial(
                &labeled,
                &scorer,
                &key(&hash, name),
                &scored_at,
            ));
        }
    }
    // 候補が今のプロファイルと同じなら、今のプロファイルの `now` と同じキーになるので並べない
    if let Some(candidate) = candidate
        && db.profile_hash(owner)?.as_deref() != Some(profile::hash(candidate).as_str())
        && let Some(preference) = preference_of(db, &client, cfg, candidate, cancel).await?
    {
        let (name, formula) = eval::TRIAL_FORMULAS[0];
        let scorer = embed_score::Scorer::new(&preference, formula, &reference);
        trials.extend(eval::embedding_trial(
            &labeled,
            &scorer,
            &key(&profile::hash(candidate), name),
            &scored_at,
        ));
    }
    // 途中で `embed rebuild` されると、要約のベクトル（始めの世代）と好みのベクトル（新しい世代）が混ざる
    if db.embedding_space()?.map(|s| s.id) != Some(space.id) {
        tracing::warn!("the embedding space was rebuilt during eval; showing no embedding trials");
        return Ok(Vec::new());
    }
    Ok(trials)
}

fn read_profile(file: &std::path::Path) -> Result<profile::Profile, Error> {
    let text = std::fs::read_to_string(file).map_err(|source| Error::ReadFile {
        path: file.to_path_buf(),
        source,
    })?;
    Ok(profile::parse(&text)?)
}

/// ラベルの付いた記事を候補のプロファイルで採点する。クォータ・シグナルは `redo` と同じ。
/// ロックは呼び出し側が取る。
async fn score_candidate(
    config: &config::Config,
    data: &std::path::Path,
    db: &Db,
    candidate: &profile::Profile,
    max_llm_calls: Option<u32>,
    cancel: &Cancel,
) -> Result<(), Error> {
    let llm = Backends::from_config(&config.llm, data);
    let mut quota = Quota::from_config(config, max_llm_calls);
    let articles: Vec<i64> = db
        .eval_labels(db.owner_id()?)?
        .iter()
        .map(|l| l.article_id)
        .collect();
    let report = run::eval_profile(
        RunEnv {
            db,
            llm: &llm,
            quota: &mut quota,
            cancel,
            clock: &chrono::Utc::now,
        },
        config,
        candidate,
        &articles,
    )
    .await?;
    finish(report)
}
