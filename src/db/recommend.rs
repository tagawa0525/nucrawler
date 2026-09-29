#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;
    use crate::recommend::{Feature, FeatureKind};

    /// 関心分野 `interests` に当たったとして LLM が `llm` 点を付けた記事。
    fn article_with(db: &Db, url: &str, llm: u8, interests: &[&str]) -> i64 {
        let a = page_article(db, url, "2026-09-26T00:00:00.000Z");
        let digest = add_digest(db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        let interests: Vec<String> = interests.iter().map(|s| s.to_string()).collect();
        db.insert_score_with_matches(
            score_key(db),
            digest,
            llm,
            None,
            ScoreMatches {
                interests: &interests,
                excludes: &[],
            },
            t("2026-09-26T02:00:00Z"),
        )
        .unwrap();
        a
    }

    /// LLM が「市場」を高く付けても低く評価し、「燃料」を低く付けても高く評価してきた。
    fn rate_training(db: &Db) {
        let owner = db.owner_id().unwrap();
        for i in 0..10 {
            let market = article_with(db, &format!("https://e.com/m{i}"), 85, &["市場"]);
            db.rate(owner, market, Rating::new(1), t("2026-09-27T00:00:00Z"))
                .unwrap();
            let fuel = article_with(db, &format!("https://e.com/f{i}"), 40, &["燃料"]);
            db.rate(owner, fuel, Rating::new(5), t("2026-09-27T00:00:00Z"))
                .unwrap();
        }
    }

    fn item(db: &Db, id: i64) -> ListItem {
        db.list_articles(list_query(db, true))
            .unwrap()
            .into_iter()
            .find(|i| i.article_id == id)
            .unwrap()
    }

    /// 評価が無ければ、推薦点は LLM の点数と同じ。
    #[test]
    fn without_ratings_the_list_keeps_the_llm_scores() {
        let db = Db::open_in_memory().unwrap();
        let a = article_with(&db, "https://e.com/a", 72, &["市場"]);
        let i = item(&db, a);
        assert_eq!((i.score, i.llm_score), (Some(72), Some(72)));
    }

    /// 一覧の並び・閾値は推薦点で決まる。評価を付けると、次の読み出しから効く。
    #[test]
    fn the_list_follows_the_recommended_score() {
        let db = Db::open_in_memory().unwrap();
        let market = article_with(&db, "https://e.com/a", 80, &["市場"]);
        let fuel = article_with(&db, "https://e.com/b", 55, &["燃料"]);
        assert_eq!(item(&db, market).score, Some(80));
        let order = |db: &Db| -> Vec<i64> {
            db.list_articles(list_query(db, true))
                .unwrap()
                .into_iter()
                .map(|i| i.article_id)
                .filter(|id| [market, fuel].contains(id))
                .collect()
        };
        assert_eq!(order(&db), [market, fuel]);

        rate_training(&db);
        let (m, f) = (item(&db, market), item(&db, fuel));
        assert_eq!((m.llm_score, f.llm_score), (Some(80), Some(55)));
        assert!(
            m.score.unwrap() < 60 && f.score.unwrap() >= 60,
            "{m:?} {f:?}"
        );
        assert_eq!(order(&db), [fuel, market]);
        // 既定の一覧（最低点 60）は推薦点で絞る
        let shown = list_ids(&db, false);
        assert!(
            shown.contains(&fuel) && !shown.contains(&market),
            "{shown:?}"
        );
        // 検索の最低点も推薦点
        assert!(
            found(
                &db,
                SearchQuery {
                    min_score: Some(60),
                    ..search_query(&db)
                }
            )
            .contains(&fuel)
        );
    }

    /// 正則化を強くすると、補正はほとんど効かない。
    #[test]
    fn a_strong_prior_keeps_the_llm_scores() {
        let db = Db::open_in_memory().unwrap().with_prior_strength(1e6);
        let market = article_with(&db, "https://e.com/a", 80, &["市場"]);
        rate_training(&db);
        assert!(item(&db, market).score.unwrap().abs_diff(80) <= 1);
    }

    /// 詳細には、推薦点の補正の内訳（効いた特徴と、動かした点数）を付ける。
    #[test]
    fn detail_carries_the_adjustments() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let market = article_with(&db, "https://e.com/a", 80, &["市場"]);
        rate_training(&db);
        let d = db
            .article_detail(owner, Some("h1"), market)
            .unwrap()
            .unwrap();
        let interest = Feature {
            kind: FeatureKind::Interest,
            key: "市場".into(),
        };
        let points = d
            .adjustments
            .iter()
            .find(|(f, _)| *f == interest)
            .map(|(_, p)| *p);
        assert!(points.is_some_and(|p| p < 0), "{:?}", d.adjustments);
    }
}
