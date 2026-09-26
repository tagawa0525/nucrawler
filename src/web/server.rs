//! Web UI の HTTP サーバー。画面の描画は `html`、データは `Db` に任せ、ここではルーティングと
//! 行動の記録（詳細・和訳を開いた、👍/👎、和訳の依頼）だけを行う。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Lang, WebConfig};
    use crate::db::{ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact};

    /// 英語の記事に本文と digest を付ける。
    fn seed(db: &Db, url: &str, title_ja: &str) -> (i64, i64) {
        let id = db
            .insert_article(&NewArticle {
                source_id: "wnn",
                url,
                title: "Title",
                lang: Lang::En,
                published_at: None,
            })
            .unwrap()
            .unwrap();
        let body = db
            .insert_content(id, ContentKind::Body, ContentOrigin::Page, "body")
            .unwrap();
        let payload = serde_json::json!({
            "title_ja": title_ja, "summary_ja": "要約", "points_ja": ["点"],
            "implications_ja": "", "lwr_relevant": true, "topics": ["規制・審査"],
        });
        let digest = db
            .insert_artifact(
                &NewArtifact {
                    article_id: id,
                    kind: ArtifactKind::Digest,
                    backend: "claude-cli",
                    model: "sonnet",
                    prompt_version: 1,
                    payload: &payload,
                    inputs: &[body],
                },
                chrono::Utc::now(),
            )
            .unwrap();
        (id, digest)
    }

    fn add_translation(db: &Db, article_id: i64) {
        let body = db
            .insert_content(
                article_id,
                ContentKind::Fulltext,
                ContentOrigin::Page,
                "full",
            )
            .unwrap();
        db.insert_translation(
            &NewArtifact {
                article_id,
                kind: ArtifactKind::Translation,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &serde_json::json!({"body_ja": "和訳の本文"}),
                inputs: &[body],
            },
            chrono::Utc::now(),
        )
        .unwrap();
    }

    struct Server {
        base: String,
        state: AppState,
        client: reqwest::Client,
    }

    impl Server {
        async fn start(db: Db) -> Self {
            let state = AppState::new(db, WebConfig::default());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(axum::serve(listener, router(state.clone())).into_future());
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            Self {
                base,
                state,
                client,
            }
        }

        async fn get(&self, path: &str) -> (u16, String) {
            let res = self
                .client
                .get(format!("{}{path}", self.base))
                .send()
                .await
                .unwrap();
            (res.status().as_u16(), res.text().await.unwrap())
        }

        /// フォームの送信（`body` は application/x-www-form-urlencoded）。
        fn form(&self, path: &str, body: &'static str) -> reqwest::RequestBuilder {
            self.client
                .post(format!("{}{path}", self.base))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body)
        }

        async fn post(&self, path: &str, body: &'static str) -> reqwest::Response {
            self.form(path, body).send().await.unwrap()
        }

        fn count(&self, sql: &str) -> i64 {
            self.state.db.lock().unwrap().query_i64(sql).unwrap()
        }
    }

    #[tokio::test]
    async fn list_shows_articles_and_starts_a_visit() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let (status, html) = server.get("/?all=1").await;
        assert_eq!(status, 200);
        assert!(html.contains("見出しA"), "{html}");
        assert_eq!(
            server.count("SELECT count(*) FROM users WHERE last_seen_at IS NOT NULL"),
            1
        );
        // 未採点の記事は既定の一覧には出ない
        let (_, html) = server.get("/").await;
        assert!(!html.contains("見出しA"), "{html}");
    }

    #[tokio::test]
    async fn detail_records_opens_once_per_view() {
        let db = Db::open_in_memory().unwrap();
        let (id, digest) = seed(&db, "https://e.com/a", "見出しA");
        add_translation(&db, id);
        let server = Server::start(db).await;
        let events = |kind: &str| {
            server.count(&format!(
                "SELECT count(*) FROM events WHERE article_id = {id} AND kind = '{kind}'"
            ))
        };

        let (status, html) = server.get(&format!("/articles/{id}")).await;
        assert_eq!(status, 200);
        assert!(html.contains("見出しA"), "{html}");
        assert_eq!(events("open_detail"), 1);
        // 版の切り替えは新たに開いたことにしない
        server.get(&format!("/articles/{id}?digest={digest}")).await;
        assert_eq!(events("open_detail"), 1);

        let (status, html) = server
            .get(&format!("/articles/{id}?view=translation"))
            .await;
        assert_eq!(status, 200);
        assert!(html.contains("和訳の本文"), "{html}");
        assert_eq!(events("open_translation"), 1);
        assert_eq!(events("open_detail"), 1);
    }

    #[tokio::test]
    async fn translation_view_without_translation_is_not_an_open() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let (status, _) = server
            .get(&format!("/articles/{id}?view=translation"))
            .await;
        assert_eq!(status, 200);
        assert_eq!(
            server.count("SELECT count(*) FROM events WHERE kind = 'open_translation'"),
            0
        );
    }

    #[tokio::test]
    async fn unknown_article_is_not_found() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        assert_eq!(server.get("/articles/999").await.0, 404);
        let res = server.post("/articles/999/feedback", "kind=up").await;
        assert_eq!(res.status().as_u16(), 404);
        let res = server.post("/articles/999/translation-request", "").await;
        assert_eq!(res.status().as_u16(), 404);
        assert_eq!(server.count("SELECT count(*) FROM events"), 0);
    }

    #[tokio::test]
    async fn feedback_records_event_and_returns_to_detail() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let res = server
            .post(&format!("/articles/{id}/feedback"), "kind=down")
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}")
        );
        assert_eq!(
            server.count("SELECT count(*) FROM events WHERE kind = 'down'"),
            1
        );
        let res = server
            .post(&format!("/articles/{id}/feedback"), "kind=open_detail")
            .await;
        assert_eq!(res.status().as_u16(), 400);
        assert_eq!(server.count("SELECT count(*) FROM events"), 1);
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

    /// 認証の無いサーバーなので、別のサイトのページから利用者のブラウザ経由で
    /// 行動を書き込まれないようにする。
    #[tokio::test]
    async fn cross_site_posts_are_rejected() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        for (origin, expected) in [("https://evil.example", 403), (server.base.as_str(), 303)] {
            let res = server
                .form(&format!("/articles/{id}/feedback"), "kind=up")
                .header("origin", origin)
                .send()
                .await
                .unwrap();
            assert_eq!(res.status().as_u16(), expected, "{origin}");
        }
        assert_eq!(server.count("SELECT count(*) FROM events"), 1);
    }
}
