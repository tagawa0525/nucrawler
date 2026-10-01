//! `eval`：評価（1〜5）を正解ラベルにして、採点のキーごとに点数が評価の順にどれだけ並んでいるかを表示する。

use std::fmt::Write as _;

use crate::db::{EvalKey, ExploreStats, Label, LabeledScore, Rating};
use crate::recommend::{Example, leave_one_out};

/// 関心（評価 4〜5）・不要（評価 1〜2）のどちらかがこれより少なければ、指標は参考値と注記する
const FEW_LABELS: usize = 5;

/// 一致率：評価の違う記事の組のうち、評価の高い方の点数が高い割合（同点は半分と数える）。
/// 評価が 2 通りだけなら AUC と同じ。評価の違う組が無ければ `None`。
pub fn concordance(pairs: &[(u8, Rating)]) -> Option<f64> {
    // 同点を半分と数えるため、2 倍で数える
    let (mut twice, mut total) = (0usize, 0usize);
    for (i, (score_a, rating_a)) in pairs.iter().enumerate() {
        for (score_b, rating_b) in &pairs[i + 1..] {
            if rating_a == rating_b {
                continue;
            }
            // 評価の高い方の点数から見た順
            let (high, low) = if rating_a > rating_b {
                (score_a, score_b)
            } else {
                (score_b, score_a)
            };
            twice += match high.cmp(low) {
                std::cmp::Ordering::Greater => 2,
                std::cmp::Ordering::Equal => 1,
                std::cmp::Ordering::Less => 0,
            };
            total += 1;
        }
    }
    (total > 0).then(|| twice as f64 / (2 * total) as f64)
}

/// 10 点刻みの点数帯の下限。100 点は 90 帯に入れる。
pub fn band(score: u8) -> u8 {
    (score.min(99) / 10) * 10
}

/// 評価の表示。`current` は現行のプロファイルの hash、`candidate` は候補のプロファイルの hash、
/// `version` は今の score のプロンプトの版。`all` が偽なら、今の版の現行と候補のキーだけを出す。
pub fn render(
    labels: &[Label],
    scores: &[LabeledScore],
    current: Option<&str>,
    candidate: Option<&str>,
    version: i64,
    all: bool,
    prior_strength: f64,
) -> String {
    let current = current.map(|hash| (hash, version));
    let mut out = String::new();
    let per_rating: Vec<String> = Rating::all()
        .rev()
        .map(|r| {
            let n = labels.iter().filter(|l| l.rating == r).count();
            format!("★{} {n}", r.get())
        })
        .collect();
    let _ = writeln!(
        out,
        "labels: {} rated ({})",
        labels.len(),
        per_rating.join(", ")
    );
    let positive = labels.iter().filter(|l| l.rating.is_positive()).count();
    let negative = labels.iter().filter(|l| l.rating.is_negative()).count();
    if positive < FEW_LABELS || negative < FEW_LABELS {
        let _ = writeln!(
            out,
            "note: fewer than {FEW_LABELS} ratings of 4-5 or of 1-2; treat the numbers as rough"
        );
    }
    let of_current = |k: &EvalKey| {
        current.is_some_and(|(hash, version)| {
            k.profile_hash == hash && k.prompt_version == current_version(k, version)
        })
    };
    // その場で計算した式の候補は、現行のプロファイルのものを現行と分けて並べる
    let is_trial = |k: &EvalKey| k.backend == TRIAL_BACKEND && of_current(k);
    let is_current = |k: &EvalKey| k.backend != TRIAL_BACKEND && of_current(k);
    // 候補は今の版のプロンプトで採点する。現行のプロファイルが無くても判定できるようにする
    let is_candidate = |k: &EvalKey| {
        k.prompt_version == current_version(k, version)
            && candidate.is_some_and(|hash| k.profile_hash == hash)
            && !is_current(k)
            && !is_trial(k)
    };
    let mut keys: Vec<&EvalKey> = scores.iter().map(|s| &s.key).collect();
    keys.sort_by_key(|k| (!is_current(k), !is_candidate(k), !is_trial(k), *k));
    keys.dedup();
    if !all {
        keys.retain(|k| is_current(k) || is_candidate(k) || is_trial(k));
        // 候補だけ採点済みでも、比べる相手が無いことを示す
        if !keys.iter().any(|k| is_current(k)) {
            out.push('\n');
            if current.is_none() {
                let _ = writeln!(
                    out,
                    "no profile; import one with `nucrawler profile import FILE`"
                );
            } else {
                let _ = writeln!(
                    out,
                    "no scores for the current profile and prompt version (see --all)"
                );
            }
        }
    }
    for key in keys {
        out.push('\n');
        let role = if is_current(key) {
            "  (current)"
        } else if is_candidate(key) {
            "  (candidate)"
        } else if is_trial(key) {
            "  (trial)"
        } else {
            ""
        };
        render_key(&mut out, key, role, labels, scores, prior_strength);
    }
    out
}

