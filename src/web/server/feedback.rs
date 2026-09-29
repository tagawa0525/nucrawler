//! 記事への操作（評価・ブックマーク・既読・和訳の依頼）。

use super::*;

#[derive(serde::Deserialize)]
pub(super) struct RatingForm {
    value: String,
}

/// 評価を付ける（1〜5）。空の値なら評価なしに戻す。
pub(super) async fn rating(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<RatingForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let rating = match form.value.as_str() {
        "" => None,
        value => Some(
            value
                .parse()
                .ok()
                .and_then(Rating::new)
                .ok_or(AppError::BadRequest("value must be 1..=5 or empty"))?,
        ),
    };
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        find_article(db, user, id)?;
        Ok(db.rate(user, id, rating, Utc::now())?)
    })
    .await?;
    Ok(back_to_detail(id, false, "", ""))
}

#[derive(serde::Deserialize)]
pub(super) struct MarkForm {
    on: String,
}

impl MarkForm {
    /// `on=1` で付け、`on=0` で外す。
    fn on(&self) -> Result<bool, AppError> {
        match self.on.as_str() {
            "1" => Ok(true),
            "0" => Ok(false),
            _ => Err(AppError::BadRequest("on must be 1 or 0")),
        }
    }
}

/// ブックマーク（後で読む）の印を付け外しする。
pub(super) async fn bookmark(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<MarkForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let on = form.on()?;
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        find_article(db, user, id)?;
        Ok(db.set_bookmark(user, id, on, Utc::now())?)
    })
    .await?;
    Ok(back_to_detail(id, false, "", ""))
}

/// 既読の印を付け外しする。
pub(super) async fn read(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<MarkForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let on = form.on()?;
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        find_article(db, user, id)?;
        Ok(db.set_read(user, id, on, Utc::now())?)
    })
    .await?;
    Ok(back_to_detail(id, false, "", ""))
}

pub(super) async fn translation_request(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        let detail = find_article(db, user, id)?;
        if !detail.can_request_translation() {
            return Err(AppError::BadRequest(
                "only english articles with a public body can be translated",
            ));
        }
        Ok(db.request_translation(user, id, Utc::now())?)
    })
    .await?;
    Ok(back_to_detail(id, false, "", ""))
}

#[cfg(test)]
mod tests {
    use crate::config::Lang;
    use crate::db::{ContentKind, ContentOrigin, Db, NewArticle};
    use crate::web::server::test_support::*;

    /// 評価は 1〜5 で付け直せ、空の値で評価なしに戻る。付けたら詳細に戻る。
    #[tokio::test]
    async fn rating_is_recorded_and_returns_to_detail() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let path = format!("/articles/{id}/rating");
        let res = server.post(&path, "value=2").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}?back=1")
        );
        server.post(&path, "value=5").await;
        assert_eq!(
            server.strings("SELECT CAST(value AS TEXT) FROM ratings"),
            ["5"]
        );
        for bad in ["value=0", "value=6", "value=x"] {
            let res = server.post(&path, bad).await;
            assert_eq!(res.status().as_u16(), 400, "{bad}");
        }
        assert_eq!(
            server.strings("SELECT CAST(value AS TEXT) FROM ratings"),
            ["5"]
        );
        assert_eq!(server.post(&path, "value=").await.status().as_u16(), 303);
        assert_eq!(server.count("SELECT count(*) FROM ratings"), 0);
        let res = server.post("/articles/999/rating", "value=3").await;
        assert_eq!(res.status().as_u16(), 404);
    }

    /// 既読とブックマークは印として付け外しする。付けたら詳細に戻る。
    #[tokio::test]
    async fn read_and_bookmark_marks_are_toggled() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        for (mark, table) in [("bookmark", "bookmarks"), ("read", "reads")] {
            let path = format!("/articles/{id}/{mark}");
            let res = server.post(&path, "on=1").await;
            assert_eq!(res.status().as_u16(), 303, "{mark}");
            assert_eq!(
                res.headers()["location"].to_str().unwrap(),
                format!("/articles/{id}?back=1")
            );
            assert_eq!(server.count(&format!("SELECT count(*) FROM {table}")), 1);
            assert_eq!(server.post(&path, "on=x").await.status().as_u16(), 400);
            assert_eq!(server.post(&path, "on=0").await.status().as_u16(), 303);
            assert_eq!(server.count(&format!("SELECT count(*) FROM {table}")), 0);
            let res = server
                .form(&path, "on=1")
                .header("origin", "https://evil.example")
                .send()
                .await
                .unwrap();
            assert_eq!(res.status().as_u16(), 403, "{mark}");
            let res = server.post(&format!("/articles/999/{mark}"), "on=1").await;
            assert_eq!(res.status().as_u16(), 404, "{mark}");
        }
        // 行動としては記録しない
        assert_eq!(server.count("SELECT count(*) FROM events"), 0);
    }

    /// 振り分け（見送り・取り消し）の受付は、印の付け外しに置き換えた。
    #[tokio::test]
    async fn triage_endpoints_are_gone() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        for path in ["feedback", "feedback/undo"] {
            let res = server
                .post(&format!("/articles/{id}/{path}"), "kind=bookmark")
                .await;
            assert_eq!(res.status().as_u16(), 404, "{path}");
        }
    }

    #[tokio::test]
    async fn translation_request_is_recorded() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let res = server
            .post(&format!("/articles/{id}/translation-request"), "")
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            server.count("SELECT count(*) FROM translation_requests WHERE done_at IS NULL"),
            1
        );
        let (_, html) = server.get(&format!("/articles/{id}")).await;
        assert!(html.contains("和訳待ち"), "{html}");
    }

    /// 和訳の処理が拾えない記事（日本語、公開の本文が無い英語）への依頼は受け付けない。
    /// 受け付けると「和訳待ち」のまま永久に残る。
    #[tokio::test]
    async fn untranslatable_articles_cannot_be_requested() {
        let db = Db::open_in_memory().unwrap();
        let article = |url, lang| {
            db.insert_article(&NewArticle {
                source_id: "wnn",
                url,
                title: "Title",
                lang,
                published_at: None,
            })
            .unwrap()
            .unwrap()
        };
        let ja = article("https://e.com/ja", Lang::Ja);
        db.insert_content(ja, ContentKind::Body, ContentOrigin::Page, "本文")
            .unwrap();
        let no_body = article("https://e.com/no-body", Lang::En);
        let server = Server::start(db).await;
        for id in [ja, no_body] {
            let res = server
                .post(&format!("/articles/{id}/translation-request"), "")
                .await;
            assert_eq!(res.status().as_u16(), 400, "{id}");
        }
        assert_eq!(server.count("SELECT count(*) FROM translation_requests"), 0);
    }
}
