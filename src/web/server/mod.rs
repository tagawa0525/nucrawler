//! Web UI の HTTP サーバー。画面の描画は `html`、データは `Db` に任せ、ここではルーティングと
//! 反応の記録（詳細・和訳を開いた、評価、既読・ブックマークの印、和訳の依頼）だけを行う。フィードは `feed`、JSON API の応答の形は `api` が決める。

use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Form, Path, Query, RawQuery, State};

use axum::http::{HeaderMap, StatusCode, header};

use axum::response::{Html, IntoResponse, Redirect, Response};

use axum::routing::{get, post};

use chrono::{Duration, Utc};

use crate::config::WebConfig;

use crate::db::{
    Db, DbError, ListQuery, NewReport, OpenKind, Rating, ReportFilter, ReportKind, ReportStatus,
    Visibility,
};

use crate::search::Params;

use crate::web::html::{self, DetailView, Page, SourceLabels};

use crate::web::{api, feed};

mod feedback;
mod glossary;
mod json;
mod notes;
mod pages;
#[cfg(test)]
mod test_support;

use feedback::*;
use glossary::*;
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
        .route("/articles/{id}/rating", post(rating))
        .route("/articles/{id}/bookmark", post(bookmark))
        .route("/articles/{id}/read", post(read))
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
    let hash = db.profile_hash(user)?;
    Ok((user, hash))
}

fn find_article(db: &Db, user: i64, id: i64) -> Result<crate::db::ArticleDetail, AppError> {
    db.article_detail(user, None, id)?.ok_or(AppError::NotFound)
}

/// 記事への書き込み（評価・印・和訳の依頼・指摘・コメント）の後に戻る詳細。`back=1` を付け、詳細はそれを
/// 開いたとは数えない（数えると、外した既読が付き直り、開いた記録も戻るたびに増える）。
/// `translation` なら和訳の表示に戻る。`extra` は足すクエリ（末尾に `&`）、`fragment` は `#` から。
fn back_to_detail(id: i64, translation: bool, extra: &str, fragment: &str) -> Redirect {
    let view = if translation { "view=translation&" } else { "" };
    Redirect::to(&format!("/articles/{id}?{view}{extra}back=1{fragment}"))
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
                .form(&format!("/articles/{id}/rating"), "value=4")
                .header("origin", origin)
                .send()
                .await
                .unwrap();
            assert_eq!(res.status().as_u16(), expected, "{origin}");
        }
        assert_eq!(server.count("SELECT count(*) FROM ratings"), 1);
    }
}
