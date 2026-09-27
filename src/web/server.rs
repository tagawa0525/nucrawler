//! Web UI の HTTP サーバー。画面の描画は `html`、データは `Db` に任せ、ここではルーティングと
//! 行動の記録（詳細・和訳を開いた、👍/👎、和訳の依頼）だけを行う。フィードは `feed`、JSON API の応答の形は `api` が決める。

use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Form, Path, Query, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{Duration, Utc};

use crate::config::WebConfig;
use crate::db::{Db, DbError, ListQuery, NewTermReport, SignalKind};
use crate::search::Params;
use crate::web::html::{self, DetailView, Page, SourceLabels};
use crate::web::{api, feed};

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
        .route("/articles/{id}/term-report", post(term_report))
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

/// Web の一覧に出す記事（設定の期間・件数・最低点）。
fn list_items(
    db: &Db,
    web: &WebConfig,
    user: i64,
    profile_hash: Option<&str>,
    now: chrono::DateTime<Utc>,
    show_all: bool,
) -> Result<Vec<crate::db::ListItem>, DbError> {
    db.list_articles(ListQuery {
        user_id: user,
        profile_hash,
        min_score: web.min_score,
        since: now - Duration::days(web.list_days.into()),
        show_all,
        limit: web.list_limit,
    })
}

/// 警告は直近 24 時間のものだけ出す。
fn warnings(db: &Db) -> Result<Vec<crate::db::Warning>, DbError> {
    db.warnings(Utc::now() - Duration::hours(24))
}

#[derive(serde::Deserialize)]
struct ListParams {
    all: Option<String>,
    /// Web の一覧だけが使う（過去の欄に既読も出す）
    read: Option<String>,
}

async fn list(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Html<String>, AppError> {
    let show_all = params.all.as_deref() == Some("1");
    let show_read = params.read.as_deref() == Some("1");
    let web = state.web.clone();
    let labels = state.labels.clone();
    let page = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        let boundary =
            db.begin_visit(user, now, Duration::minutes(web.visit_gap_minutes.into()))?;
        let items = list_items(db, &web, user, hash.as_deref(), now, show_all)?;
        let (new, earlier) = html::split_sections(items, boundary.as_deref(), show_read);
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
        };
        let view = html::ListView {
            all: show_all,
            read: show_read,
        };
        Ok(html::list_page(&new, &earlier, view, &page))
    })
    .await?;
    Ok(Html(page))
}

/// Web の既定の一覧と同じ記事の Atom フィード。閲覧ではないので、訪問も開いたことも記録しない。
async fn feed(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    // 記事のリンクは絶対 URL にする。http で待ち受けているので `http://` + Host
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map_or_else(|| state.web.bind.to_string(), str::to_string);
    let base = format!("http://{host}");
    let web = state.web.clone();
    let labels = state.labels.clone();
    let xml = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        let items = list_items(db, &web, user, hash.as_deref(), now, false)?;
        Ok(feed::atom(
            &items,
            &base,
            &labels,
            &crate::db::timestamp(now),
        ))
    })
    .await?;
    Ok((
        [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
        xml,
    )
        .into_response())
}

fn json(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Web の一覧と同じ記事（`all=1` ならすべて）。閲覧ではないので、訪問も開いたことも記録しない。
async fn api_list(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Response, AppError> {
    let show_all = params.all.as_deref() == Some("1");
    let web = state.web.clone();
    let labels = state.labels.clone();
    let body = with_db(&state, move |db| {
        let now = Utc::now();
        let (user, hash) = viewer(db)?;
        let items = list_items(db, &web, user, hash.as_deref(), now, show_all)?;
        Ok(serde_json::to_string(&api::ArticleList::new(
            &items, &labels,
        ))?)
    })
    .await?;
    Ok(json(body))
}

/// 検索画面。一覧で隠す記事も語や条件で探せる。閲覧ではないので、訪問も開いたことも記録しない。
/// 条件の誤りは、条件を残したフォームとともに 400 で返す。
async fn search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    let params = Params::from_query(raw.as_deref().unwrap_or(""));
    let web = state.web.clone();
    let labels = state.labels.clone();
    let (status, page) = with_db(&state, move |db| {
        let (user, hash) = viewer(db)?;
        let vocabulary = db.topic_usage()?;
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
        };
        // 条件が無くても（並びだけでも）値の誤りは 400 で返してから、フォームだけの画面にする
        let html = match params.to_query(user, hash.as_deref(), web.list_limit) {
            Ok(_) if params.is_empty() => {
                html::search_page(&params, None, &vocabulary, None, &page)
            }
            Ok(q) => {
                let items = db.search_articles(&q)?;
                html::search_page(&params, Some(&items), &vocabulary, None, &page)
            }
            Err(e) => {
                let html =
                    html::search_page(&params, None, &vocabulary, Some(&e.to_string()), &page);
                return Ok((StatusCode::BAD_REQUEST, html));
            }
        };
        Ok((StatusCode::OK, html))
    })
    .await?;
    Ok((status, Html(page)).into_response())
}

