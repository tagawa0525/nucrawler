//! embedding の近さから点数を出す式（計画 010）。I/O を持たない。
//!
//! 記事と関心分野 i の類似度 s_i、推薦しない話題 j の類似度 t_j（どちらも 0 未満は 0 に切り詰める）から、
//! 関心の強さ a（既定は重み付きの最大）と見たくなさ b = max_j t_j を求め、生の値 a − λb を、直近の記事の
//! 生の値の中での百分位にして 0〜100 点にする。モデルごとに類似度の分布が違う（0.75〜0.9 に集まるモデルもある）
//! ので、百分位にして一覧の最低点の意味をそろえる。

use crate::embedding::dot;

/// 式（まとめ方・λ・百分位の基準の取り方）と入力の組み立て方の版。変えたら上げる。
pub const SCORE_VERSION: i64 = 1;

/// 百分位の基準にする直近の要約の上限（期間の設定や記事の増え方で計算量が膨らまないように）。
pub const REFERENCE_LIMIT: usize = 3000;

/// 推薦しない話題の減点の重み。
pub const LAMBDA: f32 = 1.0;

/// 好みのベクトル。
#[derive(Debug, Clone, PartialEq)]
pub struct Preference {
    pub interests: Vec<Interest>,
    pub excludes: Vec<Exclude>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Interest {
    pub topic: String,
    pub weight: f32,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Exclude {
    pub topic: String,
    pub vector: Vec<f32>,
}

/// 関心分野ごとの「重み × 類似度」のまとめ方。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Aggregate {
    /// 最大（多くの話題に少しずつ触れる記事で上がりすぎない）
    WeightedMax,
    /// 重みで割った平均（重みが 0 の分野は除く）
    WeightedMean,
    /// 大きい方から k 個の平均
    TopK(usize),
}

/// 点数の式。本番は `Formula::default()`、`eval` はほかの式も試す。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Formula {
    pub lambda: f32,
    pub aggregate: Aggregate,
}

impl Default for Formula {
    fn default() -> Self {
        Self {
            lambda: LAMBDA,
            aggregate: Aggregate::WeightedMax,
        }
    }
}

/// 生の値と、補正の特徴にする関心分野・推薦しない話題（`Preference` の添字）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Raw {
    pub value: f32,
    /// 関心の強さが正のとき、「重み × 類似度」が最も大きい関心分野
    pub interest: Option<usize>,
    /// 減点が正で、関心を上回ったとき（λb > 0 かつ λb ≥ a）、最も近い推薦しない話題
    pub exclude: Option<usize>,
}

/// 記事のベクトル（正規化済み）の生の値。
pub fn raw(preference: &Preference, article: &[f32], formula: Formula) -> Raw {
    let similarity = |v: &[f32]| dot(v, article).max(0.0);
    let weighted: Vec<f32> = preference
        .interests
        .iter()
        .map(|i| i.weight * similarity(&i.vector))
        .collect();
    let strongest = argmax(&weighted);
    let a = match formula.aggregate {
        Aggregate::WeightedMax => strongest.map_or(0.0, |i| weighted[i]),
        Aggregate::WeightedMean => {
            let total: f32 = preference.interests.iter().map(|i| i.weight).sum();
            if total > 0.0 {
                weighted.iter().sum::<f32>() / total
            } else {
                0.0
            }
        }
        Aggregate::TopK(k) => {
            let mut sorted = weighted;
            sorted.sort_by(|x, y| y.total_cmp(x));
            let top = &sorted[..k.min(sorted.len())];
            if top.is_empty() {
                0.0
            } else {
                top.iter().sum::<f32>() / top.len() as f32
            }
        }
    };
    let excluded: Vec<f32> = preference
        .excludes
        .iter()
        .map(|e| similarity(&e.vector))
        .collect();
    let closest = argmax(&excluded);
    let b = closest.map_or(0.0, |j| excluded[j]);
    Raw {
        value: a - formula.lambda * b,
        interest: strongest.filter(|_| a > 0.0),
        exclude: closest.filter(|_| {
            let penalty = formula.lambda * b;
            penalty > 0.0 && penalty >= a
        }),
    }
}

/// 最大の値の添字（同じなら先のもの）。空なら `None`。
fn argmax(values: &[f32]) -> Option<usize> {
    values
        .iter()
        .enumerate()
        .fold(None, |best: Option<(usize, f32)>, (i, &v)| match best {
            Some((_, b)) if b >= v => best,
            _ => Some((i, v)),
        })
        .map(|(i, _)| i)
}

/// 点数と、補正の特徴にする関心分野・推薦しない話題の名前。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scored {
    pub score: u8,
    pub interest: Option<String>,
    pub exclude: Option<String>,
}

/// 好みのベクトルと式で、百分位の基準（直近の要約）に照らして記事を採点する。
#[derive(Debug, Clone)]
pub struct Scorer<'a> {
    preference: &'a Preference,
    formula: Formula,
    /// 基準の要約の生の値
    reference: Vec<f32>,
}

