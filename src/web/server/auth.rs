//! ログインとセッション（計画 009）。ログインしているかはルーター全体に掛ける層で確かめ、
//! ハンドラは層が入れた `Viewer` を受け取る（ハンドラごとに確かめると、付け忘れた画面が認証なしで開く）。

use super::*;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use tokio::sync::Semaphore;

use crate::auth;
use crate::db::Viewer;

/// セッションのトークンを入れる Cookie の名前。
pub(super) const SESSION_COOKIE: &str = "nucrawler_session";
/// 層がリクエストに入れる、要求元のセッションのトークン（ログアウト・フィードのトークンの作り直しで使う）。
#[derive(Clone)]
pub(super) struct SessionToken(pub(super) String);

/// ログインしていなくても通すパス。ログイン画面と、フィード（リーダーはログインできないので、URL のトークンで読む）。
fn is_public(path: &str) -> bool {
    matches!(path, "/login" | "/feed.xml")
}

/// 要求の Cookie から、セッションのトークンを取り出す。
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_string())
        .filter(|v| !v.is_empty())
}

/// ルーター全体に掛ける層：ログイン画面とフィードのほかは、セッションの利用者を `Viewer` として要求に入れる。
/// ログインしていなければ、画面（GET）はログイン画面へ元の画面を `next` に持って移し、JSON と書き込みは 401。
pub(super) async fn require_session(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    if is_public(req.uri().path()) {
        return next.run(req).await;
    }
    let token = session_cookie(req.headers());
    let viewer = match token.clone() {
        None => Ok(None),
        Some(token) => {
            with_db(&state, move |db| {
                Ok(db.session_viewer(&token, chrono::Utc::now())?)
            })
            .await
        }
    };
    match (viewer, token) {
        (Ok(Some(viewer)), Some(token)) => {
            req.extensions_mut().insert(viewer);
            req.extensions_mut().insert(SessionToken(token));
            next.run(req).await
        }
        (Err(e), _) => e.into_response(),
        _ => unauthenticated(&req),
    }
}

fn unauthenticated(req: &Request) -> Response {
    let page = matches!(*req.method(), Method::GET | Method::HEAD)
        && !req.uri().path().starts_with("/api/");
    if page {
        let here = req
            .uri()
            .path_and_query()
            .map_or("/", |p| p.as_str())
            .to_string();
        let next: String = url::form_urlencoded::byte_serialize(here.as_bytes()).collect();
        Redirect::to(&format!("/login?next={next}")).into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "login required").into_response()
    }
}

/// ルーター全体に掛ける層：書き込み（GET・HEAD 以外）は、ブラウザが付ける Origin がこのサーバー自身でなければ断る。
/// ログイン・ログアウトも含めて、付け忘れる書き込みが無いよう、ハンドラごとではなくここで確かめる。
pub(super) async fn same_origin(req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD)
        && let Err(e) = check_same_origin(req.headers())
    {
        return e.into_response();
    }
    next.run(req).await
}

/// IP ごとに、この回数を超えて続けて失敗したら待たせる（ID ごとより多いのは、1 台から複数の人が打ち間違えることもあるため）。
const IP_FAILURES_MAX: u8 = 20;
/// IP ごとの待ち時間。最後に数えた失敗からこの時間が過ぎると、数え直す。
const IP_WINDOW: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// IP ごとの記録の数の上限（IP を変え続ける相手でもメモリが増え続けないよう）。
pub(super) const IP_ENTRIES_MAX: usize = 4096;

/// 接続元の IP ごとのログインの失敗（メモリに持ち、再起動で消えてよい）。ID ごとの回数は `users` の行にあるので、
/// 存在しない ID を変えながら試す相手には効かない。その相手をここで止める。
#[derive(Default)]
pub(super) struct IpThrottle {
    entries: std::sync::Mutex<HashMap<IpAddr, IpEntry>>,
}

