//! Web UI の HTTP サーバー。画面の描画は `html`、データは `Db` に任せ、ここではルーティングと
//! 行動の記録（詳細・和訳を開いた、👍/👎、和訳の依頼）だけを行う。フィードは `feed`、JSON API の応答の形は `api` が決める。

use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Form, Path, Query, RawQuery, State};

use axum::http::{HeaderMap, StatusCode, header};

use axum::response::{Html, IntoResponse, Redirect, Response};

use axum::routing::{get, post};

use chrono::{Duration, Utc};

use crate::config::WebConfig;

use crate::db::{
    Db, DbError, ListQuery, NewReport, ReportFilter, ReportKind, ReportStatus, SignalKind,
    Visibility,
};

use crate::search::Params;

use crate::web::html::{self, DetailView, Page, SourceLabels};

use crate::web::{api, feed};

mod feedback;
mod json;
mod notes;
mod pages;
#[cfg(test)]
mod test_support;

use feedback::*;
use json::*;
use notes::*;
use pages::*;

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
    labels: Arc<SourceLabels>,
}

impl AppState {
    pub fn new(db: Db, web: WebConfig, labels: SourceLabels) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            web: Arc::new(web),
            labels: Arc::new(labels),
        }
    }
}

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/", get(list))
        .route("/articles/{id}", get(detail))
        .route("/feed.xml", get(feed))
        .route("/api/articles", get(api_list))
        .route("/api/articles/{id}", get(api_detail))
        .route("/search", get(search))
        .route("/api/search", get(api_search))
        .route("/articles/{id}/feedback", post(feedback))
        .route("/articles/{id}/feedback/undo", post(undo_feedback))
        .route(
            "/articles/{id}/translation-request",
            post(translation_request),
        )
        .route("/articles/{id}/report", post(add_report))
        .route("/articles/{id}/comments", post(add_comment))
        .route("/comments/{id}", post(update_comment))
        .route("/comments/{id}/delete", post(delete_comment))
        .route("/settings", get(settings))
        .route("/glossary", get(glossary).post(add_glossary_term))
        .route("/glossary/{id}", post(update_glossary_term))
        .route("/glossary/{id}/delete", post(delete_glossary_term))
        .route("/reports", get(reports))
        .route("/reports/{id}", post(resolve_report))
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
    #[error("failed to encode json")]
    Json(#[from] serde_json::Error),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(&'static str),
    #[error("cross-site request")]
    CrossSite,
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    InvalidSearch(#[from] crate::search::SearchError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            AppError::Db(_) | AppError::Join(_) | AppError::Json(_) => {
                // 詳細（SQL やスキーマ）はログにだけ残し、応答には出さない
                tracing::error!("{}", crate::errors::error_chain(&self));
                let status = StatusCode::INTERNAL_SERVER_ERROR;
                return (status, "internal server error").into_response();
            }
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) | AppError::InvalidSearch(_) => StatusCode::BAD_REQUEST,
            AppError::CrossSite => StatusCode::FORBIDDEN,
            AppError::Conflict(_) => StatusCode::CONFLICT,
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

async fn glossary(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let labels = state.labels.clone();
    let page = with_db(&state, move |db| {
        let entries = db.glossary_entries()?;
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
        };
        Ok(html::glossary_page(&entries, &page))
    })
    .await?;
    Ok(Html(page))
}

#[derive(serde::Deserialize)]
struct GlossaryForm {
    // 欄が無いときも空と同じく検証で 400 にする
    #[serde(default)]
    target: String,
    #[serde(default)]
    abbr: String,
    #[serde(default)]
    note: String,
    /// 1 行に 1 つ
    #[serde(default)]
    sources: String,
}

impl GlossaryForm {
    /// 空白を除き、空の行と大文字小文字だけ違う重複の原語を落とす。訳語と原語は必須。
    fn into_term(self) -> Result<crate::glossary::Term, AppError> {
        let filled = |s: &str| Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
        let target =
            filled(&self.target).ok_or(AppError::BadRequest("target must not be empty"))?;
        let mut sources: Vec<String> = Vec::new();
        for source in self.sources.lines().filter_map(filled) {
            if !sources.iter().any(|s| s.eq_ignore_ascii_case(&source)) {
                sources.push(source);
            }
        }
        if sources.is_empty() {
            return Err(AppError::BadRequest("sources must not be empty"));
        }
        Ok(crate::glossary::Term {
            sources,
            target,
            abbr: filled(&self.abbr),
            note: filled(&self.note),
        })
    }
}

/// 訳語集の重なりは、どの訳語と重なったかを利用者に返す。
fn glossary_error(e: DbError) -> AppError {
    match e {
        DbError::GlossaryConflict(message) => AppError::Conflict(message),
        e => e.into(),
    }
}

async fn add_glossary_term(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<GlossaryForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let term = form.into_term()?;
    let id = with_db(&state, move |db| {
        db.add_glossary_term(&term, Utc::now())
            .map_err(glossary_error)
    })
    .await?;
    Ok(Redirect::to(&format!("/glossary#term-{id}")))
}

