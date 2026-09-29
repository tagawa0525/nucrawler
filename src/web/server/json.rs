//! JSON API。応答の形は `web::api` が決める。

use super::*;

pub(super) fn json(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Web の一覧と同じ記事（`min` で最低点を選べ、0 ならすべて）。閲覧ではないので、訪問も開いたことも記録しない。
pub(super) async fn api_list(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Response, AppError> {
    let min = params.min(&state.web)?;
    let web = state.web.clone();
    let labels = state.labels.clone();
    let body = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        let items = list_items(db, &web, user, hash.as_deref(), now, min)?;
        Ok(serde_json::to_string(&api::ArticleList::new(
            &items, &labels,
        ))?)
    })
    .await?;
    Ok(json(body))
}

/// 検索画面と同じ条件の検索。閲覧ではないので、訪問も開いたことも記録しない。
pub(super) async fn api_search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    let params = Params::from_query(raw.as_deref().unwrap_or(""));
    let web = state.web.clone();
    let labels = state.labels.clone();
    let body = with_db(&state, move |db| {
        let (user, hash) = viewer(db)?;
        let q = params.to_query(user, hash.as_deref(), web.list_limit)?;
        let items = db.search_articles(&q)?;
        Ok(serde_json::to_string(&api::ArticleList::new(
            &items, &labels,
        ))?)
    })
    .await?;
    Ok(json(body))
}

/// 記事 1 件の最新の要約と和訳。閲覧ではないので、開いたことを記録しない。
pub(super) async fn api_detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let labels = state.labels.clone();
    let body = with_db(&state, move |db| {
        let (user, hash) = viewer(db)?;
        let detail = db
            .article_detail(user, hash.as_deref(), id)?
            .ok_or(AppError::NotFound)?;
        Ok(serde_json::to_string(&api::ArticleBody::new(
            &detail, &labels,
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
                {"id": a, "rating": 5, "bookmarked": false, "read": true},
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

    /// API の一覧は既定では Web と同じ記事を出し、`min` で最低点を選べる（0 ですべて）。閲覧としては記録しない。
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
        assert_eq!(a["score"], 90);
        assert_eq!(a["url"], "https://e.com/good?a=1&b=2");

        let (_, json) = server.get_json("/api/articles?min=0").await;
        assert_eq!(json["articles"].as_array().unwrap().len(), 5, "{json}");
        let (_, json) = server.get_json("/api/articles?min=5").await;
        // 低い点（10 点）も出る。評価 2・無関係・未採点は 0 のときだけ
        assert_eq!(json["articles"].as_array().unwrap().len(), 2, "{json}");
        let (status, _) = server.get_json("/api/articles?min=x").await;
        assert_eq!(status, 400);
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
