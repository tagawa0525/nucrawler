#[cfg(test)]
mod tests {
    use super::*;

    fn topic(name: &str) -> Feature {
        Feature {
            kind: FeatureKind::Topic,
            key: name.into(),
        }
    }

    fn example(llm_score: u8, features: &[Feature], rating: u8) -> Example {
        Example {
            llm_score,
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
        assert!(
            (model.a - 1.0).abs() < 1e-9 && model.b.abs() < 1e-9,
            "{model:?}"
        );
        assert_eq!(model.score(50, std::slice::from_ref(&t)), 50);
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