async fn update_glossary_term(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<GlossaryForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let term = form.into_term()?;
    let found = with_db(&state, move |db| {
        db.update_glossary_term(id, &term, Utc::now())
            .map_err(glossary_error)
    })
    .await?;
    if !found {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to(&format!("/glossary#term-{id}")))
}

async fn delete_glossary_term(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let found = with_db(&state, move |db| Ok(db.delete_glossary_term(id)?)).await?;
    if !found {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to("/glossary"))
}

fn find_article(db: &Db, user: i64, id: i64) -> Result<crate::db::ArticleDetail, AppError> {
    db.article_detail(user, None, id)?.ok_or(AppError::NotFound)
}

/// 認証の無いサーバーなので、別のサイトのページから利用者のブラウザ経由で書き込まれないよう、
/// ブラウザが付ける Origin がこのサーバー自身（http で待ち受けているので `http://` + Host）で
/// なければ拒否する（Origin の無い curl などは通す）。
fn check_same_origin(headers: &HeaderMap) -> Result<(), AppError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let origin_host = origin.to_str().ok().and_then(|o| o.strip_prefix("http://"));
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    match (origin_host, host) {
        (Some(o), Some(h)) if o == h => Ok(()),
        _ => Err(AppError::CrossSite),
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Db;
    use crate::web::server::test_support::*;

    /// 原語は 1 行に 1 つ。空の行と、大文字小文字だけ違う重複は除く。
    #[tokio::test]
    async fn glossary_terms_are_added_updated_and_deleted() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let sources = "SELECT group_concat(s.source, '|') FROM glossary_sources AS s
                       JOIN glossary_terms AS t ON t.id = s.term_id WHERE t.abbr = 'EDG'";
        let res = server
            .post(
                "/glossary",
                "target=%E9%9D%9E%E5%B8%B8%E7%94%A8DG&abbr=+EDG+&note=&sources=emergency+diesel+generator%0D%0AEDG%0D%0A+%0D%0Aedg",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        let id = server.count("SELECT id FROM glossary_terms WHERE abbr = 'EDG'");
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/glossary#term-{id}")
        );
        assert_eq!(server.strings(sources), ["emergency diesel generator|EDG"]);
        assert_eq!(
            server.count("SELECT count(*) FROM glossary_terms WHERE abbr = 'EDG' AND note IS NULL"),
            1
        );

        let path = format!("/glossary/{id}");
        let res = server
            .post(
                &path,
                "target=%E9%9D%9E%E5%B8%B8%E7%94%A8DG&abbr=EDG&note=&sources=EDG",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(server.strings(sources), ["EDG"]);

        let delete = format!("/glossary/{id}/delete");
        let res = server.post(&delete, "").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"].to_str().unwrap(), "/glossary");
        assert_eq!(
            server.count("SELECT count(*) FROM glossary_terms WHERE abbr = 'EDG'"),
            0
        );
        assert_eq!(server.post(&delete, "").await.status().as_u16(), 404);
        assert_eq!(
            server
                .post(&path, "target=x&sources=x")
                .await
                .status()
                .as_u16(),
            404
        );
    }

    #[tokio::test]
    async fn glossary_rejects_invalid_or_conflicting_terms() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let before = server.count("SELECT count(*) FROM glossary_sources");
        // 訳語と原語は必須
        assert_eq!(
            server
                .post("/glossary", "target=+&sources=x")
                .await
                .status()
                .as_u16(),
            400
        );
        assert_eq!(
            server
                .post("/glossary", "target=x&sources=%0D%0A+")
                .await
                .status()
                .as_u16(),
            400
        );
        assert_eq!(
            server.post("/glossary", "target=x").await.status().as_u16(),
            400
        );
        // ほかの訳語の原語は使えず、どの訳語のものかを返す
        let res = server.post("/glossary", "target=x&sources=atf").await;
        assert_eq!(res.status().as_u16(), 409);
        assert!(res.text().await.unwrap().contains("事故耐性燃料"));
        let res = server
            .form("/glossary", "target=x&sources=x")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        assert_eq!(
            server.count("SELECT count(*) FROM glossary_sources"),
            before
        );
    }

    /// 内部エラーの詳細（SQL やスキーマ）は応答に出さず、ログにだけ残す。
    #[tokio::test]
    async fn internal_errors_do_not_leak_details() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        db.conn().execute_batch("DROP TABLE events").unwrap();
        let server = Server::start(db).await;
        let (status, body) = server.get(&format!("/articles/{id}")).await;
        assert_eq!(status, 500);
        assert!(
            !body.contains("events") && !body.contains("table"),
            "{body}"
        );
    }

    /// 認証の無いサーバーなので、別のサイトのページから利用者のブラウザ経由で
    /// 行動を書き込まれないようにする。
    #[tokio::test]
    async fn cross_site_posts_are_rejected() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        // http で待ち受けているので、同じホストでも https のページは別のオリジン
        let https_same_host = server.base.replacen("http://", "https://", 1);
        for (origin, expected) in [
            ("https://evil.example", 403),
            ("null", 403),
            (https_same_host.as_str(), 403),
            (server.base.as_str(), 303),
        ] {
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
