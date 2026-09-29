//! 記事への操作（👍/👎・ブックマーク・取り消し・和訳の依頼）。

use super::*;

#[derive(serde::Deserialize)]
pub(super) struct FeedbackForm {
    kind: String,
}

pub(super) async fn feedback(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<FeedbackForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    // ブックマークを外すのは行動ではなく状態の変更（ブックマークした行動は残す）
    let kind = match form.kind.as_str() {
        "up" => Some(SignalKind::Up),
        "down" => Some(SignalKind::Down),
        "bookmark" => Some(SignalKind::Bookmark),
        "dismiss" => Some(SignalKind::Dismiss),
        "unbookmark" => None,
        _ => {
            return Err(AppError::BadRequest(
                "kind must be up, down, bookmark, unbookmark or dismiss",
            ));
        }
    };
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        find_article(db, user, id)?;
        match kind {
            Some(kind) => db.record_event(user, id, kind, Utc::now())?,
            None => db.unbookmark(user, id)?,
        }
        Ok(())
    })
    .await?;
    Ok(Redirect::to(&format!("/articles/{id}")))
}

/// 一覧のスワイプの取り消し。その振り分けを無かったことにする。
pub(super) async fn undo_feedback(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<FeedbackForm>,
) -> Result<StatusCode, AppError> {
    check_same_origin(&headers)?;
    let kind = match form.kind.as_str() {
        "bookmark" => SignalKind::Bookmark,
        "dismiss" => SignalKind::Dismiss,
        _ => return Err(AppError::BadRequest("kind must be bookmark or dismiss")),
    };
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        find_article(db, user, id)?;
        Ok(db.undo_event(user, id, kind)?)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
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
    Ok(Redirect::to(&format!("/articles/{id}")))
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
            format!("/articles/{id}")
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

    /// 👍/👎 は評価に置き換えたので、振り分けの受付では受け付けない。
    #[tokio::test]
    async fn feedback_no_longer_takes_thumbs() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        for kind in ["kind=up", "kind=down", "kind=open_detail"] {
            let res = server.post(&format!("/articles/{id}/feedback"), kind).await;
            assert_eq!(res.status().as_u16(), 400, "{kind}");
        }
        assert_eq!(server.count("SELECT count(*) FROM events"), 0);
    }

    /// ブックマークは状態として残り、外せる。「見ない」は行動として記録する。
    #[tokio::test]
    async fn feedback_bookmarks_and_dismisses() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let path = format!("/articles/{id}/feedback");
        let post = |kind: &'static str| server.post(&path, kind);

        assert_eq!(post("kind=bookmark").await.status().as_u16(), 303);
        assert_eq!(server.count("SELECT count(*) FROM bookmarks"), 1);
        assert_eq!(post("kind=unbookmark").await.status().as_u16(), 303);
        assert_eq!(server.count("SELECT count(*) FROM bookmarks"), 0);
        // 外しても、ブックマークした行動は採点のために残る
        assert_eq!(
            server.count("SELECT count(*) FROM events WHERE kind = 'bookmark'"),
            1
        );
        assert_eq!(post("kind=dismiss").await.status().as_u16(), 303);
        assert_eq!(
            server.count("SELECT count(*) FROM events WHERE kind = 'dismiss'"),
            1
        );
    }

    /// スワイプの取り消しは、その行動を無かったことにする。取り消せるのは振り分けだけ。
    #[tokio::test]
    async fn undo_takes_back_a_bookmark_or_dismissal() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let feedback = format!("/articles/{id}/feedback");
        let undo = format!("/articles/{id}/feedback/undo");

        server.post(&feedback, "kind=bookmark").await;
        let res = server.post(&undo, "kind=bookmark").await;
        assert_eq!(res.status().as_u16(), 204);
        server.post(&feedback, "kind=dismiss").await;
        let res = server.post(&undo, "kind=dismiss").await;
        assert_eq!(res.status().as_u16(), 204);
        assert_eq!(server.count("SELECT count(*) FROM events"), 0);
        assert_eq!(server.count("SELECT count(*) FROM bookmarks"), 0);

        server.post(&feedback, "kind=dismiss").await;
        let res = server.post(&undo, "kind=up").await;
        assert_eq!(res.status().as_u16(), 400);
        assert_eq!(server.count("SELECT count(*) FROM events"), 1);

        let res = server
            .form(&undo, "kind=dismiss")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        let res = server
            .post("/articles/999/feedback/undo", "kind=dismiss")
            .await;
        assert_eq!(res.status().as_u16(), 404);
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
