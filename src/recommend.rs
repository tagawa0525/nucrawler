//! 推薦点：関心プロファイルとの近さの点数（embedding。計画 017）に、利用者の評価から学んだ補正を足した点数（計画 004）。
//!
//! 記事ごとに `z = x + Σ w_f`（x は LLM 点の logit、f は記事の特徴）を求め、`100·σ(z)` を推薦点にする。
//! 評価 1〜5 を 0〜1 に写した値を目標に、交差エントロピーと L2 の正則化で特徴ごとの重み w を学ぶ。正則化は
//! 今の振る舞い（w = 0：推薦点 = LLM 点）を中心に置くので、評価が少ないうちは補正が小さい。
//! 全記事に効く係数（LLM 点の傾きや全体のずれ）は学ばない。学ぶと評価 1 件で全記事の点数が動いてしまい、
//! 評価した記事と特徴を共有しない記事まで補正されるため。
//! I/O を持たない。

use std::collections::{BTreeMap, BTreeSet};

use crate::db::Rating;

/// 特徴の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureKind {
    /// 要約に付いたトピック（統合を反映した語）
    Topic,
    /// 記事のソース
    Source,
    /// 点数が当たった関心分野
    Interest,
    /// 点数が当たった推薦しない話題
    Exclude,
}

/// 記事の特徴（あれば 1、無ければ 0）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Feature {
    pub kind: FeatureKind,
    pub key: String,
}

/// 学習の 1 件：評価した記事の LLM 点と特徴、評価。
#[derive(Debug, Clone, PartialEq)]
pub struct Example {
    pub base_score: u8,
    pub features: Vec<Feature>,
    pub rating: Rating,
}

/// 補正のモデル。`weights` に無い特徴は 0（効かない）。
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub weights: BTreeMap<Feature, f64>,
}

/// LLM 点を確率とみなすときの端（0 点・100 点で logit が無限にならないように）
const EDGE: f64 = 0.005;
/// Newton 法の打ち切り
const MAX_ITERATIONS: usize = 100;
const TOLERANCE: f64 = 1e-10;

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

/// LLM 点（0〜100）の logit。
fn base_logit(base_score: u8) -> f64 {
    let p = (f64::from(base_score) / 100.0).clamp(EDGE, 1.0 - EDGE);
    (p / (1.0 - p)).ln()
}

/// 確率を 0〜100 の点数にする。
fn points(p: f64) -> u8 {
    // p は (0, 1) に収まるので、四捨五入した値は 0〜100
    (p * 100.0).round() as u8
}

/// 評価を 0〜1 の目標にする（1→0、3→0.5、5→1）。
fn target(rating: Rating) -> f64 {
    f64::from(rating.get() - Rating::MIN) / f64::from(Rating::MAX - Rating::MIN)
}

/// 重複を除いた特徴。
fn distinct(features: &[Feature]) -> BTreeSet<&Feature> {
    features.iter().collect()
}

impl Model {
    /// 今の振る舞い（推薦点 = LLM 点）。
    pub fn identity() -> Self {
        Self {
            weights: BTreeMap::new(),
        }
    }

