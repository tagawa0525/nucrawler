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
mod pages;
#[cfg(test)]
mod test_support;

use feedback::*;
use json::*;
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

#[derive(serde::Deserialize)]
struct ReportForm {
    #[serde(default)]
    kind: String,
    // 欄が無いときも空と同じく検証で 400 にする（無いと取り出しの段階で 422 になる）
    #[serde(default)]
    found: String,
    #[serde(default)]
    wanted: String,
    #[serde(default)]
    source: String,
    /// 訳語の指摘ではメモ、ほかの種類では内容
    #[serde(default)]
    note: String,
    /// 和訳を読んでいたなら `translation`（戻る先）
    view: Option<String>,
}

/// 指摘を受付箱に入れ、読んでいた画面の指摘の欄へ戻る。訳語の指摘は気になった訳が、
/// ほかの種類は内容が必須。
async fn add_report(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<ReportForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let kind = ReportKind::parse(&form.kind).ok_or(AppError::BadRequest(
        "kind must be term, translation, digest, topic, body or other",
    ))?;
    let filled = |s: &str| Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
    let (found, wanted, source, note) = (
        filled(&form.found),
        filled(&form.wanted),
        filled(&form.source),
        filled(&form.note),
    );
    if kind == ReportKind::Term && found.is_none() {
        return Err(AppError::BadRequest("found must not be empty"));
    }
    if kind != ReportKind::Term && note.is_none() {
        return Err(AppError::BadRequest("note must not be empty"));
    }
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        find_article(db, user, id)?;
        let report = match (kind, found.as_deref(), note.as_deref()) {
            (ReportKind::Term, Some(found), note) => NewReport::Term {
                found,
                wanted: wanted.as_deref(),
                source: source.as_deref(),
                note,
            },
            (kind, _, Some(body)) => NewReport::Other { kind, body },
            _ => unreachable!("required fields are checked above"),
        };
        Ok(db.add_report(user, id, &report, Utc::now())?)
    })
    .await?;
    let view = if form.view.as_deref() == Some("translation") {
        "view=translation&"
    } else {
        ""
    };
    Ok(Redirect::to(&format!(
        "/articles/{id}?{view}reported=1#reports"
    )))
}

#[derive(serde::Deserialize)]
struct CommentForm {
    // 欄が無いときも空と同じく検証で 400 にする
    #[serde(default)]
    body: String,
    /// チェックしたときだけ `1`（公開）。無ければ非公開
    public: Option<String>,
    /// 和訳を読んでいたなら `translation`（戻る先）
    view: Option<String>,
}

impl CommentForm {
    fn body(&self) -> Result<String, AppError> {
        Some(self.body.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or(AppError::BadRequest("body must not be empty"))
    }

    /// `1` なら公開、欄が無ければ非公開。ほかの値は公開範囲を取り違えないよう拒否する。
    fn visibility(&self) -> Result<Visibility, AppError> {
        match self.public.as_deref() {
            None => Ok(Visibility::Private),
            Some("1") => Ok(Visibility::Public),
            Some(_) => Err(AppError::BadRequest("public must be 1 or absent")),
        }
    }
}

/// 記事のコメントの欄へ戻る。和訳を読んでいたなら和訳のまま。
fn back_to_comments(article_id: i64, view: Option<&str>) -> Redirect {
    let view = if view == Some("translation") {
        "?view=translation"
    } else {
        ""
    };
    Redirect::to(&format!("/articles/{article_id}{view}#comments"))
}

async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<CommentForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let (body, visibility) = (form.body()?, form.visibility()?);
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        find_article(db, user, id)?;
        Ok(db.add_comment(user, id, &body, visibility, Utc::now())?)
    })
    .await?;
    Ok(back_to_comments(id, form.view.as_deref()))
}

/// 自分のコメントを直す。他人のコメントは無いものとして扱う。
async fn update_comment(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<CommentForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let (body, visibility) = (form.body()?, form.visibility()?);
    let article = with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        Ok(db.update_comment(user, id, &body, visibility, Utc::now())?)
    })
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(back_to_comments(article, form.view.as_deref()))
}

#[derive(serde::Deserialize)]
struct CommentDeleteForm {
    view: Option<String>,
}

/// 自分のコメントを消す。他人のコメントは無いものとして扱う。
async fn delete_comment(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<CommentDeleteForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let article = with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        Ok(db.delete_comment(user, id)?)
    })
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(back_to_comments(article, form.view.as_deref()))
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

