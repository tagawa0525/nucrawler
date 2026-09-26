//! Web UI の HTTP サーバー。画面の描画は `html`、データは `Db` に任せ、ここではルーティングと
//! 行動の記録（詳細・和訳を開いた、👍/👎、和訳の依頼）だけを行う。

use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{Duration, Utc};

use crate::config::WebConfig;
use crate::db::{Db, DbError, ListQuery, SignalKind};
use crate::web::html::{self, DetailView};

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("failed to listen on {addr}")]
    Bind {
        addr: std::net::SocketAddr,
        source: std::io::Error,
    },
    #[error("web server failed")]
    Io(#[source] std::io::Error),
}

/// ハンドラで共有する状態。`Db` は同期 API なので、`spawn_blocking` の中で 1 つずつ使う。
#[derive(Clone)]
pub struct AppState {
    db: Arc<Mutex<Db>>,
    web: Arc<WebConfig>,
}

impl AppState {
    pub fn new(db: Db, web: WebConfig) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            web: Arc::new(web),
        }
    }
}

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/", get(list))
        .route("/articles/{id}", get(detail))
        .route("/articles/{id}/feedback", post(feedback))
        .route(
            "/articles/{id}/translation-request",
            post(translation_request),
        )
        .with_state(state)
}

/// `shutdown` が完了するまで待ち受ける。
pub async fn run(
    addr: std::net::SocketAddr,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    tracing::info!(%addr, "serving the web ui");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(ServeError::Io)
}

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("a database task failed")]
    Join(#[from] tokio::task::JoinError),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(&'static str),
    #[error("cross-site request")]
    CrossSite,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            AppError::Db(_) | AppError::Join(_) => {
                tracing::error!("{}", crate::errors::error_chain(&self));
                StatusCode::INTERNAL_SERVER_ERROR
            }
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::CrossSite => StatusCode::FORBIDDEN,
        };
        (status, self.to_string()).into_response()
    }
}

/// DB の処理をブロッキング用のスレッドで行う。
async fn with_db<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&Db) -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || {
        // 前のハンドラが panic しても、書き込み途中のトランザクションは drop で巻き戻っている
        let db = db.lock().unwrap_or_else(PoisonError::into_inner);
        f(&db)
    })
    .await?
}

/// 利用者（今は所有者だけ）と、現在のプロファイルのハッシュ。
fn viewer(db: &Db) -> Result<(i64, Option<String>), DbError> {
    let user = db.owner_id()?;
    let hash = db.load_profile(user)?.map(|(_, hash)| hash);
    Ok((user, hash))
}

/// 警告は直近 24 時間のものだけ出す。
fn warnings(db: &Db) -> Result<Vec<crate::db::Warning>, DbError> {
    db.warnings(Utc::now() - Duration::hours(24))
}

#[derive(serde::Deserialize)]
struct ListParams {
    all: Option<String>,
}

async fn list(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Html<String>, AppError> {
    let show_all = params.all.as_deref() == Some("1");
    let web = state.web.clone();
    let page = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        let boundary =
            db.begin_visit(user, now, Duration::minutes(web.visit_gap_minutes.into()))?;
        let items = db.list_articles(ListQuery {
            user_id: user,
            profile_hash: hash.as_deref(),
            min_score: web.min_score,
            since: now - Duration::days(web.list_days.into()),
            show_all,
            limit: web.list_limit,
        })?;
        let (new, earlier) = html::split_sections(items, boundary.as_deref());
        Ok(html::list_page(&new, &earlier, show_all, &warnings(db)?))
    })
    .await?;
    Ok(Html(page))
}

#[derive(serde::Deserialize)]
struct DetailParams {
    view: Option<String>,
    digest: Option<i64>,
    translation: Option<i64>,
}

async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<DetailParams>,
) -> Result<Html<String>, AppError> {
    let view = DetailView {
        digest: params.digest,
        show_translation: params.view.as_deref() == Some("translation"),
        translation: params.translation,
    };
    let page = with_db(&state, move |db| {
        let (user, hash) = viewer(db)?;
        let detail = db
            .article_detail(user, hash.as_deref(), id)?
            .ok_or(AppError::NotFound)?;
        // 開いたことだけを記録し、版の切り替えは数えない（同じ記事の反応が重なると
        // 採点に渡す直近の反応が偏る）
        let opened = if view.show_translation {
            (view.translation.is_none() && !detail.translations.is_empty())
                .then_some(SignalKind::OpenTranslation)
        } else {
            view.digest.is_none().then_some(SignalKind::OpenDetail)
        };
        if let Some(kind) = opened {
            db.record_event(user, id, kind, Utc::now())?;
        }
        Ok(html::detail_page(&detail, view, &warnings(db)?))
    })
    .await?;
    Ok(Html(page))
}

#[derive(serde::Deserialize)]
struct FeedbackForm {
    kind: String,
}

async fn feedback(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<FeedbackForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let kind = match form.kind.as_str() {
        "up" => SignalKind::Up,
        "down" => SignalKind::Down,
        _ => return Err(AppError::BadRequest("kind must be up or down")),
    };
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        ensure_article(db, user, id)?;
        Ok(db.record_event(user, id, kind, Utc::now())?)
    })
    .await?;
    Ok(Redirect::to(&format!("/articles/{id}")))
}

async fn translation_request(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        ensure_article(db, user, id)?;
        Ok(db.request_translation(user, id, Utc::now())?)
    })
    .await?;
    Ok(Redirect::to(&format!("/articles/{id}")))
}

fn ensure_article(db: &Db, user: i64, id: i64) -> Result<(), AppError> {
    db.article_detail(user, None, id)?
        .map(drop)
        .ok_or(AppError::NotFound)
}

/// 認証の無いサーバーなので、別のサイトのページから利用者のブラウザ経由で書き込まれないよう、
/// ブラウザが付ける Origin がこのサーバー自身でなければ拒否する（Origin の無い curl などは通す）。
fn check_same_origin(headers: &HeaderMap) -> Result<(), AppError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    let origin_host = origin.to_str().ok().and_then(|o| {
        o.strip_prefix("http://")
            .or_else(|| o.strip_prefix("https://"))
    });
    match (origin_host, host) {
        (Some(o), Some(h)) if o == h => Ok(()),
        _ => Err(AppError::CrossSite),
    }
}

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