    /// 評価から学ぶ。`prior_strength` は正則化の強さ（大きいほど今の振る舞いに近いまま）で、正の有限値であること
    /// （設定から来る値は読み込むときに確かめる）。重みを持つのは、評価した記事に現れた特徴だけ。
    pub fn fit(examples: &[Example], prior_strength: f64) -> Self {
        assert!(
            prior_strength.is_finite() && prior_strength > 0.0,
            "prior_strength must be positive and finite"
        );
        let features: Vec<&Feature> = examples
            .iter()
            .flat_map(|e| e.features.iter())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let index: BTreeMap<&Feature, usize> =
            features.iter().enumerate().map(|(i, f)| (*f, i)).collect();
        // 1 件ごとの (LLM 点の logit, 値が 1 の特徴の列, 目標)
        let rows: Vec<(f64, Vec<usize>, f64)> = examples
            .iter()
            .map(|e| {
                let cols = distinct(&e.features)
                    .into_iter()
                    .map(|f| index[f])
                    .collect();
                (base_logit(e.base_score), cols, target(e.rating))
            })
            .collect();
        let n = features.len();
        // 事前の中心は w = 0（推薦点 = LLM 点）
        let mut theta = vec![0.0; n];
        let z = |theta: &[f64], (x, cols, _): &(f64, Vec<usize>, f64)| {
            x + cols.iter().map(|&c| theta[c]).sum::<f64>()
        };
        let loss = |theta: &[f64]| {
            let data: f64 = rows
                .iter()
                .map(|row| {
                    let z = z(theta, row);
                    // 交差エントロピー：y·log(1+e^-z) + (1-y)·log(1+e^z)
                    row.2 * softplus(-z) + (1.0 - row.2) * softplus(z)
                })
                .sum();
            let penalty: f64 = theta.iter().map(|t| t.powi(2)).sum();
            data + prior_strength / 2.0 * penalty
        };
        for _ in 0..MAX_ITERATIONS {
            let mut grad: Vec<f64> = theta.iter().map(|t| prior_strength * t).collect();
            let mut hess = vec![vec![0.0; n]; n];
            for (i, row) in hess.iter_mut().enumerate() {
                row[i] = prior_strength;
            }
            for row in &rows {
                let p = sigmoid(z(&theta, row));
                let (_, cols, y) = row;
                for &i in cols {
                    grad[i] += p - y;
                    for &j in cols {
                        hess[i][j] += p * (1.0 - p);
                    }
                }
            }
            let step = solve_spd(hess, &grad);
            // 損失が減るまで歩幅を半分にする（凸なので必ず減る向き）
            let before = loss(&theta);
            let mut scale = 1.0;
            let next = loop {
                let next: Vec<f64> = theta
                    .iter()
                    .zip(&step)
                    .map(|(t, s)| t - scale * s)
                    .collect();
                if loss(&next) <= before || scale < 1e-8 {
                    break next;
                }
                scale /= 2.0;
            };
            let moved = next
                .iter()
                .zip(&theta)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            theta = next;
            if moved < TOLERANCE {
                break;
            }
        }
        Self {
            weights: features.into_iter().cloned().zip(theta).collect(),
        }
    }

    /// 特徴の重みの和（知らない特徴は 0）。
    pub fn weight_sum(&self, features: &[Feature]) -> f64 {
        distinct(features)
            .into_iter()
            .filter_map(|f| self.weights.get(f))
            .sum()
    }

    /// 推薦点（0〜100）。
    pub fn score(&self, base_score: u8, features: &[Feature]) -> u8 {
        score_from(base_score, self.weight_sum(features))
    }

    /// 重みを、特徴のキー（`feature_key`）から重みへの JSON のオブジェクトにする（SQL に渡す）。
    pub fn weights_json(&self) -> String {
        let map: serde_json::Map<String, serde_json::Value> = self
            .weights
            .iter()
            .map(|(f, w)| (feature_key(f), (*w).into()))
            .collect();
        serde_json::Value::Object(map).to_string()
    }

    /// 補正の内訳：特徴ごとに、その特徴が無かったときの推薦点からどれだけ動かしたか（点）。
    /// 動かしていない特徴は除き、大きく効いたものから並べる。
    pub fn contributions(&self, base_score: u8, features: &[Feature]) -> Vec<(Feature, i32)> {
        let all = i32::from(self.score(base_score, features));
        let mut parts: Vec<(Feature, i32)> = distinct(features)
            .into_iter()
            .filter(|f| self.weights.contains_key(*f))
            .map(|f| {
                let others: Vec<Feature> = features.iter().filter(|g| *g != f).cloned().collect();
                (f.clone(), all - i32::from(self.score(base_score, &others)))
            })
            .filter(|(_, p)| *p != 0)
            .collect();
        parts.sort_by(|a, b| b.1.abs().cmp(&a.1.abs()).then_with(|| a.0.cmp(&b.0)));
        parts
    }
}

