//! JSON API。応答の形は `web::api` が決める。

use super::*;

pub(super) fn json(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Web の一覧と同じ記事。Web と同じく `min` で最低点（0 なら点数で絞らない）、`rating` で評価の条件を選べる。
/// 閲覧ではないので、訪問も開いたことも記録しない。
pub(super) async fn api_list(
    State(state): State<AppState>,
    Extension(me): Extension<crate::db::Viewer>,
    Query(params): Query<ListParams>,
) -> Result<Response, AppError> {
    let body = with_db_and_config(&state, move |db, web, labels| {
        let now = Utc::now();
        let (user, hash) = viewer(db, me)?;
        let min = params.min_or(web.default_min(db.has_scores(user, hash.as_deref())?))?;
        let items = list_items(db, web, user, hash.as_deref(), now, min, params.rating()?)?;
        Ok(serde_json::to_string(&api::ArticleList::new(
            &items, labels,
        ))?)
    })
    .await?;
    Ok(json(body))
}

/// 検索画面と同じ条件の検索。閲覧ではないので、訪問も開いたことも記録しない。
pub(super) async fn api_search(
    State(state): State<AppState>,
    Extension(me): Extension<crate::db::Viewer>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    let params = Params::from_query(raw.as_deref().unwrap_or(""));
    let body = with_db_and_config(&state, move |db, web, labels| {
        let (user, hash) = viewer(db, me)?;
        let q = params.to_query(user, hash.as_deref(), web.list_limit)?;
        let items = db.search_articles(&q)?;
        Ok(serde_json::to_string(&api::ArticleList::new(
            &items, labels,
        ))?)
    })
    .await?;
    Ok(json(body))
}

#[derive(serde::Deserialize)]
pub(super) struct MarksParams {
    #[serde(default)]
    ids: String,
}

/// 一度に読み直せる印の件数の上限（一覧の件数 `web.list_limit` の既定 200 より多めに取る）。
/// 画面の側（marks.js の MAX_MARK_IDS）は、これを超える件数を分けて問い合わせる
const MAX_MARK_IDS: usize = 500;

/// 記事の印（評価・ブックマーク・既読）。`ids` はカンマ区切りの記事の ID。一覧に戻ったときの読み直しに使い、
/// 閲覧ではないので、訪問も開いたことも記録しない。
pub(super) async fn api_marks(
    State(state): State<AppState>,
    Extension(me): Extension<crate::db::Viewer>,
    Query(params): Query<MarksParams>,
) -> Result<Response, AppError> {
    let ids: Vec<i64> = if params.ids.is_empty() {
        Vec::new()
    } else {
        params
            .ids
            .split(',')
            .map(|id| id.parse().ok().filter(|id: &i64| *id > 0))
            .collect::<Option<_>>()
            .ok_or(AppError::BadRequest(
                "ids must be positive integers separated by commas",
            ))?
    };
    if ids.len() > MAX_MARK_IDS {
        return Err(AppError::BadRequest("too many ids"));
    }
    let body = with_db(&state, move |db| {
        let (user, _) = viewer(db, me)?;
        Ok(serde_json::to_string(&api::MarkList::new(
            db.marks(user, &ids)?,
        ))?)
    })
    .await?;
    Ok(json(body))
}

/// 記事 1 件の最新の要約と和訳。閲覧ではないので、開いたことを記録しない。
pub(super) async fn api_detail(
    State(state): State<AppState>,
    Extension(me): Extension<crate::db::Viewer>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let body = with_db_and_config(&state, move |db, _, labels| {
        let (user, hash) = viewer(db, me)?;
        let detail = db
            .article_detail(user, hash.as_deref(), id)?
            .ok_or(AppError::NotFound)?;
        Ok(serde_json::to_string(&api::ArticleBody::new(
            &detail, labels,
        ))?)
    })
    .await?;
    Ok(json(body))
}

#[cfg(test)]
mod tests {
    use crate::db::Db;
    use crate::web::server::test_support::*;

    /// 印の読み直し（一覧に戻ったとき）は、指定した記事の評価・ブックマーク・既読だけを返す。
    /// 閲覧ではないので、訪問も開いたことも記録しない。
    #[tokio::test]
    async fn api_marks_returns_only_the_marks() {
        let db = Db::open_in_memory().unwrap();
        let (a, _) = seed(&db, "https://e.com/a", "題A");
        let (b, _) = seed(&db, "https://e.com/b", "題B");
        db.rate(
            db.owner_id().unwrap(),
            a,
            crate::db::Rating::new(5),
            chrono::Utc::now(),
        )
        .unwrap();
        let server = Server::start(db).await;
        let (status, json) = server
            .get_json(&format!("/api/marks?ids={a},{b},999"))
            .await;
        assert_eq!(status, 200);
        assert_eq!(
            json,
            serde_json::json!({"marks": [
                // 評価しただけでは既読にならない
                {"id": a, "rating": 5, "bookmarked": false, "read": false},
                {"id": b, "rating": null, "bookmarked": false, "read": false},
            ]})
        );
        server.assert_no_views();
        for bad in ["ids=x", "ids=1,,2", "ids=-1"] {
            let (status, _) = server.get_json(&format!("/api/marks?{bad}")).await;
            assert_eq!(status, 400, "{bad}");
        }
        let many: Vec<String> = (1..=501).map(|i| i.to_string()).collect();
        let (status, _) = server
            .get_json(&format!("/api/marks?ids={}", many.join(",")))
            .await;
        assert_eq!(status, 400, "too many ids");
        let (status, json) = server.get_json("/api/marks?ids=").await;
        assert_eq!((status, json), (200, serde_json::json!({"marks": []})));
    }

    /// 画面の側は、受付の上限を超える件数を分けて問い合わせる（上限を揃えておく）。
    #[test]
    fn the_script_batches_marks_by_the_same_limit() {
        let script = include_str!("../html/assets/marks.js");
        assert!(
            script.contains(&format!("const MAX_MARK_IDS = {};", super::MAX_MARK_IDS)),
            "{script}"
        );
    }

    /// API の検索は検索画面と同じ条件で、トピックやソースを繰り返し指定できる。
    #[tokio::test]
    async fn api_search_uses_the_same_conditions() {
        let db = Db::open_in_memory().unwrap();
        let (a, _) = seed(&db, "https://e.com/a", "題A");
        seed(&db, "https://e.com/b", "題B");
        let server = Server::start(db).await;
        let (status, json) = server
            .get_json("/api/search?q=%E9%A1%8CA&topic=%E8%A6%8F%E5%88%B6%E3%83%BB%E5%AF%A9%E6%9F%BB&source=none&source=wnn")
            .await;
        assert_eq!(status, 200);
        let ids: Vec<i64> = json["articles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [a], "{json}");
        let (_, json) = server
            .get_json("/api/search?topic=%E7%87%83%E6%96%99")
            .await;
        assert!(json["articles"].as_array().unwrap().is_empty(), "{json}");
        server.assert_no_views();
    }

    /// API の一覧は既定では Web と同じ記事を出し、Web と同じく `min` で最低点（0 なら点数で絞らない）、`rating` で
    /// 評価の条件（既定は ★1〜2 を隠す）を選べる。閲覧としては記録しない。
    #[tokio::test]
    async fn api_lists_the_same_articles_as_the_web() {
        let db = Db::open_in_memory().unwrap();
        let good = seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let (status, json) = server.get_json("/api/articles").await;
        assert_eq!(status, 200);
        let articles = json["articles"].as_array().unwrap();
        assert_eq!(articles.len(), 1, "{json}");
        let a = &articles[0];
        assert_eq!(a["id"], good);
        assert_eq!(a["title_ja"], "A&B <C>\u{1}");
        assert_eq!(a["summary_ja"], "要約");
        // 点数は推薦点（種に付けた評価 2 の記事と特徴を共有するので、補正の前の点数から少し下がる）
        assert_eq!(a["llm_score"], 90);
        assert!(a["score"].as_u64().is_some_and(|s| s < 90), "{a}");
        assert_eq!(a["url"], "https://e.com/good?a=1&b=2");

        // 0 は点数の条件だけを外す（評価 2 は評価の条件で隠れたまま）
        let (_, json) = server.get_json("/api/articles?min=0").await;
        assert_eq!(json["articles"].as_array().unwrap().len(), 4, "{json}");
        let (_, json) = server.get_json("/api/articles?min=0&rating=any").await;
        assert_eq!(json["articles"].as_array().unwrap().len(), 5, "{json}");
        let (_, json) = server.get_json("/api/articles?min=5").await;
        // 低い点（10 点）も出る。無関係・未採点は 0 のときだけ
        assert_eq!(json["articles"].as_array().unwrap().len(), 2, "{json}");
        for bad in ["min=x", "rating=x"] {
            let (status, _) = server.get_json(&format!("/api/articles?{bad}")).await;
            assert_eq!(status, 400, "{bad}");
        }
        server.assert_no_views();
    }

    /// API の詳細は最新の要約と、和訳があればその本文を返す。閲覧としては記録しない。
    #[tokio::test]
    async fn api_detail_returns_latest_digest_and_translation() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        add_translation(&db, id);
        let (plain, _) = seed(&db, "https://e.com/b", "見出しB");
        let server = Server::start(db).await;

        let (status, json) = server.get_json(&format!("/api/articles/{id}")).await;
        assert_eq!(status, 200);
        assert_eq!(json["id"], id);
        assert_eq!(json["digest"]["title_ja"], "見出しA");
        assert_eq!(json["digest"]["summary_ja"], "要約");
        assert_eq!(json["digest"]["points_ja"], serde_json::json!(["点"]));
        assert_eq!(json["translation"]["body_ja"], "和訳の本文");

        let (_, json) = server.get_json(&format!("/api/articles/{plain}")).await;
        assert_eq!(json["translation"], serde_json::Value::Null, "{json}");
        assert_eq!(server.get_json("/api/articles/999").await.0, 404);
        server.assert_no_views();
    }
}