struct IpEntry {
    failures: u8,
    /// 最後に数えた失敗
    last: Instant,
}

impl IpEntry {
    fn blocked(&self, now: Instant) -> bool {
        self.failures > IP_FAILURES_MAX && now < self.last + IP_WINDOW
    }
}

impl IpThrottle {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, IpEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// この IP が待ち時間中か。待ち時間は送り手自身の失敗だけで決まり、どの ID を送ったかによらない。
    pub(super) fn blocked(&self, ip: IpAddr, now: Instant) -> bool {
        self.lock().get(&ip).is_some_and(|e| e.blocked(now))
    }

    /// 失敗を 1 回数える。待ち時間中の試行は数えない（送り続けても待ち時間は延びない）。ログインの成功では消さない
    /// （自分の ID での成功を挟めば、ほかの ID を制限なく試せてしまうため）。
    pub(super) fn record_failure(&self, ip: IpAddr, now: Instant) {
        let mut entries = self.lock();
        if entries.get(&ip).is_some_and(|e| e.blocked(now)) {
            return;
        }
        entries.retain(|_, e| now < e.last + IP_WINDOW);
        let entry = entries.entry(ip).or_insert(IpEntry {
            failures: 0,
            last: now,
        });
        entry.failures = (entry.failures + 1).min(IP_FAILURES_MAX + 1);
        entry.last = now;
        if entries.len() > IP_ENTRIES_MAX
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.last)
                .map(|(ip, _)| *ip)
        {
            entries.remove(&oldest);
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.lock().len()
    }
}

/// パスワードの計算（argon2id）を同時に走らせる数。
const HASHING_MAX: usize = 2;

/// Web サーバーのパスワードの計算をすべて通す部品。同時に走らせる数に上限を置き、空きが無ければ待たずに断る
/// （待たせる数にも上限が要るため）。計算は `spawn_blocking` で行い、取った枠はそのクロージャに持たせるので、
/// 要求が途中で捨てられても、計算とその後の記録は最後まで走り、終わるまで枠を返さない。
#[derive(Clone)]
pub(super) struct PasswordHasher {
    permits: Arc<Semaphore>,
}

impl Default for PasswordHasher {
    fn default() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(HASHING_MAX)),
        }
    }
}

/// 計算の空きが無い。
#[derive(Debug)]
pub(super) struct Busy;

impl PasswordHasher {
    pub(super) async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Busy> {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| Busy)?;
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f()
        });
        match task.await {
            Ok(value) => Ok(value),
            // 計算の中の panic は、呼び出し側にそのまま伝える
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => panic!("password hashing task failed: {e}"),
        }
    }

    #[cfg(test)]
    pub(super) fn available(&self) -> usize {
        self.permits.available_permits()
    }
}

/// セッションの Cookie。HTTP で待ち受けるので `Secure` は付けられない（付けるとブラウザが送らない）。
/// `SameSite=Lax` は、フィードやチャットのリンクから開いたときにも送られるように（書き込みは `same_origin` で守る）。
fn session_cookie_header(token: &str, max_age: i64) -> String {
    format!("{SESSION_COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}")
}

/// ログイン後に戻る先。`next` を、ブラウザと同じ規則（WHATWG URL）で `http://{host}/` を基準に解釈し、
/// 自分のホストを指すときだけ使う。文字列の形で判定すると、`/\evil.example` のような値でブラウザの解釈とずれる。
/// 返すのは絶対 URL（パスだけを返すと、`//evil.example` のようなパスをブラウザが別のホストとして読む）。
fn return_to(host: &str, next: Option<&str>) -> String {
    let Ok(base) = url::Url::parse(&format!("http://{host}/")) else {
        return "/".to_string();
    };
    next.and_then(|n| base.join(n).ok())
        .filter(|u| u.origin() == base.origin())
        .unwrap_or_else(|| base.clone())
        .to_string()
}