/// 推薦点を、LLM 点と特徴の重みの和から求める（SQL の `recommend_score` と `Model::score` で共有する）。
pub fn score_from(base_score: u8, weight_sum: f64) -> u8 {
    points(sigmoid(base_logit(base_score) + weight_sum))
}

/// 特徴を SQL で突き合わせるキー（`topic:燃料` など）。
pub fn feature_key(f: &Feature) -> String {
    let kind = match f.kind {
        FeatureKind::Topic => "topic",
        FeatureKind::Source => "source",
        FeatureKind::Interest => "interest",
        FeatureKind::Exclude => "exclude",
    };
    format!("{kind}:{}", f.key)
}

/// 記事の特徴：ソース、要約のトピック、点数が当たった関心分野と推薦しない話題。
pub fn features(
    source_id: &str,
    topics: &[String],
    matched: &[String],
    excluded: &[String],
) -> Vec<Feature> {
    let of = |kind, keys: &[String]| {
        keys.iter()
            .map(move |key| Feature {
                kind,
                key: key.clone(),
            })
            .collect::<Vec<_>>()
    };
    let mut all = vec![Feature {
        kind: FeatureKind::Source,
        key: source_id.to_string(),
    }];
    all.extend(of(FeatureKind::Topic, topics));
    all.extend(of(FeatureKind::Interest, matched));
    all.extend(of(FeatureKind::Exclude, excluded));
    all
}

/// 1 件ずつ外して学習し、外した 1 件の推薦点を予測する（例の順）。学習と評価に同じ評価を使うと
/// 当たり具合が良く出すぎるので、`eval` の比較にはこちらを使う。
pub fn leave_one_out(examples: &[Example], prior_strength: f64) -> Vec<u8> {
    (0..examples.len())
        .map(|i| {
            let others: Vec<Example> = examples
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, e)| e.clone())
                .collect();
            let e = &examples[i];
            Model::fit(&others, prior_strength).score(e.base_score, &e.features)
        })
        .collect()
}

/// log(1 + e^z) を、z が大きくても溢れないように求める。
fn softplus(z: f64) -> f64 {
    if z > 0.0 {
        z + (-z).exp().ln_1p()
    } else {
        z.exp().ln_1p()
    }
}