/// `eval` がその場で計算する embedding の点数（式やプロファイルの候補）のバックエンド。保存はしない。
pub const TRIAL_BACKEND: &str = "embedding-trial";

/// `eval` で比べる式（名前と式）。最初は今の式を同じ時点・同じ基準で計算し直したもので、ほかの候補と比べる基準にする
/// （保存した点数は採点した時点の基準で固まっているので、そのままでは候補と比べられない）。
pub const TRIAL_FORMULAS: [(&str, crate::embed_score::Formula); 5] = {
    use crate::embed_score::{Aggregate, Formula, LAMBDA};
    let current = Formula {
        lambda: LAMBDA,
        aggregate: Aggregate::WeightedMax,
    };
    [
        ("now", current),
        (
            "λ=0.5",
            Formula {
                lambda: 0.5,
                ..current
            },
        ),
        (
            "λ=0",
            Formula {
                lambda: 0.0,
                ..current
            },
        ),
        (
            "mean",
            Formula {
                aggregate: Aggregate::WeightedMean,
                ..current
            },
        ),
        (
            "top3",
            Formula {
                aggregate: Aggregate::TopK(3),
                ..current
            },
        ),
    ]
};

/// 評価した記事を、`preference` と `formula` で採点した点数（`key` のキーで）。百分位の基準は `reference`
/// （直近の要約のベクトル）を同じ式で計算した値。保存した点数（採点した時点の基準で固まっている）とは別に、
/// 式どうしを同じ時点・同じ基準で比べるために使う。
pub fn embedding_trial(
    labeled: &[crate::db::LabeledVector],
    reference: &[Vec<f32>],
    preference: &crate::embed_score::Preference,
    formula: crate::embed_score::Formula,
    key: &EvalKey,
    scored_at: &str,
) -> Vec<LabeledScore> {
    use crate::embed_score::{percentile, raw};
    let reference: Vec<f32> = reference
        .iter()
        .map(|v| raw(preference, v, formula).value)
        .collect();
    labeled
        .iter()
        .map(|l| {
            let r = raw(preference, &l.vector, formula);
            let matched: Vec<String> = r
                .interest
                .map(|i| preference.interests[i].topic.clone())
                .into_iter()
                .collect();
            let excluded: Vec<String> = r
                .exclude
                .map(|j| preference.excludes[j].topic.clone())
                .into_iter()
                .collect();
            LabeledScore {
                key: key.clone(),
                article_id: l.article_id,
                score: percentile(r.value, &reference),
                scored_at: scored_at.to_string(),
                features: crate::recommend::features(&l.source_id, &l.topics, &matched, &excluded),
            }
        })
        .collect()
}

/// キーの採点器の今の版：embedding なら式の版、LLM ならプロンプトの版（`llm_version`）。
fn current_version(key: &EvalKey, llm_version: i64) -> i64 {
    if key.backend == crate::db::EMBED_BACKEND || key.backend == TRIAL_BACKEND {
        crate::embed_score::SCORE_VERSION
    } else {
        llm_version
    }
}

/// 確認枠の評価の内訳。評価した記事のうち関心（評価 4〜5）の割合を、閾値未満での見逃し率の見積もりとして示す。
pub fn render_explore(stats: ExploreStats) -> String {
    if stats.picked == 0 {
        return String::new();
    }
    let rated = stats.positive + stats.neutral + stats.negative;
    let mut out = format!(
        "\nexplore: {} picked below the threshold, {rated} rated ({} of interest, {} neutral, {} not)\n",
        stats.picked, stats.positive, stats.neutral, stats.negative
    );
    if rated > 0 {
        let _ = writeln!(
            out,
            "  about {:.0}% of the rated picks were of interest (misses below the threshold)",
            stats.positive as f64 * 100.0 / rated as f64
        );
    }
    out
}