/// 検索画面と同じ条件の検索。閲覧ではないので、訪問も開いたことも記録しない。
async fn api_search(
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
async fn api_detail(
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

#[derive(serde::Deserialize)]
struct DetailParams {
    view: Option<String>,
    digest: Option<i64>,
    translation: Option<i64>,
    reported: Option<String>,
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
        reported: params.reported.is_some(),
    };
    let labels = state.labels.clone();
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
        let warnings = warnings(db)?;
        let page = Page {
            warnings: &warnings,
            labels: &labels,
        };
        Ok(html::detail_page(&detail, view, &page))
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
async fn undo_feedback(
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

async fn translation_request(
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

#[derive(serde::Deserialize)]
struct TermReportForm {
    found: String,
    #[serde(default)]
    wanted: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    note: String,
    /// 和訳を読んでいたなら `translation`（戻る先）
    view: Option<String>,
}

/// 訳語の指摘を受付箱に入れ、読んでいた画面の指摘の欄へ戻る。
async fn term_report(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<TermReportForm>,
) -> Result<Redirect, AppError> {
    check_same_origin(&headers)?;
    let filled = |s: &str| Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
    let found = filled(&form.found).ok_or(AppError::BadRequest("found must not be empty"))?;
    let (wanted, source, note) = (
        filled(&form.wanted),
        filled(&form.source),
        filled(&form.note),
    );
    with_db(&state, move |db| {
        let (user, _) = viewer(db)?;
        find_article(db, user, id)?;
        let report = NewTermReport {
            found: &found,
            wanted: wanted.as_deref(),
            source: source.as_deref(),
            note: note.as_deref(),
        };
        Ok(db.report_term(user, id, &report, Utc::now())?)
    })
    .await?;
    let view = if form.view.as_deref() == Some("translation") {
        "view=translation&"
    } else {
        ""
    };
    Ok(Redirect::to(&format!(
        "/articles/{id}?{view}reported=1#term-report"
    )))
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
    use super::*;
    use crate::config::{Lang, WebConfig};
    use crate::db::{ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact};

    /// 英語の記事に本文と digest を付ける。
    fn seed(db: &Db, url: &str, title_ja: &str) -> (i64, i64) {
        seed_with(db, url, title_ja, true)
    }

    fn seed_with(db: &Db, url: &str, title_ja: &str, lwr_relevant: bool) -> (i64, i64) {
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
                },
                chrono::Utc::now(),
            )
            .unwrap();
        (id, digest)
    }

    /// 所有者の現在のプロファイルで digest を採点する。
    fn score(db: &Db, digest: i64, score: u8) {
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
        };
        db.insert_score(key, digest, score, Some("理由"), chrono::Utc::now())
            .unwrap();
    }

    /// 既定の一覧に出る記事 1 件と、出ない記事（低い点、👎、軽水炉と無関係、未採点）。
    /// 出る記事の ID を返す。
    fn seed_recommended_and_hidden(db: &Db) -> i64 {
        let (good, digest) = seed(db, "https://e.com/good?a=1&b=2", "A&B <C>\u{1}");
        score(db, digest, 90);
        let (_, digest) = seed(db, "https://e.com/low", "低い点");
        score(db, digest, 10);
        let (down, digest) = seed(db, "https://e.com/down", "👎した");
        score(db, digest, 90);
        db.record_event(db.owner_id().unwrap(), down, SignalKind::Down, Utc::now())
            .unwrap();
        let (_, digest) = seed_with(db, "https://e.com/unrelated", "無関係", false);
        score(db, digest, 90);
        seed(db, "https://e.com/unscored", "未採点");
        good
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

        /// 閲覧の行動（開いた記録と訪問の区切り）が 1 つも記録されていない。
        fn assert_no_views(&self) {
            assert_eq!(
                self.count("SELECT count(*) FROM events WHERE kind LIKE 'open_%'"),
                0
            );
            assert_eq!(
                self.count("SELECT count(*) FROM users WHERE last_seen_at IS NOT NULL"),
                0
            );
        }

        async fn get_with_type(&self, path: &str) -> (u16, String, String) {
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

        async fn get_json(&self, path: &str) -> (u16, serde_json::Value) {
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

    /// フィードは既定の一覧と同じ記事を Atom で出し、閲覧としては記録しない。
    #[tokio::test]
    async fn feed_lists_recommended_articles_as_atom() {
        let db = Db::open_in_memory().unwrap();
        let good = seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let (status, content_type, xml) = server.get_with_type("/feed.xml").await;
        assert_eq!(status, 200);
        assert!(
            content_type.starts_with("application/atom+xml"),
            "{content_type}"
        );
        assert!(
            xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"),
            "{xml}"
        );
        assert!(
            xml.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">"),
            "{xml}"
        );
        assert_eq!(xml.matches("<entry>").count(), 1, "{xml}");
        for hidden in ["低い点", "👎した", "無関係", "未採点"] {
            assert!(!xml.contains(hidden), "{hidden}: {xml}");
        }
        // 和訳タイトル・要約・元記事と詳細ページへのリンク・日付
        assert!(xml.contains("<title>A&amp;B &lt;C&gt;</title>"), "{xml}");
        assert!(xml.contains("<summary>要約</summary>"), "{xml}");
        assert!(
            xml.contains(&format!(
                "<link rel=\"alternate\" href=\"{}/articles/{good}\"/>",
                server.base
            )),
            "{xml}"
        );
        assert!(
            xml.contains("<link rel=\"related\" href=\"https://e.com/good?a=1&amp;b=2\"/>"),
            "{xml}"
        );
        assert!(xml.contains("<updated>20"), "{xml}");
        // XML 1.0 に書けない制御文字は、実体参照にもできないので落とす
        assert!(!xml.contains('\u{1}') && !xml.contains("&#1;"), "{xml}");
        server.assert_no_views();
    }

    /// フィードとエントリの ID は、アクセスしたアドレスによらず同じ（リーダーが既読を見失わない）。
    /// リンクはアクセスしたアドレスから作る。
    #[tokio::test]
    async fn feed_ids_do_not_depend_on_the_host() {
        let db = Db::open_in_memory().unwrap();
        let good = seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let hosts = ["100.64.0.1:8080", "nucrawler.tailnet.ts.net"];
        let mut feeds = Vec::new();
        for host in hosts {
            let res = server
                .client
                .get(format!("{}/feed.xml", server.base))
                .header(header::HOST, host)
                .send()
                .await
                .unwrap();
            assert_eq!(res.status().as_u16(), 200);
            let xml = res.text().await.unwrap();
            assert!(
                xml.contains(&format!(
                    "<link rel=\"alternate\" href=\"http://{host}/articles/{good}\"/>"
                )),
                "{xml}"
            );
            feeds.push(xml);
        }
        let ids: Vec<Vec<&str>> = feeds
            .iter()
            .map(|xml| {
                xml.split("<id>")
                    .skip(1)
                    .map(|s| s.split_once("</id>").unwrap().0)
                    .collect()
            })
            .collect();
        // フィードとエントリ 1 件
        assert_eq!(ids[0].len(), 2, "{}", feeds[0]);
        assert_eq!(ids[0], ids[1]);
        for id in &ids[0] {
            for host in hosts {
                assert!(!id.contains(host), "{id}");
            }
        }
    }

    /// 検索画面は一覧で隠す記事も語で引ける。条件が無ければフォームだけで、閲覧としては記録しない。
    #[tokio::test]
    async fn search_page_finds_articles_by_terms() {
        let db = Db::open_in_memory().unwrap();
        let (hit, _) = seed_with(&db, "https://e.com/hit", "炉心溶融の解析", false);
        seed(&db, "https://e.com/other", "燃料の話");
        let server = Server::start(db).await;
        let (status, html) = server.get("/search").await;
        assert_eq!(status, 200);
        assert!(html.contains(r#"action="/search""#), "{html}");
        assert!(!html.contains("燃料の話"), "{html}");

        let (status, html) = server.get("/search?q=%E7%82%89%E5%BF%83").await;
        assert_eq!(status, 200);
        assert!(html.contains(&format!("/articles/{hit}")), "{html}");
        assert!(!html.contains("燃料の話"), "{html}");
        server.assert_no_views();
    }

    #[tokio::test]
    async fn search_rejects_invalid_conditions() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let (status, html) = server.get("/search?since=2026%2F09").await;
        assert_eq!(status, 400);
        // 並びは条件に数えないが、誤りは誤りとして返す
        let (status, _) = server.get("/search?sort=old").await;
        assert_eq!(status, 400);
        let (status, _) = server.get("/search?sort=score").await;
        assert_eq!(status, 200);
        assert!(
            html.contains("since must be YYYY, YYYY-MM or YYYY-MM-DD"),
            "{html}"
        );
        assert!(
            html.contains(r#"action="/search""#),
            "the form stays usable: {html}"
        );
        let (status, body) = server.get("/api/search?lang=fr").await;
        assert_eq!(status, 400);
        assert!(body.contains("lang must be en or ja"), "{body}");
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

    /// API の一覧は既定では Web と同じ記事を出し、`all=1` ですべてを出す。閲覧としては記録しない。
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

        let (_, json) = server.get_json("/api/articles?all=1").await;
        assert_eq!(json["articles"].as_array().unwrap().len(), 5, "{json}");
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

    /// `read=1` を受け取り、切り替えのリンクに反映する。
    #[tokio::test]
    async fn list_reads_the_read_toggle() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let (status, html) = server.get("/?all=1&read=1").await;
        assert_eq!(status, 200);
        assert!(html.contains("過去の既読も表示：ON"), "{html}");
        assert!(html.contains(r#"href="/?all=1""#), "{html}");
        assert!(html.contains(r#"href="/?read=1""#), "{html}");
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

        server.post(&feedback, "kind=up").await;
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

    /// 一覧でブックマークした記事は、振り分け済みとして一覧から外れる。
    #[tokio::test]
    async fn list_leaves_out_bookmarked_articles() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        server
            .post(&format!("/articles/{id}/feedback"), "kind=bookmark")
            .await;
        let (_, html) = server.get("/?all=1").await;
        assert!(!html.contains("見出しA"), "{html}");
        let (_, html) = server.get("/search?bookmarked=1").await;
        assert!(html.contains("見出しA"), "{html}");
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

    /// 訳語の指摘は受付箱に入り、空の欄は記録しない。読んでいた画面に戻る。
    #[tokio::test]
    async fn term_report_is_recorded_and_returns_to_detail() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::start(db).await;
        let path = format!("/articles/{id}/term-report");
        let res = server
            .post(
                &path,
                "found=%E7%B5%A6%E6%B2%B9%E5%81%9C%E6%AD%A2&wanted=&source=+refueling+outage+&note=&view=translation",
            )
            .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}?view=translation&reported=1#term-report")
        );
        assert_eq!(
            server.count(&format!(
                "SELECT count(*) FROM term_reports
                 WHERE article_id = {id} AND found = '給油停止' AND wanted IS NULL
                   AND source = 'refueling outage' AND note IS NULL AND resolved_at IS NULL"
            )),
            1
        );
        let res = server.post(&path, "found=x").await;
        assert_eq!(
            res.headers()["location"].to_str().unwrap(),
            format!("/articles/{id}?reported=1#term-report")
        );
        let (_, html) = server.get(&format!("/articles/{id}?reported=1")).await;
        assert!(html.contains("訳語の指摘を受け付けました"), "{html}");

        // 気になった訳は必須（欄が無くても空でも同じ）
        assert_eq!(server.post(&path, "wanted=a").await.status().as_u16(), 400);
        assert_eq!(
            server
                .post(&path, "found=+&wanted=a")
                .await
                .status()
                .as_u16(),
            400
        );
        let res = server
            .form(&path, "found=x")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        let res = server.post("/articles/999/term-report", "found=x").await;
        assert_eq!(res.status().as_u16(), 404);
        assert_eq!(server.count("SELECT count(*) FROM term_reports"), 2);
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
