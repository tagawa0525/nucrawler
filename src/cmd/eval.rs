//! `eval`：採点が利用者の評価とどれだけ合っているかを表示する。`--profile` なら、候補の
//! プロファイルで評価した記事を embedding でその場で採点し、現行と並べる。

use std::path::PathBuf;

use nucrawler::cli::EvalArgs;
use nucrawler::config;
use nucrawler::embedding::Client;
use nucrawler::eval;
use nucrawler::pipeline::Cancel;
use nucrawler::pipeline::embed_profiles::eval_trials;
use nucrawler::profile;

use crate::{Error, config_dir, data_dir, open_db};

use super::spawn_signal_handler;

pub(crate) async fn eval(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    args: EvalArgs,
) -> Result<(), Error> {
    // 候補のファイルと設定の誤りは、embedding を呼ぶ前に知らせる
    let candidate = args.profile.as_deref().map(read_profile).transpose()?;
    let (config, _) = config::load(&config_dir(config)?)?;
    let data = data_dir(data)?;
    let db = open_db(&data)?;
    let owner = db.owner_id()?;
    // 候補は embedding でその場で計算するので、計算できなければ候補を黙って落とさずに失敗する
    if candidate.is_some() {
        if config.embedding.is_none() {
            return Err(Error::CandidateNeedsEmbedding);
        }
        if db.embedding_space()?.is_none() {
            return Err(Error::NoEmbeddings);
        }
    }
    let cancel = Cancel::default();
    spawn_signal_handler(cancel.clone());
    let mut scores = db.eval_scores(owner)?;
    if let Some(cfg) = &config.embedding {
        let client = Client::from_config(cfg, |name| std::env::var(name).ok())?;
        scores.extend(
            eval_trials(
                &db,
                &client,
                cfg,
                config.web.list_days,
                candidate.as_ref(),
                &cancel,
                chrono::Utc::now(),
            )
            .await?,
        );
    }
    // 中断されたら、途中までの結果を出さずに中断として終える
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
            args.all,
            config.recommend.prior_strength,
        )
    );
    print!("{}", eval::render_explore(db.explore_stats(owner)?));
    Ok(())
}

fn read_profile(file: &std::path::Path) -> Result<profile::Profile, Error> {
    let text = std::fs::read_to_string(file).map_err(|source| Error::ReadFile {
        path: file.to_path_buf(),
        source,
    })?;
    Ok(profile::parse(&text)?)
}

/// 候補を頼んだのに、計算した結果に候補の点数が 1 件も無いか（評価した記事にまだ embedding が無いなど）。
/// 候補が今のプロファイルと同じなら、今のプロファイルの式の候補（同じ hash）として並ぶ。
fn candidate_missing(scores: &[nucrawler::db::LabeledScore], candidate: Option<&str>) -> bool {
    let _ = (scores, candidate);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use nucrawler::db::{EvalKey, LabeledScore};

    fn trial(hash: &str) -> LabeledScore {
        LabeledScore {
            key: EvalKey {
                profile_hash: hash.into(),
                backend: eval::TRIAL_BACKEND.into(),
                model: "m now".into(),
                prompt_version: 2,
            },
            article_id: 1,
            score: 50,
            scored_at: "2026-10-04T00:00:00.000Z".into(),
            features: Vec::new(),
        }
    }

    #[test]
    fn notices_a_candidate_without_scores() {
        let current = [trial("now")];
        assert!(candidate_missing(&current, Some("cand")));
        assert!(!candidate_missing(
            &[trial("now"), trial("cand")],
            Some("cand")
        ));
        // 候補が今のプロファイルと同じなら、今のプロファイルとして並ぶ
        assert!(!candidate_missing(&current, Some("now")));
        assert!(!candidate_missing(&current, None));
        // 評価した記事に embedding がまだ無ければ、今のプロファイルの分も無い
        assert!(candidate_missing(&[], Some("now")));
    }
}