/// 受付箱の絞り込み。状況が無ければ受付中、`all` ならすべて。種類が無ければすべて。
fn report_filter(status: Option<&str>, kind: Option<&str>) -> Result<ReportFilter, AppError> {
    let status = match status {
        None => Some(ReportStatus::Pending),
        Some("all") => None,
        Some(s) => Some(ReportStatus::parse(s).ok_or(AppError::BadRequest(
            "status must be pending, added, existing, done, rejected or all",
        ))?),
    };
    let kind = kind
        .map(|k| {
            ReportKind::parse(k).ok_or(AppError::BadRequest(
                "kind must be term, translation, digest, topic, body or other",
            ))
        })
        .transpose()?;
    Ok(ReportFilter {
        status,
        kind,
        article_id: None,
    })
}

#[derive(serde::Deserialize)]
struct ReportsParams {
    status: Option<String>,
    kind: Option<String>,
}

async fn reports(
    State(state): State<AppState>,
    Query(params): Query<ReportsParams>,
) -> Result<Html<String>, AppError> {
    let filter = report_filter(params.status.as_deref(), params.kind.as_deref())?;
    let labels = state.labels.clone();
    let page = with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        let reports = db.reports(user, &filter)?;
        let counts = db.report_counts()?;
        let terms = db.glossary_entries()?;
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
        };
        Ok(html::reports_page(
            &reports, &counts, &filter, &terms, &page,
        ))
    })
    .await?;
    Ok(Html(page))
}

#[derive(serde::Deserialize)]
struct ResolveReportForm {
    #[serde(default)]
    status: String,
    /// 結び付ける訳語の id（空なら無し）
    #[serde(default)]
    term_id: String,
    #[serde(default)]
    reply: String,
    /// 戻る先の絞り込み
    back_status: Option<String>,
    back_kind: Option<String>,
}

