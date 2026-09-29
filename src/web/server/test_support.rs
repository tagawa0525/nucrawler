//! サーバーのテストで共有する補助。

use super::*;
use crate::config::{Lang, WebConfig};
use crate::db::{ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact, Rating};

/// 英語の記事に本文と digest を付ける。
pub(super) fn seed(db: &Db, url: &str, title_ja: &str) -> (i64, i64) {
    seed_with(db, url, title_ja, true)
}

pub(super) fn seed_with(db: &Db, url: &str, title_ja: &str, lwr_relevant: bool) -> (i64, i64) {
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
        "implications_ja": "", "lwr_relevant": lwr_relevant, "topics": ["規制・審査"],
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
                glossary_at: None,
            },
            chrono::Utc::now(),
        )
        .unwrap();
    (id, digest)
}

/// 所有者の現在のプロファイルで digest を採点する。
pub(super) fn score(db: &Db, digest: i64, score: u8) {
    let owner = db.owner_id().unwrap();
    let profile = crate::profile::Profile {
        interests: vec![],
        exclude: vec![],
    };
    db.save_profile(owner, &profile, chrono::Utc::now())
        .unwrap();
    let hash = crate::profile::hash(&profile);
    let key = crate::db::ScoreKey {
        user_id: owner,
        profile_hash: &hash,
        backend: "claude-cli",
        model: "sonnet",
        prompt_version: 1,
    };
    db.insert_score(key, digest, score, Some("理由"), chrono::Utc::now())
        .unwrap();
}

/// 既定の一覧に出る記事 1 件と、出ない記事（低い点、👎、軽水炉と無関係、未採点）。
/// 出る記事の ID を返す。
pub(super) fn seed_recommended_and_hidden(db: &Db) -> i64 {
    let (good, digest) = seed(db, "https://e.com/good?a=1&b=2", "A&B <C>\u{1}");
    score(db, digest, 90);
    let (_, digest) = seed(db, "https://e.com/low", "低い点");
    score(db, digest, 10);
    let (down, digest) = seed(db, "https://e.com/down", "評価 2");
    score(db, digest, 90);
    db.rate(db.owner_id().unwrap(), down, Rating::new(2), Utc::now())
        .unwrap();
    let (_, digest) = seed_with(db, "https://e.com/unrelated", "無関係", false);
    score(db, digest, 90);
    seed(db, "https://e.com/unscored", "未採点");
    good
}

pub(super) fn add_translation(db: &Db, article_id: i64) {
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
            glossary_at: None,
        },
        chrono::Utc::now(),
    )
    .unwrap();
}

pub(super) struct Server {
    pub(super) base: String,
    pub(super) state: AppState,
    pub(super) client: reqwest::Client,
}

impl Server {
    pub(super) async fn start(db: Db) -> Self {
        let state = AppState::new(db, WebConfig::default(), SourceLabels::new());
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

    pub(super) async fn get(&self, path: &str) -> (u16, String) {
        let res = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap();
        (res.status().as_u16(), res.text().await.unwrap())
    }

    /// フォームの送信（`body` は application/x-www-form-urlencoded）。
    pub(super) fn form(&self, path: &str, body: &'static str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
    }

    pub(super) async fn post(&self, path: &str, body: &'static str) -> reqwest::Response {
        self.form(path, body).send().await.unwrap()
    }

    pub(super) fn count(&self, sql: &str) -> i64 {
        self.state.db.lock().unwrap().query_i64(sql).unwrap()
    }

    pub(super) fn strings(&self, sql: &str) -> Vec<String> {
        self.state.db.lock().unwrap().query_strings(sql).unwrap()
    }

    /// 閲覧の行動（開いた記録・既読・訪問の区切り）が 1 つも記録されていない。
    pub(super) fn assert_no_views(&self) {
        assert_eq!(
            self.count("SELECT count(*) FROM events WHERE kind LIKE 'open_%'"),
            0
        );
        // 評価した記事は既読になるので、種の評価で付いた既読は除く
        assert_eq!(
            self.count(
                "SELECT count(*) FROM reads AS rd WHERE NOT EXISTS (
                   SELECT 1 FROM ratings AS rt
                   WHERE rt.user_id = rd.user_id AND rt.article_id = rd.article_id)"
            ),
            0
        );
        assert_eq!(
            self.count("SELECT count(*) FROM users WHERE last_seen_at IS NOT NULL"),
            0
        );
    }

    pub(super) async fn get_with_type(&self, path: &str) -> (u16, String, String) {
        let res = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap();
        let content_type = res
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (
            res.status().as_u16(),
            content_type,
            res.text().await.unwrap(),
        )
    }

    pub(super) async fn get_json(&self, path: &str) -> (u16, serde_json::Value) {
        let (status, content_type, body) = self.get_with_type(path).await;
        if status != 200 {
            return (status, serde_json::Value::Null);
        }
        assert!(
            content_type.starts_with("application/json"),
            "{content_type}"
        );
        (status, serde_json::from_str(&body).unwrap())
    }
}
