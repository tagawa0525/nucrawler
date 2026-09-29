#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    fn rating(db: &Db, article_id: i64) -> Option<Rating> {
        db.search_articles(&search_query(db))
            .unwrap()
            .into_iter()
            .find(|i| i.article_id == article_id)
            .unwrap()
            .rating
    }

    #[test]
    fn ratings_are_one_to_five() {
        assert_eq!(Rating::new(0), None);
        assert_eq!(Rating::new(1).map(Rating::get), Some(1));
        assert_eq!(Rating::new(5).map(Rating::get), Some(5));
        assert_eq!(Rating::new(6), None);
    }

    /// 記事ごとに評価は 1 つで、付け直すと置き換わり、None で評価なしに戻る。
    #[test]
    fn rating_is_replaced_and_cleared() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let four = Rating::new(4).unwrap();
        let two = Rating::new(2).unwrap();
        assert_eq!(rating(&db, a), None);
        db.rate(owner, a, Some(four), t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert_eq!(rating(&db, a), Some(four));
        db.rate(owner, a, Some(two), t("2026-09-27T01:00:00Z"))
            .unwrap();
        assert_eq!(rating(&db, a), Some(two));
        assert_eq!(
            db.query_strings("SELECT rated_at FROM ratings").unwrap(),
            ["2026-09-27T01:00:00.000Z"]
        );
        db.rate(owner, a, None, t("2026-09-27T02:00:00Z")).unwrap();
        assert_eq!(rating(&db, a), None);
        assert_eq!(db.query_i64("SELECT count(*) FROM ratings").unwrap(), 0);
    }

    /// 評価 1〜2（不要）の記事は一覧の既定から隠れ、3 以上は残る。
    #[test]
    fn low_ratings_are_hidden_by_default() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let article = |url, score| {
            scored_article(&db, url, Lang::En, "2026-09-26T00:00:00.000Z", score)
        };
        let two = article("https://e.com/two", 90);
        let three = article("https://e.com/three", 80);
        let unrated = article("https://e.com/unrated", 70);
        for (id, value) in [(two, 2), (three, 3)] {
            db.rate(owner, id, Rating::new(value), t("2026-09-27T00:00:00Z"))
                .unwrap();
        }
        assert_eq!(list_ids(&db, false), [three, unrated]);
        assert_eq!(list_ids(&db, true), [two, three, unrated]);
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    hide_below: Some(60),
                    ..search_query(&db)
                }
            ),
            [unrated, three]
        );
    }

    /// 検索は、指定した評価以上の記事に絞れる（評価なしは除く）。
    #[test]
    fn search_filters_by_minimum_rating() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let article = |url, published| scored_article(&db, url, Lang::En, published, 80);
        let five = article("https://e.com/five", "2026-09-26T00:00:00.000Z");
        let four = article("https://e.com/four", "2026-09-25T00:00:00.000Z");
        let three = article("https://e.com/three", "2026-09-24T00:00:00.000Z");
        let _unrated = article("https://e.com/unrated", "2026-09-23T00:00:00.000Z");
        for (id, value) in [(five, 5), (four, 4), (three, 3)] {
            db.rate(owner, id, Rating::new(value), t("2026-09-27T00:00:00Z"))
                .unwrap();
        }
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    min_rating: Rating::new(4),
                    ..search_query(&db)
                }
            ),
            [five, four]
        );
    }
}