/// 指摘の対応状況を変え、受付箱の同じ絞り込みへ戻る。付けられる状況は種類で決まり、
/// 訳語を結び付けられるのは訳語の指摘だけ。
async fn resolve_report(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<ResolveReportForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let status = ReportStatus::parse(&form.status).ok_or(AppError::BadRequest(
        "status must be pending, added, existing, done or rejected",
    ))?;
    let term_id = match form.term_id.trim() {
        "" => None,
        s => Some(
            s.parse::<i64>()
                .map_err(|_| AppError::BadRequest("term_id must be a number"))?,
        ),
    };
    let reply = Some(form.reply.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // 戻る先が読めなければ既定（受付中）に戻す
    let back =
        report_filter(form.back_status.as_deref(), form.back_kind.as_deref()).unwrap_or_default();
    with_db(&state, move |db| {
        let kind = db.report_kind(id)?.ok_or(AppError::NotFound)?;
        if !ReportStatus::for_kind(kind).contains(&status) {
            return Err(AppError::BadRequest("the status does not fit the report"));
        }
        if let Some(term_id) = term_id {
            if kind != ReportKind::Term {
                return Err(AppError::BadRequest("only term reports link a term"));
            }
            if !db.glossary_entries()?.iter().any(|e| e.id == term_id) {
                return Err(AppError::BadRequest("unknown term_id"));
            }
        }
        db.resolve_report(id, status, term_id, reply.as_deref(), Utc::now())?;
        Ok(())
    })
    .await?;
    Ok(Redirect::to(&html::reports_href(&back)))
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

    /// 訳語の指摘は受付箱に入り、空の欄は記録しない。読んでいた画面の指摘の欄に戻る。
    #[tokio::test]
    async fn term_report_is_recorded_and_returns_to_detail() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let path = format!("/articles/{id}/report");
        let res = server
            .post(
                &path,
                "kind=term&found=%E7%B5%A6%E6%B2%B9%E5%81%9C%E6%AD%A2&wanted=&source=+refueling+outage+&note=&view=translation",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}?view=translation&reported=1#reports")
        );
        assert_eq!(
            server.count(&format!(
                "SELECT count(*) FROM reports
                 WHERE article_id = {id} AND kind = 'term' AND found = '給油停止' AND wanted IS NULL
                   AND source = 'refueling outage' AND note IS NULL AND resolved_at IS NULL"
            )),
            1
        );
        let res = server.post(&path, "kind=term&found=x").await;
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}?reported=1#reports")
        );
        let (_, html) = server.get(&format!("/articles/{id}?reported=1")).await;
        assert!(html.contains("指摘を受け付けました"), "{html}");

        // 気になった訳は必須（欄が無くても空でも同じ）
        assert_eq!(
            server
                .post(&path, "kind=term&wanted=a")
                .await
                .status()
                .as_u16(),
            400
        );
        assert_eq!(
            server
                .post(&path, "kind=term&found=+&wanted=a")
                .await
                .status()
                .as_u16(),
            400
        );
        let res = server
            .form(&path, "kind=term&found=x")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        let res = server
            .post("/articles/999/report", "kind=term&found=x")
            .await;
        assert_eq!(res.status().as_u16(), 404);
        assert_eq!(server.count("SELECT count(*) FROM reports"), 2);
    }

    /// 訳語以外の指摘は種類と内容だけを記録し、内容は必須。
    #[tokio::test]
    async fn other_reports_are_recorded_with_their_kind() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let path = format!("/articles/{id}/report");
        let res = server
            .post(
                &path,
                "kind=body&note=+%E5%BE%8C%E5%8D%8A%E3%81%8C%E7%84%A1%E3%81%84+&found=x",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            server.strings("SELECT kind || '|' || note || '|' || (found IS NULL) FROM reports"),
            ["body|後半が無い|1"]
        );
        for body in [
            "kind=body&note=+",
            "kind=body",
            "kind=bogus&note=x",
            "note=x",
        ] {
            assert_eq!(
                server.post(&path, body).await.status().as_u16(),
                400,
                "{body}"
            );
        }
        assert_eq!(server.count("SELECT count(*) FROM reports"), 1);
    }

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

    /// 受付箱は既定で受付中だけを出し、対応すると受付中から外れて記事の詳細に状況が出る。
    #[tokio::test]
    async fn reports_are_listed_and_resolved() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        server
            .post(
                &format!("/articles/{id}/report"),
                "kind=term&found=%E7%B5%A6%E6%B2%B9%E5%81%9C%E6%AD%A2",
            )
            .await;
        let report = server.count("SELECT id FROM reports");
        let (_, html) = server.get("/settings").await;
        assert!(html.contains("受付中 1 件"), "{html}");
        let (status, html) = server.get("/reports").await;
        assert_eq!(status, 200);
        assert!(
            html.contains("給油停止") && html.contains("見出しA"),
            "{html}"
        );

        let res = server
            .post(
                &format!("/reports/{report}"),
                "status=added&term_id=1&reply=&back_status=pending",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"].to_str().unwrap(), "/reports");
        assert_eq!(
            server.count(
                "SELECT count(*) FROM reports
                 WHERE status = 'added' AND term_id = 1 AND reply IS NULL AND resolved_at IS NOT NULL"
            ),
            1
        );
        let (_, html) = server.get("/reports").await;
        assert!(!html.contains("給油停止"), "{html}");
        let (_, html) = server.get("/reports?status=all").await;
        assert!(html.contains("給油停止"), "{html}");
        let (_, html) = server.get(&format!("/articles/{id}")).await;
        assert!(html.contains("給油停止（追加済）"), "{html}");

        let res = server
            .post(
                &format!("/reports/{report}"),
                "status=rejected&term_id=&back_status=all",
            )
            .await;
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            "/reports?status=all"
        );
    }

    /// 訳語以外の指摘は対応済にでき、訳語集の状況や訳語は付けられない。種類で絞れる。
    #[tokio::test]
    async fn other_reports_are_resolved_as_done_and_filtered_by_kind() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let article = format!("/articles/{id}/report");
        server
            .post(
                &article,
                "kind=topic&note=%E3%83%88%E3%83%94%E3%83%83%E3%82%AF%E9%81%95%E3%81%84",
            )
            .await;
        server
            .post(
                &article,
                "kind=term&found=%E7%B5%A6%E6%B2%B9%E5%81%9C%E6%AD%A2",
            )
            .await;
        let (status, html) = server.get("/reports?status=all&kind=topic").await;
        assert_eq!(status, 200);
        assert!(
            html.contains("トピック違い") && !html.contains("給油停止"),
            "{html}"
        );
        assert_eq!(server.get("/reports?kind=bogus").await.0, 400);

        let topic = server.count("SELECT id FROM reports WHERE kind = 'topic'");
        let path = format!("/reports/{topic}");
        for body in ["status=added", "status=existing", "status=done&term_id=1"] {
            assert_eq!(
                server.post(&path, body).await.status().as_u16(),
                400,
                "{body}"
            );
        }
        let res = server
            .post(&path, "status=done&back_status=all&back_kind=topic")
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            "/reports?status=all&kind=topic"
        );
        assert_eq!(
            server.count(
                "SELECT count(*) FROM reports WHERE status = 'done' AND resolved_at IS NOT NULL"
            ),
            1
        );
        let term = server.count("SELECT id FROM reports WHERE kind = 'term'");
        let res = server
            .post(&format!("/reports/{term}"), "status=done")
            .await;
        assert_eq!(res.status().as_u16(), 400);
    }

    #[tokio::test]
    async fn reports_reject_invalid_requests() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        server
            .post(&format!("/articles/{id}/report"), "kind=term&found=x")
            .await;
        let path = format!("/reports/{}", server.count("SELECT id FROM reports"));
        assert_eq!(server.get("/reports?status=bogus").await.0, 400);
        assert_eq!(
            server.post(&path, "status=bogus").await.status().as_u16(),
            400
        );
        assert_eq!(server.post(&path, "term_id=1").await.status().as_u16(), 400);
        assert_eq!(
            server
                .post(&path, "status=added&term_id=x")
                .await
                .status()
                .as_u16(),
            400
        );
        assert_eq!(
            server
                .post("/reports/999", "status=added")
                .await
                .status()
                .as_u16(),
            404
        );
        let res = server
            .form(&path, "status=added")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        assert_eq!(
            server.count("SELECT count(*) FROM reports WHERE status = 'pending'"),
            1
        );
    }

    /// コメントは書いて、直して、消せる。公開はチェックしたときだけで、読んでいた画面に戻る。
    #[tokio::test]
    async fn comments_are_written_edited_and_deleted() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let res = server
            .post(
                &format!("/articles/{id}/comments"),
                "body=+%E3%83%A1%E3%83%A2+&public=1&view=translation",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}?view=translation#comments")
        );
        assert_eq!(
            server.strings("SELECT body || '|' || visibility FROM comments"),
            ["メモ|public"]
        );
        let (_, html) = server.get(&format!("/articles/{id}")).await;
        assert!(html.contains("<p>メモ</p>"), "{html}");

        let comment = server.count("SELECT id FROM comments");
        let res = server
            .post(&format!("/comments/{comment}"), "body=%E6%94%B9")
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}#comments")
        );
        assert_eq!(
            server.strings("SELECT body || '|' || visibility FROM comments"),
            ["改|private"]
        );
        let res = server
            .post(&format!("/comments/{comment}/delete"), "view=translation")
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}?view=translation#comments")
        );
        assert_eq!(server.count("SELECT count(*) FROM comments"), 0);
    }

    #[tokio::test]
    async fn comments_reject_invalid_requests() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        db.conn()
            .execute_batch(&format!(
                "INSERT INTO users (id, login, display_name) VALUES (99, 'other', 'other');
                 INSERT INTO comments (id, user_id, article_id, body, visibility, created_at, updated_at)
                   VALUES (5, 99, {id}, 'x', 'public', '2026-09-27T00:00:00.000Z', '2026-09-27T00:00:00.000Z');"
            ))
            .unwrap();
        let server = Server::start(db).await;
        let add = format!("/articles/{id}/comments");
        assert_eq!(server.post(&add, "body=+").await.status().as_u16(), 400);
        assert_eq!(server.post(&add, "public=1").await.status().as_u16(), 400);
        // 公開は `1` のときだけ。ほかの値で公開範囲を変えさせない
        for body in ["body=x&public=0", "body=x&public="] {
            assert_eq!(
                server.post(&add, body).await.status().as_u16(),
                400,
                "{body}"
            );
        }
        assert_eq!(
            server
                .post("/articles/999/comments", "body=x")
                .await
                .status()
                .as_u16(),
            404
        );
        // 他人のコメントは直せず消せない
        assert_eq!(
            server.post("/comments/5", "body=y").await.status().as_u16(),
            404
        );
        assert_eq!(
            server
                .post("/comments/5/delete", "")
                .await
                .status()
                .as_u16(),
            404
        );
        let res = server
            .form(&add, "body=x")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        assert_eq!(server.strings("SELECT body FROM comments"), ["x"]);
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