impl<'a> Scorer<'a> {
    /// `reference` は基準にする要約のベクトル（正規化済み）。
    pub fn new(preference: &'a Preference, formula: Formula, reference: &[Vec<f32>]) -> Self {
        Self {
            preference,
            formula,
            reference: reference
                .iter()
                .map(|v| raw(preference, v, formula).value)
                .collect(),
        }
    }

    /// 記事のベクトル（正規化済み）の点数。
    pub fn score(&self, article: &[f32]) -> Scored {
        let r = raw(self.preference, article, self.formula);
        Scored {
            score: percentile(r.value, &self.reference),
            interest: r
                .interest
                .map(|i| self.preference.interests[i].topic.clone()),
            exclude: r.exclude.map(|j| self.preference.excludes[j].topic.clone()),
        }
    }
}

/// 生の値 `value` を、基準（直近の要約の生の値）の中での百分位にして 0〜100 点にする。
/// 0 以下は 0 点。基準が無ければ 50 点。同じ値は中間の順位にする。
pub fn percentile(value: f32, reference: &[f32]) -> u8 {
    if value <= 0.0 {
        return 0;
    }
    if reference.is_empty() {
        return 50;
    }
    let below = reference.iter().filter(|&&r| r < value).count();
    let equal = reference.iter().filter(|&&r| r == value).count();
    let share = (below as f64 + 0.5 * equal as f64) / reference.len() as f64;
    // 0〜1 の割合なので、丸めた値は 0〜100 に収まる
    (100.0 * share).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2 次元の単位ベクトル（角度 θ 度）。
    fn at(degrees: f32) -> Vec<f32> {
        let r = degrees.to_radians();
        vec![r.cos(), r.sin()]
    }

    fn interest(topic: &str, weight: f32, degrees: f32) -> Interest {
        Interest {
            topic: topic.into(),
            weight,
            vector: at(degrees),
        }
    }

    fn exclude(topic: &str, degrees: f32) -> Exclude {
        Exclude {
            topic: topic.into(),
            vector: at(degrees),
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    /// 関心の強さは重み付きの最大。重い分野に近い記事の方が上で、その分野が特徴になる。
    #[test]
    fn weighted_max_prefers_heavier_interests() {
        let p = Preference {
            interests: vec![interest("heavy", 1.0, 0.0), interest("light", 0.4, 90.0)],
            excludes: vec![],
        };
        let near_heavy = raw(&p, &at(0.0), Formula::default());
        let near_light = raw(&p, &at(90.0), Formula::default());
        assert!(close(near_heavy.value, 1.0), "{near_heavy:?}");
        assert!(close(near_light.value, 0.4), "{near_light:?}");
        assert_eq!(near_heavy.interest, Some(0));
        assert_eq!(near_light.interest, Some(1));
        assert_eq!(near_heavy.exclude, None);
    }

    /// 推薦しない話題に近い記事は下がる。減点が関心を上回ったときだけ、その話題が特徴になる。
    #[test]
    fn excludes_lower_the_value() {
        let p = Preference {
            interests: vec![interest("i", 1.0, 0.0)],
            excludes: vec![exclude("x", 60.0)],
        };
        // 関心 cos 60° = 0.5、減点 cos 0° = 1
        let near_exclude = raw(&p, &at(60.0), Formula::default());
        assert!(close(near_exclude.value, 0.5 - 1.0), "{near_exclude:?}");
        assert_eq!(near_exclude.exclude, Some(0));
        // 関心 1、減点 0.5：関心が上回るので、推薦しない話題は特徴にしない
        let near_interest = raw(&p, &at(0.0), Formula::default());
        assert!(close(near_interest.value, 0.5), "{near_interest:?}");
        assert_eq!(near_interest.exclude, None);
        let lighter = Formula {
            lambda: 0.5,
            ..Formula::default()
        };
        assert!(close(raw(&p, &at(60.0), lighter).value, 0.0));
        // 減点しない式（λ = 0）では、推薦しない話題を特徴にしない（関心が 0 の記事でも）
        let no_penalty = Formula {
            lambda: 0.0,
            ..Formula::default()
        };
        assert_eq!(raw(&p, &at(150.0), no_penalty).exclude, None);
    }

    /// 類似度は 0 未満を 0 に切り詰める。推薦しない話題と反対向きの記事も加点されず、関心と反対向きの記事で
    /// 重みの小さい分野が選ばれることもない。
    #[test]
    fn negative_similarities_count_as_zero() {
        let p = Preference {
            interests: vec![interest("heavy", 1.0, 0.0), interest("light", 0.4, 10.0)],
            excludes: vec![exclude("x", 0.0)],
        };
        let opposite = raw(&p, &at(180.0), Formula::default());
        assert!(close(opposite.value, 0.0), "{opposite:?}");
        assert_eq!((opposite.interest, opposite.exclude), (None, None));
    }

    /// 推薦しない話題が無ければ減点しない。関心分野が無ければ関心の強さは 0。
    #[test]
    fn empty_lists_contribute_nothing() {
        let only_interest = Preference {
            interests: vec![interest("i", 0.7, 0.0)],
            excludes: vec![],
        };
        assert!(close(
            raw(&only_interest, &at(0.0), Formula::default()).value,
            0.7
        ));
        let only_exclude = Preference {
            interests: vec![],
            excludes: vec![exclude("x", 0.0)],
        };
        let r = raw(&only_exclude, &at(0.0), Formula::default());
        assert!(close(r.value, -1.0), "{r:?}");
        assert_eq!(r.interest, None);
    }

    /// 平均と上位 k 個の平均（`eval` で比べる）。
    #[test]
    fn other_aggregates() {
        let p = Preference {
            interests: vec![
                interest("a", 1.0, 0.0),
                interest("b", 0.5, 0.0),
                interest("c", 0.0, 0.0),
            ],
            excludes: vec![],
        };
        let mean = Formula {
            aggregate: Aggregate::WeightedMean,
            ..Formula::default()
        };
        // (1×1 + 0.5×1) / (1 + 0.5)。重み 0 の分野は除く
        assert!(close(raw(&p, &at(0.0), mean).value, 1.0));
        let top2 = Formula {
            aggregate: Aggregate::TopK(2),
            ..Formula::default()
        };
        assert!(close(raw(&p, &at(0.0), top2).value, 0.75));
        let r = raw(&p, &at(0.0), top2);
        assert_eq!(r.interest, Some(0));
    }

    /// 共通の向きに寄った単位ベクトル（どの文どうしも類似度が高いモデルを模す）。
    fn tilted(x: f32, y: f32) -> Vec<f32> {
        let norm = (1.0 + x * x + y * y).sqrt();
        vec![1.0 / norm, x / norm, y / norm]
    }

    /// どの文どうしも類似度が高いモデルでは、推薦しない話題との類似度が関心分野との類似度と同じ程度になる。
    /// 類似度を基準の要約での平均と標準偏差で標準化してから組み合わせ、関心分野に寄った記事を 0 点に落とさない。
    #[test]
    fn standardizes_similarities_against_the_reference() {
        let p = Preference {
            interests: vec![Interest {
                topic: "i".into(),
                weight: 0.9,
                vector: tilted(0.3, 0.0),
            }],
            excludes: vec![Exclude {
                topic: "x".into(),
                vector: tilted(0.0, 0.3),
            }],
        };
        let reference = [
            tilted(0.3, 0.0),
            tilted(0.0, 0.3),
            tilted(0.0, 0.0),
            tilted(-0.3, 0.0),
            tilted(0.0, -0.3),
        ];
        let scorer = Scorer::new(&p, Formula::default(), &reference);
        // 関心分野に寄った記事は基準の中で最も高い（ほかの 4 件より上で、自分と同じ値が 1 件）
        let near_interest = scorer.score(&tilted(0.3, 0.0));
        assert_eq!(near_interest.score, 90, "{near_interest:?}");
        assert_eq!(near_interest.interest.as_deref(), Some("i"));
        assert_eq!(near_interest.exclude, None);
        // 推薦しない話題に寄った記事は 0 点で、その話題が特徴になる
        let near_exclude = scorer.score(&tilted(0.0, 0.3));
        assert_eq!(near_exclude.score, 0, "{near_exclude:?}");
        assert_eq!(near_exclude.exclude.as_deref(), Some("x"));
    }

    /// 基準の類似度がばらつかない（基準が 1 件以下か、全部同じ）ときは、標準化せずに類似度をそのまま使う。
    #[test]
    fn keeps_raw_similarities_without_spread() {
        let p = Preference {
            interests: vec![interest("i", 1.0, 0.0)],
            excludes: vec![],
        };
        for reference in [vec![], vec![at(30.0)], vec![at(30.0), at(30.0)]] {
            let scorer = Scorer::new(&p, Formula::default(), &reference);
            let scored = scorer.score(&at(0.0));
            assert_eq!(scored.interest.as_deref(), Some("i"), "{reference:?}");
            assert!(scored.score > 0, "{reference:?}: {scored:?}");
        }
    }

    /// 百分位：基準の中で小さい値の数と、同じ値の半分の数の割合。0 以下は 0 点、基準が無ければ 50 点。
    #[test]
    fn percentiles() {
        let reference = [0.1, 0.2, 0.3, 0.4];
        assert_eq!(percentile(0.3, &reference), 63); // (2 + 0.5) / 4
        assert_eq!(percentile(0.5, &reference), 100);
        assert_eq!(percentile(0.05, &reference), 0);
        assert_eq!(percentile(0.25, &reference), 50);
        assert_eq!(percentile(0.0, &reference), 0);
        assert_eq!(percentile(-0.2, &reference), 0);
        assert_eq!(percentile(0.3, &[]), 50);
        assert_eq!(percentile(0.0, &[]), 0);
        // 全部同じ値なら中央
        assert_eq!(percentile(0.3, &[0.3, 0.3, 0.3]), 50);
    }
}