/// 正定値対称行列 `a` について `a·x = b` を、コレスキー分解で解く。
fn solve_spd(mut a: Vec<Vec<f64>>, b: &[f64]) -> Vec<f64> {
    let n = b.len();
    // a を下三角 L（a = L·Lᵀ）で置き換える
    for j in 0..n {
        let d = a[j][j] - (0..j).map(|k| a[j][k] * a[j][k]).sum::<f64>();
        // 正則化で対角は正なので、丸め誤差以外で負にはならない
        let d = d.max(f64::MIN_POSITIVE).sqrt();
        a[j][j] = d;
        for i in j + 1..n {
            let s = a[i][j] - (0..j).map(|k| a[i][k] * a[j][k]).sum::<f64>();
            a[i][j] = s / d;
        }
    }
    let mut y = vec![0.0; n];
    for i in 0..n {
        y[i] = (b[i] - (0..i).map(|k| a[i][k] * y[k]).sum::<f64>()) / a[i][i];
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        x[i] = (y[i] - (i + 1..n).map(|k| a[k][i] * x[k]).sum::<f64>()) / a[i][i];
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic(name: &str) -> Feature {
        Feature {
            kind: FeatureKind::Topic,
            key: name.into(),
        }
    }

    fn example(base_score: u8, features: &[Feature], rating: u8) -> Example {
        Example {
            base_score,
            features: features.to_vec(),
            rating: Rating::new(rating).unwrap(),
        }
    }

    /// `n` 件の同じ例。
    fn repeat(n: usize, e: Example) -> Vec<Example> {
        std::iter::repeat_n(e, n).collect()
    }

    /// 評価が 0 件なら、推薦点は LLM 点と同じ（今の振る舞い）。0 と 100 だけは端を丸めて 1 と 100。
    #[test]
    fn without_ratings_the_score_is_the_llm_score() {
        let model = Model::fit(&[], 1.0);
        for llm in 1..=99 {
            assert_eq!(model.score(llm, &[topic("燃料")]), llm, "{llm}");
        }
        assert_eq!(model.score(0, &[]), 1);
        assert_eq!(model.score(100, &[]), 100);
        assert!(model.weights.is_empty());
    }

    /// 記事の特徴：ソース・要約のトピック・点数が当たった関心分野と推薦しない話題。
    #[test]
    fn features_of_an_article() {
        let f = features(
            "nrc",
            &["燃料".to_string()],
            &["燃料".to_string()],
            &["核融合".to_string()],
        );
        let kind = |k| {
            f.iter()
                .filter(|x| x.kind == k)
                .map(|x| x.key.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(kind(FeatureKind::Source), ["nrc"]);
        assert_eq!(kind(FeatureKind::Topic), ["燃料"]);
        assert_eq!(kind(FeatureKind::Interest), ["燃料"]);
        assert_eq!(kind(FeatureKind::Exclude), ["核融合"]);
    }

    /// 1 件ずつ外して学習し、外した 1 件を予測する。外した評価を使わないので、学習に使った場合より控えめになる。
    /// 1 件だけなら、外すと評価が無いので LLM 点のまま。
    #[test]
    fn leave_one_out_predicts_without_the_held_out_rating() {
        let market = topic("電力市場");
        let examples = repeat(6, example(85, std::slice::from_ref(&market), 1));
        let held_out = leave_one_out(&examples, 1.0);
        let in_sample = Model::fit(&examples, 1.0).score(85, std::slice::from_ref(&market));
        assert_eq!(held_out.len(), 6);
        assert!(
            held_out.iter().all(|&s| in_sample < s && s < 85),
            "{held_out:?} {in_sample}"
        );
        assert_eq!(leave_one_out(&examples[..1], 1.0), [85]);
        assert!(leave_one_out(&[], 1.0).is_empty());
    }

    /// 正則化の強さは正の有限値だけ（∞ だと勾配が ∞·0 で NaN になる）。
    #[test]
    #[should_panic(expected = "prior_strength must be positive and finite")]
    fn rejects_an_infinite_prior_strength() {
        Model::fit(&[], f64::INFINITY);
    }

    /// LLM が高く付けたのに低く評価したトピックは下げ、低く付けたのに高く評価したトピックは上げる。
    #[test]
    fn learns_topics_the_llm_misjudges() {
        let market = topic("電力市場");
        let fuel = topic("燃料");
        let mut examples = repeat(6, example(85, std::slice::from_ref(&market), 1));
        examples.extend(repeat(6, example(40, std::slice::from_ref(&fuel), 5)));
        let model = Model::fit(&examples, 1.0);
        assert!(model.weights[&market] < 0.0, "{:?}", model.weights);
        assert!(model.weights[&fuel] > 0.0, "{:?}", model.weights);
        assert!(model.score(85, std::slice::from_ref(&market)) < 70);
        assert!(model.score(40, std::slice::from_ref(&fuel)) > 55);
        // 当てはまらない特徴は効かない
        assert_eq!(
            model.score(60, &[topic("未知")]),
            model.score(60, &[]),
            "{model:?}"
        );
    }

    /// 評価が少ないうちは補正を小さくし、同じ傾向の評価が増えるほど大きくする。事前の強さを上げると小さくなる。
    #[test]
    fn shrinks_when_ratings_are_few() {
        let market = topic("電力市場");
        let drop = |n: usize, prior: f64| {
            let model = Model::fit(
                &repeat(n, example(85, std::slice::from_ref(&market), 1)),
                prior,
            );
            85 - i32::from(model.score(85, std::slice::from_ref(&market)))
        };
        let (one, three, ten) = (drop(1, 1.0), drop(3, 1.0), drop(10, 1.0));
        assert!(0 < one && one < three && three < ten, "{one} {three} {ten}");
        assert!(drop(3, 10.0) < three, "stronger prior shrinks more");
    }

    /// 評価 3 は「どちらでもない」で、LLM 点が 50 なら補正しない（予測と目標が一致する）。
    #[test]
    fn a_neutral_rating_at_the_midpoint_changes_nothing() {
        let t = topic("規制・審査");
        let model = Model::fit(&repeat(5, example(50, std::slice::from_ref(&t), 3)), 1.0);
        assert!(model.weights[&t].abs() < 1e-9, "{model:?}");
        assert_eq!(model.score(50, std::slice::from_ref(&t)), 50);
    }

    /// 補正は特徴ごとにだけ学ぶ。評価した記事と特徴を共有しない記事の推薦点は、評価がいくつあっても LLM 点のまま
    /// （全体の傾きやずれを学ぶと、評価 1 件で全記事の点数が動いてしまう）。
    #[test]
    fn ratings_do_not_move_articles_without_their_features() {
        let market = topic("電力市場");
        let model = Model::fit(&[example(95, std::slice::from_ref(&market), 2)], 1.0);
        for llm in [20, 50, 70, 90] {
            assert_eq!(model.score(llm, &[topic("燃料")]), llm, "{llm}");
        }
        assert!(model.score(95, std::slice::from_ref(&market)) < 95);
    }

    /// 同じ入力なら同じ結果。同じ特徴が重なっても 1 つとして数える。
    #[test]
    fn fitting_is_deterministic_and_features_count_once() {
        let fuel = topic("燃料");
        let examples = [
            example(30, &[fuel.clone(), fuel.clone()], 5),
            example(70, &[topic("電力市場")], 2),
        ];
        let a = Model::fit(&examples, 1.0);
        let b = Model::fit(&examples, 1.0);
        assert_eq!(a, b);
        let once = Model::fit(
            &[
                example(30, std::slice::from_ref(&fuel), 5),
                examples[1].clone(),
            ],
            1.0,
        );
        assert_eq!(a, once);
        assert_eq!(
            a.score(30, &[fuel.clone(), fuel.clone()]),
            a.score(30, std::slice::from_ref(&fuel))
        );
    }

    /// 補正の内訳：特徴ごとに、その特徴が無かったときからどれだけ点数を動かしたか。効いていない特徴は出さない。
    #[test]
    fn contributions_explain_the_adjustment() {
        let market = topic("電力市場");
        let nrc = Feature {
            kind: FeatureKind::Source,
            key: "nrc".into(),
        };
        let mut examples = repeat(6, example(85, std::slice::from_ref(&market), 1));
        examples.extend(repeat(6, example(40, std::slice::from_ref(&nrc), 5)));
        let model = Model::fit(&examples, 1.0);
        let features = [market.clone(), nrc.clone(), topic("未知")];
        let parts = model.contributions(70, &features);
        let points = |f: &Feature| parts.iter().find(|(g, _)| g == f).map(|(_, p)| *p);
        assert!(points(&market).unwrap() < 0, "{parts:?}");
        assert!(points(&nrc).unwrap() > 0, "{parts:?}");
        assert_eq!(points(&topic("未知")), None, "{parts:?}");
        // 大きく効いたものから並べる
        let sizes: Vec<i32> = parts.iter().map(|(_, p)| p.abs()).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{parts:?}");
    }
}