/// 1 つのキーの結果（`role` は現行・候補の印）：カバー率、一致率、評価より後に採点した件数、点数帯ごとの評価の件数。
fn render_key(
    out: &mut String,
    key: &EvalKey,
    role: &str,
    labels: &[Label],
    scores: &[LabeledScore],
    prior_strength: f64,
) {
    let hash: String = key.profile_hash.chars().take(8).collect();
    let _ = writeln!(
        out,
        "profile {hash}  {}/{}  prompt v{}{role}",
        key.backend, key.model, key.prompt_version,
    );
    // ラベルと突き合わせた (点数, 評価, 評価より後に採点したか)
    let labeled: Vec<(&LabeledScore, Rating, bool)> = scores
        .iter()
        .filter(|s| &s.key == key)
        .filter_map(|s| {
            let label = labels.iter().find(|l| l.article_id == s.article_id)?;
            Some((s, label.rating, s.scored_at > label.at))
        })
        .collect();
    let matched: Vec<(u8, Rating, bool)> = labeled
        .iter()
        .map(|(s, rating, late)| (s.score, *rating, *late))
        .collect();
    let format = |c: Option<f64>| c.map_or_else(|| "-".to_string(), |c| format!("{c:.2}"));
    let pairs: Vec<(u8, Rating)> = matched.iter().map(|m| (m.0, m.1)).collect();
    // 推薦点（評価から学んだ補正を足した点数）は、1 件ずつ外して学習した予測で測る
    let examples: Vec<Example> = labeled
        .iter()
        .map(|(s, rating, _)| Example {
            llm_score: s.score,
            features: s.features.clone(),
            rating: *rating,
        })
        .collect();
    let adjusted: Vec<(u8, Rating)> = leave_one_out(&examples, prior_strength)
        .into_iter()
        .zip(examples.iter().map(|e| e.rating))
        .collect();
    let _ = writeln!(
        out,
        "  scored {}/{}  concordance {}  adjusted {} (leave-one-out)",
        matched.len(),
        labels.len(),
        format(concordance(&pairs)),
        format(concordance(&adjusted)),
    );
    let late = matched.iter().filter(|m| m.2).count();
    // embedding の入力に反応は入らない
    let embedding = key.backend == crate::db::EMBED_BACKEND || key.backend == TRIAL_BACKEND;
    if !embedding && key.prompt_version == 1 && late > 0 {
        // 版 1 の採点のプロンプトは直近の反応の見出しを含むので、反応の後の採点は甘くなりうる
        let _ = writeln!(
            out,
            "  note: {late} scored after the reaction; the article's own title may have been a signal, so concordance may be high"
        );
    }
    let mut header = format!("  {:<8}", "score");
    for r in Rating::all() {
        let _ = write!(header, "{:>4}", format!("★{}", r.get()));
    }
    let _ = writeln!(out, "{header}");
    for b in (0..10).rev().map(|i| i * 10) {
        let in_band: Vec<&(u8, Rating, bool)> = matched.iter().filter(|m| band(m.0) == b).collect();
        if in_band.is_empty() {
            continue;
        }
        let label = if b == 90 {
            "90-100".to_string()
        } else {
            format!("{b}-{}", b + 9)
        };
        let mut row = format!("  {label:<8}");
        for r in Rating::all() {
            let _ = write!(row, "{:>4}", in_band.iter().filter(|m| m.1 == r).count());
        }
        let _ = writeln!(out, "{row}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommend::{Feature, FeatureKind};

    fn pair(score: u8, rating: u8) -> (u8, Rating) {
        (score, Rating::new(rating).unwrap())
    }

    /// 評価の違う組のうち、評価の高い方の点数が高い割合（同点は半分）。2 値なら AUC と同じ。
    #[test]
    fn concordance_is_the_share_of_correctly_ordered_pairs() {
        assert_eq!(concordance(&[pair(90, 5), pair(10, 1)]), Some(1.0));
        assert_eq!(concordance(&[pair(10, 5), pair(90, 1)]), Some(0.0));
        assert_eq!(concordance(&[pair(50, 4), pair(50, 2)]), Some(0.5));
        // 組は (5,3) 正、(5,1) 正、(3,1) 誤。同じ評価どうし（4 と 4）は数えない
        let three = [pair(90, 5), pair(10, 3), pair(50, 1)];
        assert_eq!(concordance(&three), Some(2.0 / 3.0));
        assert_eq!(
            concordance(&[pair(90, 5), pair(10, 3), pair(50, 1), pair(20, 3)]),
            Some(0.6)
        );
        assert_eq!(concordance(&[pair(90, 4), pair(10, 4)]), None);
        assert_eq!(concordance(&[]), None);
    }

    #[test]
    fn bands_are_tens_with_100_in_the_top_band() {
        assert_eq!(band(0), 0);
        assert_eq!(band(49), 40);
        assert_eq!(band(50), 50);
        assert_eq!(band(90), 90);
        assert_eq!(band(100), 90);
    }

    fn label(article_id: i64, rating: u8) -> Label {
        Label {
            article_id,
            rating: Rating::new(rating).unwrap(),
            at: "2026-09-27T00:00:00.000Z".into(),
        }
    }

    fn key(profile_hash: &str, prompt_version: i64) -> EvalKey {
        EvalKey {
            profile_hash: profile_hash.into(),
            backend: "claude-cli".into(),
            model: "sonnet".into(),
            prompt_version,
        }
    }

    fn scored(key: &EvalKey, article_id: i64, score: u8, scored_at: &str) -> LabeledScore {
        LabeledScore {
            key: key.clone(),
            article_id,
            score,
            scored_at: scored_at.into(),
            features: Vec::new(),
        }
    }

    fn topic(name: &str) -> Feature {
        Feature {
            kind: FeatureKind::Topic,
            key: name.into(),
        }
    }

    /// 推薦点（1 件ずつ外して学習した予測）の一致率を、LLM 点の一致率と並べる。
    /// LLM が過大評価するトピックを低く、過小評価するトピックを高く評価していれば、推薦点のほうが当たる。
    #[test]
    fn compares_the_adjusted_score_left_out() {
        let current = key("h", 3);
        let at = "2026-09-26T00:00:00.000Z";
        let mut labels = Vec::new();
        let mut scores = Vec::new();
        for i in 0..5 {
            labels.push(label(i, 1));
            scores.push(LabeledScore {
                features: vec![topic("電力市場")],
                ..scored(&current, i, 62, at)
            });
            labels.push(label(10 + i, 5));
            scores.push(LabeledScore {
                features: vec![topic("燃料")],
                ..scored(&current, 10 + i, 45, at)
            });
        }
        let out = render(&labels, &scores, Some("h"), None, 3, false, 1.0);
        assert!(
            out.contains("scored 10/10  concordance 0.00  adjusted 1.00 (leave-one-out)"),
            "{out}"
        );
    }

    #[test]
    fn renders_labels_and_the_current_key() {
        let labels = [label(1, 5), label(2, 4), label(3, 2)];
        let current = key("0123456789abcdef", 1);
        let old = key("fedcba9876543210", 1);
        let before = "2026-09-26T00:00:00.000Z";
        let scores = [
            scored(&current, 1, 95, before),
            // 反応の後に採点し直した
            scored(&current, 2, 72, "2026-09-28T00:00:00.000Z"),
            scored(&current, 3, 40, before),
            scored(&old, 1, 10, before),
        ];
        let out = render(
            &labels,
            &scores,
            Some("0123456789abcdef"),
            None,
            1,
            false,
            1.0,
        );
        assert!(
            out.starts_with("labels: 3 rated (★5 1, ★4 1, ★3 0, ★2 1, ★1 0)\n"),
            "{out}"
        );
        assert!(out.contains("fewer than 5"), "{out}");
        assert!(
            out.contains("profile 01234567  claude-cli/sonnet  prompt v1  (current)"),
            "{out}"
        );
        assert!(
            out.contains("scored 3/3  concordance 1.00  adjusted"),
            "{out}"
        );
        assert!(out.contains("1 scored after the reaction"), "{out}");
        assert!(out.contains("  score     ★1  ★2  ★3  ★4  ★5\n"), "{out}");
        assert!(out.contains("  90-100     0   0   0   0   1\n"), "{out}");
        assert!(out.contains("  70-79      0   0   0   1   0\n"), "{out}");
        assert!(out.contains("  40-49      0   1   0   0   0\n"), "{out}");
        // 既定では現行のキーだけ
        assert!(!out.contains("fedcba98"), "{out}");
        let all = render(
            &labels,
            &scores,
            Some("0123456789abcdef"),
            None,
            1,
            true,
            1.0,
        );
        assert!(
            all.contains("profile fedcba98  claude-cli/sonnet  prompt v1\n"),
            "{all}"
        );
        assert!(all.contains("scored 1/3  concordance -"), "{all}");
    }

    /// 反応の見出しをプロンプトに入れていたのは版 1 だけなので、それ以降の版には注記しない。
    #[test]
    fn notes_late_scores_only_for_prompt_v1() {
        let labels = [label(1, 4), label(2, 2)];
        let v2 = key("h", 2);
        let after = "2026-09-28T00:00:00.000Z";
        let scores = [scored(&v2, 1, 80, after), scored(&v2, 2, 20, after)];
        let out = render(&labels, &scores, Some("h"), None, 2, false, 1.0);
        assert!(out.contains("scored 2/2  concordance 1.00"), "{out}");
        assert!(!out.contains("after the reaction"), "{out}");
    }

    /// embedding の点数は、embedding の式の今の版のキーを現行として、LLM の現行のキーと並べる。その場で計算するので
    /// いつも評価の後になるが、反応の見出しは入力に無いので注記しない。
    #[test]
    fn shows_embedding_scores_as_current_without_the_late_note() {
        let labels = [label(1, 4), label(2, 2)];
        let llm = key("h", 3);
        let embedding = EvalKey {
            backend: crate::db::EMBED_BACKEND.into(),
            model: "ruri".into(),
            prompt_version: crate::embed_score::SCORE_VERSION,
            ..key("h", 0)
        };
        let after = "2026-09-28T00:00:00.000Z";
        let scores = [
            scored(&llm, 1, 80, after),
            scored(&llm, 2, 20, after),
            scored(&embedding, 1, 70, after),
            scored(&embedding, 2, 30, after),
        ];
        let out = render(&labels, &scores, Some("h"), None, 3, false, 1.0);
        assert!(
            out.contains(&format!(
                "embedding/ruri  prompt v{}  (current)",
                crate::embed_score::SCORE_VERSION
            )),
            "{out}"
        );
        assert!(
            out.contains("claude-cli/sonnet  prompt v3  (current)"),
            "{out}"
        );
        assert!(!out.contains("after the reaction"), "{out}");
        // LLM の版 1 の注記は、embedding の版が 1 でも出さない
        let v1 = EvalKey {
            prompt_version: 1,
            ..embedding
        };
        let scores = [scored(&v1, 1, 70, after), scored(&v1, 2, 30, after)];
        let out = render(&labels, &scores, Some("h"), None, 3, true, 1.0);
        assert!(!out.contains("after the reaction"), "{out}");
    }

    /// その場で計算した式の候補（trial）は、現行のプロファイルのものなら --all でなくても並べる。
    #[test]
    fn shows_trials_of_the_current_profile() {
        let labels = [label(1, 4), label(2, 2)];
        let trial = EvalKey {
            backend: TRIAL_BACKEND.into(),
            model: "ruri λ=0.5".into(),
            prompt_version: crate::embed_score::SCORE_VERSION,
            ..key("h", 0)
        };
        let other = EvalKey {
            profile_hash: "old".into(),
            ..trial.clone()
        };
        let after = "2026-09-28T00:00:00.000Z";
        let scores = [
            scored(&trial, 1, 70, after),
            scored(&trial, 2, 30, after),
            scored(&other, 1, 70, after),
        ];
        let out = render(&labels, &scores, Some("h"), None, 3, false, 1.0);
        assert!(out.contains("embedding-trial/ruri λ=0.5"), "{out}");
        assert!(out.contains("(trial)"), "{out}");
        assert!(!out.contains("profile old"), "{out}");
        assert!(!out.contains("after the reaction"), "{out}");
        // 候補が今のプロファイルと同じでも、今のプロファイルの式の候補は trial のまま
        let same = render(&labels, &scores, Some("h"), Some("h"), 3, false, 1.0);
        assert!(same.contains("(trial)"), "{same}");
        assert!(!same.contains("(candidate)"), "{same}");
    }

    /// 評価した記事を、基準と同じ式で計算した百分位で採点し、補正の特徴（ソース・トピック・関心分野）も付ける。
    #[test]
    fn scores_labeled_articles_with_a_formula() {
        use crate::embed_score::{Formula, Interest, Preference};
        let preference = Preference {
            interests: vec![Interest {
                topic: "燃料".into(),
                weight: 1.0,
                vector: vec![1.0, 0.0],
            }],
            excludes: vec![],
        };
        let labeled = [crate::db::LabeledVector {
            article_id: 7,
            source_id: "wnn".into(),
            topics: vec!["燃料".into()],
            vector: vec![1.0, 0.0],
        }];
        let reference = [vec![0.6, 0.8], vec![0.0, 1.0]];
        let trial = EvalKey {
            backend: TRIAL_BACKEND.into(),
            ..key("h", 1)
        };
        let got = embedding_trial(
            &labeled,
            &reference,
            &preference,
            Formula::default(),
            &trial,
            "2026-10-01T00:00:00.000Z",
        );
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].article_id, got[0].score), (7, 100));
        assert_eq!(got[0].key, trial);
        assert_eq!(
            got[0].features,
            crate::recommend::features("wnn", &["燃料".into()], &["燃料".into()], &[])
        );
    }

    #[test]
    fn shows_the_candidate_next_to_the_current_key() {
        let labels = [label(1, 4), label(2, 2)];
        let current = key("aaaaaaaaaaaa", 2);
        let candidate = key("bbbbbbbbbbbb", 2);
        let other = key("cccccccccccc", 2);
        let at = "2026-09-26T00:00:00.000Z";
        let scores = [
            scored(&current, 1, 40, at),
            scored(&current, 2, 60, at),
            scored(&candidate, 1, 90, at),
            scored(&candidate, 2, 10, at),
            scored(&other, 1, 50, at),
        ];
        let out = render(
            &labels,
            &scores,
            Some("aaaaaaaaaaaa"),
            Some("bbbbbbbbbbbb"),
            2,
            false,
            1.0,
        );
        let current_at = out.find("profile aaaaaaaa").unwrap();
        let candidate_at = out.find("profile bbbbbbbb").unwrap();
        assert!(current_at < candidate_at, "{out}");
        assert!(out.contains("prompt v2  (candidate)"), "{out}");
        assert!(out.contains("scored 2/2  concordance 0.00"), "{out}");
        assert!(out.contains("scored 2/2  concordance 1.00"), "{out}");
        assert!(!out.contains("cccccccc"), "{out}");
    }

    /// 候補だけ採点済みでも、現行のキーの採点が無いことを示す（比べる相手が黙って消えないように）。
    #[test]
    fn says_when_only_the_candidate_has_scores() {
        let labels = [label(1, 4)];
        let candidate = key("bbbbbbbbbbbb", 2);
        let scores = [scored(&candidate, 1, 90, "2026-09-26T00:00:00.000Z")];
        let out = render(
            &labels,
            &scores,
            Some("aaaaaaaaaaaa"),
            Some("bbbbbbbbbbbb"),
            2,
            false,
            1.0,
        );
        assert!(out.contains("no scores for the current profile"), "{out}");
        assert!(out.contains("(candidate)"), "{out}");
    }

    /// プロファイルをまだ取り込んでいなくても、候補の結果は出す。
    #[test]
    fn shows_the_candidate_without_a_saved_profile() {
        let labels = [label(1, 4), label(2, 2)];
        let candidate = key("bbbbbbbbbbbb", 2);
        let at = "2026-09-26T00:00:00.000Z";
        let scores = [scored(&candidate, 1, 90, at), scored(&candidate, 2, 10, at)];
        let out = render(&labels, &scores, None, Some("bbbbbbbbbbbb"), 2, false, 1.0);
        assert!(out.contains("no profile"), "{out}");
        assert!(out.contains("prompt v2  (candidate)"), "{out}");
        assert!(out.contains("scored 2/2  concordance 1.00"), "{out}");
    }

    #[test]
    fn renders_the_explore_miss_rate() {
        let out = render_explore(ExploreStats {
            picked: 12,
            positive: 1,
            neutral: 1,
            negative: 3,
        });
        assert_eq!(
            out,
            "\nexplore: 12 picked below the threshold, 5 rated (1 of interest, 1 neutral, 3 not)\n\
             \x20 about 20% of the rated picks were of interest (misses below the threshold)\n"
        );
        let none = render_explore(ExploreStats {
            picked: 3,
            ..ExploreStats::default()
        });
        assert_eq!(
            none,
            "\nexplore: 3 picked below the threshold, 0 rated (0 of interest, 0 neutral, 0 not)\n"
        );
        assert_eq!(render_explore(ExploreStats::default()), "");
    }

    #[test]
    fn says_when_the_current_key_has_no_scores() {
        let labels = [label(1, 4)];
        let out = render(&labels, &[], Some("h"), None, 1, false, 1.0);
        assert!(out.contains("no scores for the current profile"), "{out}");
        let out = render(&[], &[], None, None, 1, false, 1.0);
        assert!(out.starts_with("labels: 0 rated"), "{out}");
        assert!(out.contains("no profile"), "{out}");
    }
}