pub(super) fn request_host(headers: &HeaderMap, web: &WebConfig) -> String {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map_or_else(|| web.bind.to_string(), str::to_string)
}

#[derive(serde::Deserialize)]
pub(super) struct LoginQuery {
    next: Option<String>,
}

pub(super) async fn login_page(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> Html<String> {
    Html(html::login_page(q.next.as_deref(), false, &state.labels))
}

#[derive(serde::Deserialize)]
pub(super) struct LoginForm {
    #[serde(default)]
    login: String,
    #[serde(default)]
    password: String,
    next: Option<String>,
}

/// 失敗の応答。ID が無い・パスワードが違う・待ち時間中・パスワードが無い・長すぎる、のどれでも同じにする
/// （応答の違いから ID の有無を調べられないように）。
fn login_failed(next: Option<&str>, labels: &html::SourceLabels) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Html(html::login_page(next, true, labels)),
    )
        .into_response()
}

/// ログイン。照合は重いので `PasswordHasher` を通し、1 段目（ハッシュを読む）・2 段目（照合）・3 段目（判定して書く）と
/// IP ごとの失敗の記録を 1 つのクロージャで行う（途中で接続が切れても、照合したものは必ず数える）。
/// ID が無いかパスワードが無ければダミーのハッシュで照合して、応答までの時間を揃える。
pub(super) async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    // 転送ヘッダーは使わない（リバースプロキシが無く、送り手が自由に書けるため）。ポートは接続ごとに変わるので除く
    let ip = peer.ip().to_canonical();
    let next = form.next.clone();
    if state.throttle.blocked(ip, Instant::now()) {
        return Ok(login_failed(next.as_deref(), &state.labels));
    }
    if !auth::valid_login(&form.login) || !auth::within_password_limit(&form.password) {
        // 長さは送り手が決めるもので ID によらないので、計算せずに返しても ID の有無は漏れない
        state.throttle.record_failure(ip, Instant::now());
        return Ok(login_failed(next.as_deref(), &state.labels));
    }
    let db = state.db.clone();
    let throttle = state.throttle.clone();
    let attempt = state
        .hasher
        .run(move || -> Result<Option<String>, DbError> {
            let lock = || db.lock().unwrap_or_else(PoisonError::into_inner);
            let read = lock().login_hash(&form.login)?;
            let verified = auth::verify_password(
                read.as_deref().unwrap_or(auth::dummy_hash()),
                &form.password,
            );
            let token =
                lock().finish_login(&form.login, read.as_deref(), verified, chrono::Utc::now())?;
            if token.is_none() {
                throttle.record_failure(ip, Instant::now());
            }
            Ok(token)
        })
        .await;
    match attempt {
        Err(Busy) => Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            "混み合っています。少し待ってから試してください",
        )
            .into_response()),
        Ok(result) => match result? {
            None => Ok(login_failed(next.as_deref(), &state.labels)),
            Some(token) => {
                let to = return_to(&request_host(&headers, &state.web), next.as_deref());
                let max_age = auth::SESSION_DAYS * 24 * 60 * 60;
                Ok((
                    [(header::SET_COOKIE, session_cookie_header(&token, max_age))],
                    Redirect::to(&to),
                )
                    .into_response())
            }
        },
    }
}

/// ログアウト。セッションを消し、同じ `Path` で期限切れの Cookie を返す。
pub(super) async fn logout(
    State(state): State<AppState>,
    Extension(SessionToken(token)): Extension<SessionToken>,
) -> Result<Response, AppError> {
    with_db(&state, move |db| Ok(db.logout(&token)?)).await?;
    Ok((
        [(header::SET_COOKIE, session_cookie_header("", 0))],
        Redirect::to("/login"),
    )
        .into_response())
}

