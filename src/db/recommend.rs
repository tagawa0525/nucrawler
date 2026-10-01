//! 推薦点（LLM の点数に、評価から学んだ補正を足した点数。`crate::recommend`）の DB 側：
//! SQL の関数 `recommend_score`、評価からの学習の材料の読み出し、学習したモデルの使い回し。

use super::*;
use crate::recommend::{Example, Model};

/// 学習したモデルと、学習した材料。材料が変わるまで使い回す。
pub(super) struct ModelCache {
    user_id: i64,
    profile_hash: String,
    examples: Vec<Example>,
    model: Model,
}

/// SQL の関数 `recommend_score(llm_score, weight_sum)` を登録する。LLM 点が NULL（未採点）なら NULL。
pub(super) fn register_functions(conn: &Connection) -> Result<(), DbError> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "recommend_score",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let Some(llm) = ctx.get::<Option<u8>>(0)? else {
                return Ok(None);
            };
            Ok(Some(crate::recommend::score_from(llm, ctx.get::<f64>(1)?)))
        },
    )?;
    Ok(())
}

/// 別名 `rows`（記事と `source_id`・`digest_id`）と `s`（採点）の行の推薦点を求める SQL の式。
/// 重みは `:rec_weights`（`Model::weights_json`）で渡す。
/// 重みは、キーが記事の特徴（`crate::recommend::feature_key`）に当たるものを 1 回ずつ足す。
pub(super) fn recommend_score_sql() -> &'static str {
    "recommend_score(s.score, (
       SELECT total(w.value) FROM json_each(:rec_weights) AS w
       WHERE w.key = 'source:' || rows.source_id
          OR w.key IN (
            SELECT 'topic:' || t.name FROM artifact_topics AS at
            JOIN topics AS t ON t.id = at.topic_id
            WHERE at.artifact_id = rows.digest_id)
          OR w.key IN (
            SELECT kind || ':' || topic FROM score_matches WHERE score_id = s.id)))"
}

impl Db {
    /// 利用者の推薦点のモデル。プロファイルが無ければ今の振る舞い（推薦点 = LLM 点）。
    /// 学習の材料（評価した記事ごとの LLM 点・特徴・評価）を毎回読み、前に学習したときと違えば学習し直す。
    /// 材料は評価した記事の分だけなので軽く、評価・採点・最新の要約・トピックの統合のどの変化も漏らさない。
    pub(super) fn recommend_model(
        &self,
        user_id: i64,
        profile_hash: Option<&str>,
    ) -> Result<Model, DbError> {
        let Some(profile_hash) = profile_hash else {
            return Ok(Model::identity());
        };
        let examples = self.recommend_examples(user_id, profile_hash)?;
        if let Some(cache) = self.recommend.borrow().as_ref()
            && cache.user_id == user_id
            && cache.profile_hash == profile_hash
            && cache.examples == examples
        {
            return Ok(cache.model.clone());
        }
        let model = Model::fit(&examples, self.prior_strength);
        *self.recommend.borrow_mut() = Some(ModelCache {
            user_id,
            profile_hash: profile_hash.to_string(),
            examples,
            model: model.clone(),
        });
        Ok(model)
    }

    /// 学習の材料：評価した記事ごとに、一覧と同じ採点（閲覧できる最新の digest の、最新のプロンプトの版で
    /// 最高点の採点）の LLM 点と特徴、評価。そのプロファイルで採点されていない記事は使わない。
    fn recommend_examples(
        &self,
        user_id: i64,
        profile_hash: &str,
    ) -> Result<Vec<Example>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH rated AS (
               SELECT rt.article_id, rt.value,
                      (SELECT r.id FROM artifacts AS r
                       WHERE r.article_id = rt.article_id AND r.kind = 'digest' AND {viewable}
                       ORDER BY r.created_at DESC, r.id DESC LIMIT 1) AS digest_id
               FROM ratings AS rt WHERE rt.user_id = :user),
             scored AS (
               SELECT rated.*,
                      (SELECT s.id FROM scores AS s
                       WHERE s.user_id = :user AND s.profile_hash = :profile
                         AND s.artifact_id = rated.digest_id
                         -- embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）使わない
                         AND s.backend <> 'embedding'
                       ORDER BY s.prompt_version DESC, s.score DESC, s.created_at DESC, s.id DESC
                       LIMIT 1) AS score_id
               FROM rated)
             SELECT x.value, s.score, a.source_id, {topics},
                    (SELECT json_group_array(topic) FROM (
                       SELECT topic FROM score_matches
                       WHERE score_id = s.id AND kind = 'interest' ORDER BY topic)),
                    (SELECT json_group_array(topic) FROM (
                       SELECT topic FROM score_matches
                       WHERE score_id = s.id AND kind = 'exclude' ORDER BY topic))
             FROM scored AS x
             JOIN scores AS s ON s.id = x.score_id
             JOIN artifacts AS r ON r.id = x.digest_id
             JOIN articles AS a ON a.id = x.article_id
             ORDER BY x.article_id",
            viewable = super::read::viewable("r"),
            topics = super::read::linked_topics("r"),
        ))?;
        let rows = stmt.query_map(
            rusqlite::named_params! {":user": user_id, ":profile": profile_hash},
            |r| {
                Ok((
                    r.get::<_, Rating>(0)?,
                    r.get::<_, u8>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (rating, llm_score, source, topics, matched, excluded) = row?;
            let [topics, matched, excluded]: [Vec<String>; 3] = [
                serde_json::from_str(&topics)?,
                serde_json::from_str(&matched)?,
                serde_json::from_str(&excluded)?,
            ];
            Ok(Example {
                llm_score,
                features: crate::recommend::features(&source, &topics, &matched, &excluded),
                rating,
            })
        })
        .collect()
    }
}

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

    /// embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）補正の学習に使わない。
    #[test]
    fn examples_ignore_embedding_scores_for_now() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = embedding_scored_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z", 95);
        db.rate(owner, a, Rating::new(5), t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert!(db.recommend_examples(owner, "h1").unwrap().is_empty());
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
        // 仕組みを確かめるので、少ない評価でもはっきり効く強さにする（既定の強さは設定のテストで確かめる）
        let db = Db::open_in_memory().unwrap().with_prior_strength(1.0);
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

    /// 学習の材料が変われば、評価や採点が増えなくても学習し直す。例えば評価した記事に新しい要約が付き、
    /// まだ採点されていなければ、その記事は材料から外れる。
    #[test]
    fn the_model_follows_changes_to_its_inputs() {
        let db = Db::open_in_memory().unwrap().with_prior_strength(1.0);
        let market = article_with(&db, "https://e.com/a", 80, &["市場"]);
        rate_training(&db);
        assert!(item(&db, market).score.unwrap() < 80);
        // 評価した記事すべてに、まだ採点していない新しい要約が付いた
        let rated: Vec<i64> = db
            .conn()
            .prepare("SELECT article_id FROM ratings")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for id in rated {
            add_digest(&db, id, "opus", "新しい版", true, "2026-09-28T00:00:00Z");
        }
        assert_eq!(item(&db, market).score, Some(80));
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
        let db = Db::open_in_memory().unwrap().with_prior_strength(1.0);
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