/// フィードのトークンを作り直す（古い購読用の URL は使えなくなる）。
pub(super) async fn rotate_feed_token(
    State(state): State<AppState>,
    Extension(SessionToken(token)): Extension<SessionToken>,
) -> Result<Redirect, AppError> {
    with_db(&state, move |db| {
        Ok(db.rotate_feed_token(&token, chrono::Utc::now())?)
    })
    .await?;
    Ok(Redirect::to("/settings"))
}

/// フィードを読む利用者（URL のトークン）。
pub(super) fn feed_viewer(db: &Db, token: Option<&str>) -> Result<Option<Viewer>, DbError> {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    Ok(db.feed_viewer(token)?.map(|user_id| Viewer {
        user_id,
        is_admin: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::web::server::test_support::*;

    const PASSWORD: &str = "correct horse battery";

    /// 所有者にパスワードを設定し、ログイン ID を返す。
    fn with_password(db: &Db) -> &'static str {
        db.reset_password("owner", &crate::auth::hash_password(PASSWORD).unwrap())
            .unwrap();
        "owner"
    }

    fn cookie_of(res: &reqwest::Response) -> String {
        res.headers()[reqwest::header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_string()
    }

    async fn login(server: &Server, body: String) -> reqwest::Response {
        server
            .client
            .post(format!("{}/login", server.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .unwrap()
    }

    fn form(pairs: &[(&str, &str)]) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish()
    }

    /// ログインしていなければ、画面はログイン画面へ（元の画面を `next` に持って）、JSON と書き込みは 401。
    /// ログイン画面とフィード（トークンで読む）は層を通らない。
    #[tokio::test]
    async fn requests_without_a_session_are_sent_to_login() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let server = Server::anonymous(db).await;
        for (path, next) in [
            ("/", "%2F"),
            ("/?min=0&read=any", "%2F%3Fmin%3D0%26read%3Dany"),
            ("/settings", "%2Fsettings"),
        ] {
            let res = server.get_raw(path).await;
            assert_eq!(res.status().as_u16(), 303, "{path}");
            assert_eq!(
                res.headers()["location"],
                format!("/login?next={next}"),
                "{path}"
            );
        }
        assert_eq!(server.get_raw("/api/articles").await.status().as_u16(), 401);
        let res = server
            .post(&format!("/articles/{id}/rating"), "value=4")
            .await;
        assert_eq!(res.status().as_u16(), 401);
        assert_eq!(server.count("SELECT count(*) FROM ratings"), 0);
        let (status, html) = server.get("/login?next=%2Fsettings").await;
        assert_eq!(status, 200);
        assert!(html.contains(r#"name="next" value="/settings""#), "{html}");
        // ログイン ID はメールアドレスに限らない（`owner` など）ので、ブラウザにメールアドレスとして検証させない
        assert!(
            html.contains(r#"<input name="login" type="text""#),
            "{html}"
        );
        assert_eq!(server.get_raw("/feed.xml").await.status().as_u16(), 401);
        // 期限切れや知らないセッションも、無いのと同じ
        let res = server
            .client
            .get(format!("{}/", server.base))
            .header(reqwest::header::COOKIE, format!("{SESSION_COOKIE}=unknown"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 303);
    }

    /// ログインするとセッションの Cookie が付き、元の画面（自分のホストの絶対 URL）へ戻る。
    #[tokio::test]
    async fn logging_in_sets_the_session_cookie_and_returns() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        let server = Server::anonymous(db).await;
        let res = login(
            &server,
            form(&[
                ("login", login_id),
                ("password", PASSWORD),
                ("next", "/?min=0"),
            ]),
        )
        .await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], format!("{}/?min=0", server.base));
        let cookie = cookie_of(&res);
        let token = cookie
            .strip_prefix(&format!("{SESSION_COOKIE}="))
            .and_then(|c| c.split(';').next())
            .unwrap()
            .to_string();
        assert_eq!(token.len(), 64, "{cookie}");
        for attr in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
            assert!(cookie.contains(attr), "{attr}: {cookie}");
        }
        // HTTP で待ち受けるので、Secure を付けるとブラウザが送らない
        assert!(!cookie.contains("Secure"), "{cookie}");
        let res = server
            .client
            .get(format!("{}/", server.base))
            .header(reqwest::header::COOKIE, format!("{SESSION_COOKIE}={token}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 200);
    }

    /// 失敗の応答は、ID が無い・パスワードが違う・パスワードが無い・長すぎる、のどれでも同じ。
    #[tokio::test]
    async fn every_login_failure_looks_the_same() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        other_user(&db, "nopassword@example.com");
        let server = Server::anonymous(db).await;
        let long = "a".repeat(1025);
        let mut responses = Vec::new();
        for (id, password) in [
            (login_id, "wrong password!"),
            ("nobody@example.com", PASSWORD),
            ("nopassword@example.com", PASSWORD),
            (login_id, long.as_str()),
        ] {
            let res = login(&server, form(&[("login", id), ("password", password)])).await;
            assert!(res.headers().get(reqwest::header::SET_COOKIE).is_none());
            responses.push((res.status().as_u16(), res.text().await.unwrap()));
        }
        assert_eq!(responses[0].0, 401);
        assert!(
            responses.iter().all(|r| *r == responses[0]),
            "{responses:?}"
        );
        assert_eq!(server.count("SELECT count(*) FROM sessions"), 0);
    }

    /// 戻り先がほかのホストを指すなら、自分のホストの / に戻す（オープンリダイレクトにしない）。
    #[tokio::test]
    async fn logins_never_return_to_another_host() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        let server = Server::anonymous(db).await;
        for next in [
            "//evil.example",
            "http://evil.example/",
            "/\\evil.example",
            "/\t/evil.example",
        ] {
            let res = login(
                &server,
                form(&[("login", login_id), ("password", PASSWORD), ("next", next)]),
            )
            .await;
            assert_eq!(res.status().as_u16(), 303, "{next:?}");
            assert_eq!(
                res.headers()["location"],
                format!("{}/", server.base),
                "{next:?}"
            );
        }
    }

    /// ログアウトはセッションを消し、同じ Path で期限切れの Cookie を返す。
    #[tokio::test]
    async fn logging_out_ends_the_session() {
        let server = Server::start(Db::open_in_memory().unwrap()).await;
        let res = server.post("/logout", "").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], "/login");
        let cookie = cookie_of(&res);
        assert!(
            cookie.contains("Max-Age=0") && cookie.contains("Path=/"),
            "{cookie}"
        );
        assert_eq!(server.get_raw("/").await.status().as_u16(), 303);
    }

    /// 他サイトの Origin からの POST は、ログインもログアウトも 403 で、セッションは作られず消えない。
    #[tokio::test]
    async fn cross_site_logins_and_logouts_are_refused() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        let server = Server::start(db).await;
        let res = server
            .client
            .post(format!("{}/login", server.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("origin", "https://evil.example")
            .body(form(&[("login", login_id), ("password", PASSWORD)]))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        let res = server
            .form("/logout", "")
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        assert_eq!(server.count("SELECT count(*) FROM sessions"), 1);
    }

    /// 同じ IP から 20 回を超えて失敗すると、ほかの ID でも、正しいパスワードでも、しばらくログインできない
    /// （ほかの ID での成功を挟んでも数え直さない）。
    #[tokio::test]
    async fn one_ip_failing_many_times_is_held_off() {
        let db = Db::open_in_memory().unwrap();
        let login_id = with_password(&db);
        let server = Server::anonymous(db).await;
        for n in 0..21 {
            let id = format!("nobody{n}@example.com");
            let res = login(&server, form(&[("login", &id), ("password", PASSWORD)])).await;
            assert_eq!(res.status().as_u16(), 401);
            if n == 10 {
                let ok = login(
                    &server,
                    form(&[("login", login_id), ("password", PASSWORD)]),
                )
                .await;
                assert_eq!(ok.status().as_u16(), 303, "a success does not reset the IP");
            }
        }
        let res = login(
            &server,
            form(&[("login", login_id), ("password", PASSWORD)]),
        )
        .await;
        assert_eq!(res.status().as_u16(), 401);
    }

    /// 2 人の利用者の評価は、互いの記録にならない。
    #[tokio::test]
    async fn each_user_rates_for_themselves() {
        let db = Db::open_in_memory().unwrap();
        let (id, _) = seed(&db, "https://e.com/a", "見出しA");
        let other = other_user(&db, "b@example.com");
        insert_session(&db, other, "b-session");
        let server = Server::start(db).await;
        let res = server
            .client
            .post(format!("{}/articles/{id}/rating", server.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                reqwest::header::COOKIE,
                format!("{SESSION_COOKIE}=b-session"),
            )
            .body("value=5")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(
            server.count(&format!(
                "SELECT count(*) FROM ratings WHERE user_id = {other}"
            )),
            1
        );
        assert_eq!(
            server.count("SELECT count(*) FROM ratings WHERE user_id = (SELECT id FROM users WHERE is_owner = 1)"),
            0
        );
    }

    /// フィードはトークンの利用者のものを出し、self のリンクはトークン付きの購読用の URL。作り直すと古いトークンは 401。
    #[tokio::test]
    async fn feeds_are_read_with_the_users_token() {
        let db = Db::open_in_memory().unwrap();
        seed_recommended_and_hidden(&db);
        let server = Server::start(db).await;
        let first = server.feed_path();
        let (status, xml) = server.get(&first).await;
        assert_eq!(status, 200);
        let token = first.strip_prefix("/feed.xml?token=").unwrap();
        assert!(
            xml.contains(&format!(
                "<link rel=\"self\" href=\"{}/feed.xml?token={token}\"/>",
                server.base
            )),
            "{xml}"
        );
        // 設定画面に購読用の URL が出る
        let (_, html) = server.get("/settings").await;
        assert!(html.contains(&format!("/feed.xml?token={token}")), "{html}");
        let res = server.post("/settings/feed-token", "").await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(server.get_raw(&first).await.status().as_u16(), 401);
        assert_eq!(
            server.get_raw("/feed.xml?token=").await.status().as_u16(),
            401
        );
    }

    /// フィードの ID は利用者ごとに違い、トークンを作り直しても変わらない（リーダーが別の利用者の購読と混ぜないように）。
    #[tokio::test]
    async fn feed_ids_are_per_user_and_survive_token_rotation() {
        let db = Db::open_in_memory().unwrap();
        let other = other_user(&db, "b@example.com");
        insert_session(&db, other, "b-session");
        let server = Server::start(db).await;
        let feed_id = |xml: &str| {
            xml.split_once("<id>")
                .and_then(|(_, rest)| rest.split_once("</id>"))
                .unwrap()
                .0
                .to_string()
        };
        let (_, mine) = server.get(&server.feed_path()).await;
        let (_, rotated) = server.get(&server.feed_path()).await;
        assert_eq!(feed_id(&mine), feed_id(&rotated));
        let theirs = server
            .state
            .db
            .lock()
            .unwrap()
            .rotate_feed_token("b-session", chrono::Utc::now())
            .unwrap()
            .unwrap();
        let (_, theirs) = server.get(&format!("/feed.xml?token={theirs}")).await;
        assert_ne!(feed_id(&mine), feed_id(&theirs));
        assert!(!feed_id(&mine).contains("token"), "{mine}");
    }

    /// パスワードの変更のフォームを、所有者のセッションで送る。
    async fn change(server: &Server, current: &str, new: &str) -> reqwest::Response {
        server
            .client
            .post(format!("{}/settings/password", server.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form(&[("current", current), ("new", new)]))
            .send()
            .await
            .unwrap()
    }

    const NEW_PASSWORD: &str = "a brand new passphrase";

    /// 変えると、要求元のものも含めてセッションとフィードのトークンがすべて失効し、要求元には新しいセッションの
    /// Cookie を返す。新しいパスワードで照合が通る。
    #[tokio::test]
    async fn changing_the_password_rotates_sessions_and_the_feed() {
        let db = Db::open_in_memory().unwrap();
        with_password(&db);
        let server = Server::start(db).await;
        let (_, html) = server.get("/settings").await;
        assert!(html.contains(r#"action="/settings/password""#), "{html}");
        let feed = server.feed_path();
        let res = change(&server, PASSWORD, NEW_PASSWORD).await;
        assert_eq!(res.status().as_u16(), 303);
        assert_eq!(res.headers()["location"], "/settings?password=changed");
        let cookie = cookie_of(&res);
        for attr in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
            assert!(cookie.contains(attr), "{attr}: {cookie}");
        }
        assert!(!cookie.contains("Secure"), "{cookie}");
        let token = cookie
            .strip_prefix(&format!("{SESSION_COOKIE}="))
            .and_then(|c| c.split(';').next())
            .unwrap()
            .to_string();
        // 変更前の Cookie（要求元のもの）とフィードの URL は使えない
        assert_eq!(server.get_raw("/").await.status().as_u16(), 303);
        assert_eq!(server.get_raw(&feed).await.status().as_u16(), 401);
        let res = server
            .client
            .get(format!("{}/settings", server.base))
            .header(reqwest::header::COOKIE, format!("{SESSION_COOKIE}={token}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 200);
        let hash = server
            .state
            .db
            .lock()
            .unwrap()
            .login_hash("owner")
            .unwrap()
            .unwrap();
        assert!(crate::auth::verify_password(&hash, NEW_PASSWORD));
    }

    /// 今のパスワードの誤りはログインの失敗と同じく数え、6 回目でその人のセッションがすべて消える。
    #[tokio::test]
    async fn wrong_current_passwords_are_counted() {
        let db = Db::open_in_memory().unwrap();
        with_password(&db);
        let server = Server::start(db).await;
        for _ in 0..5 {
            let res = change(&server, "not my password", NEW_PASSWORD).await;
            assert_eq!(res.status().as_u16(), 303);
            assert_eq!(res.headers()["location"], "/settings?password=wrong");
        }
        assert_eq!(server.get_raw("/").await.status().as_u16(), 200);
        change(&server, "not my password", NEW_PASSWORD).await;
        assert_eq!(server.get_raw("/").await.status().as_u16(), 303);
        let (_, html) = server.get("/login").await;
        assert!(html.contains("ログイン"), "{html}");
    }

    /// 新しいパスワードが条件を満たさない、または今のパスワードが長すぎるときは 400 で、何も変わらない。
    #[tokio::test]
    async fn invalid_passwords_are_rejected_before_hashing() {
        let db = Db::open_in_memory().unwrap();
        with_password(&db);
        let server = Server::start(db).await;
        let long = "a".repeat(1025);
        for (current, new) in [
            (PASSWORD, "short"),
            (PASSWORD, long.as_str()),
            (long.as_str(), NEW_PASSWORD),
        ] {
            let res = change(&server, current, new).await;
            assert_eq!(res.status().as_u16(), 400, "{current:.10} / {new:.10}");
        }
        assert_eq!(
            server.count("SELECT failed_logins FROM users WHERE login = 'owner'"),
            0
        );
        assert_eq!(server.get_raw("/").await.status().as_u16(), 200);
    }

    /// 他サイトからの変更は 403 で、パスワードもセッションも変わらない。
    #[tokio::test]
    async fn cross_site_password_changes_are_refused() {
        let db = Db::open_in_memory().unwrap();
        with_password(&db);
        let server = Server::start(db).await;
        let res = server
            .client
            .post(format!("{}/settings/password", server.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("origin", "https://evil.example")
            .body(form(&[("current", PASSWORD), ("new", NEW_PASSWORD)]))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        assert_eq!(server.get_raw("/").await.status().as_u16(), 200);
    }

    /// パスワードの変更の計算も、ログインと同じ枠を使う（空きが無ければ 503）。
    #[tokio::test]
    async fn password_changes_share_the_hashing_limit() {
        let db = Db::open_in_memory().unwrap();
        with_password(&db);
        let server = Server::start(db).await;
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(gate));
        let mut busy = Vec::new();
        for _ in 0..2 {
            let gate = gate.clone();
            let mut task = Box::pin(server.state.hasher.run(move || {
                gate.lock().unwrap().recv().unwrap();
            }));
            assert!(futures_poll_once(&mut task).await);
            busy.push(task);
        }
        let res = change(&server, PASSWORD, NEW_PASSWORD).await;
        assert_eq!(res.status().as_u16(), 503);
        release.send(()).unwrap();
        release.send(()).unwrap();
        for task in busy {
            task.await.unwrap();
        }
    }

    /// IP ごとの失敗は、ポートを除いたアドレスで数え、15 分途切れたら数え直す。記録の数には上限がある。
    #[test]
    fn ip_throttle_counts_by_address() {
        use std::net::{IpAddr, Ipv4Addr};
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let throttle = IpThrottle::default();
        for n in 0..20 {
            throttle.record_failure(a, t0 + Duration::from_secs(n));
        }
        assert!(!throttle.blocked(a, t0 + Duration::from_secs(20)));
        throttle.record_failure(a, t0 + Duration::from_secs(20));
        assert!(throttle.blocked(a, t0 + Duration::from_secs(21)));
        assert!(!throttle.blocked(b, t0 + Duration::from_secs(21)));
        // 待ち時間中の試行は数えないので、延びない
        throttle.record_failure(a, t0 + Duration::from_secs(600));
        let after = t0 + Duration::from_secs(20) + Duration::from_secs(15 * 60);
        assert!(!throttle.blocked(a, after));
        // 15 分途切れたら数え直す
        throttle.record_failure(a, after);
        assert!(!throttle.blocked(a, after));

        let many = IpThrottle::default();
        for n in 0..(IP_ENTRIES_MAX as u32 + 100) {
            many.record_failure(IpAddr::V4(Ipv4Addr::from(n)), t0);
        }
        assert_eq!(many.len(), IP_ENTRIES_MAX);
    }

    /// パスワードの計算は同時に 2 つまでで、空きが無ければ待たずに断る。要求が捨てられても、
    /// 計算（と、その後の記録）は最後まで走り、終わるまで枠を返さない。
    #[tokio::test]
    async fn hashing_is_bounded_and_survives_dropped_requests() {
        let hasher = PasswordHasher::default();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(gate));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut dropped = Vec::new();
        for _ in 0..2 {
            let (gate, done) = (gate.clone(), done.clone());
            let task = hasher.run(move || {
                gate.lock().unwrap().recv().unwrap();
                done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            // 走り出させてから要求を捨てる
            let mut task = Box::pin(task);
            assert!(futures_poll_once(&mut task).await);
            dropped.push(task);
        }
        drop(dropped);
        assert!(matches!(hasher.run(|| ()).await, Err(Busy)));
        release.send(()).unwrap();
        release.send(()).unwrap();
        for _ in 0..100 {
            if done.load(std::sync::atomic::Ordering::SeqCst) == 2 && hasher.available() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(done.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(hasher.run(|| ()).await.is_ok());
    }

    /// future を 1 回だけ進める（まだ終わっていなければ true）。
    async fn futures_poll_once<F: std::future::Future + Unpin>(f: &mut F) -> bool {
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::pin::Pin::new(&mut *f).poll(cx).is_pending())
        })
        .await
    }
}
