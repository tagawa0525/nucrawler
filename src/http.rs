//! 行儀の良い HTTP 取得：UA を名乗り、タイムアウトを設け、同じホストへの連続アクセスに間隔を空ける。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, OnceCell};
use tokio::time::Instant;
use url::Url;

use crate::config::HttpConfig;
use crate::robots::Rules;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("failed to build http client")]
    Build(#[source] reqwest::Error),
    #[error("request to {url} failed")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("too many redirects starting from {url}")]
    TooManyRedirects { url: String },
    #[error("bad redirect from {url}: {reason}")]
    BadRedirect { url: String, reason: String },
    #[error("response from {url} exceeds {limit} bytes")]
    BodyTooLarge { url: String, limit: u64 },
    #[error("robots.txt disallows {url}")]
    DisallowedByRobots { url: String },
    #[error("{url} returned {status}")]
    Status {
        url: String,
        status: reqwest::StatusCode,
    },
}

const MAX_REDIRECTS: usize = 5;

/// 取得結果。`url` はリダイレクトをたどった後の、実際に応答した URL（相対リンクの基準）。
#[derive(Debug)]
pub struct Fetched {
    pub url: Url,
    pub body: Vec<u8>,
}

pub struct Fetcher {
    client: reqwest::Client,
    per_host_delay: Duration,
    max_body_bytes: u64,
    /// robots.txt のグループ選択に使う UA の製品名（例 "nucrawler"）
    product: String,
    /// オリジンごとの robots.txt の規則。マップのロックは表の出し入れの間だけ持ち、
    /// 取得はオリジンごとの OnceCell で一度だけ行う（遅いオリジンが他を止めない）。
    robots: Mutex<HashMap<String, Arc<OnceCell<Rules>>>>,
    /// ホストごとの、次にアクセスしてよい時刻
    next_allowed: Mutex<HashMap<String, Instant>>,
}

impl Fetcher {
    pub fn new(
        user_agent: &str,
        timeout: Duration,
        per_host_delay: Duration,
        max_body_bytes: u64,
    ) -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(timeout)
            // 転送先へのアクセスにもホストごとの間隔を守らせるため、リダイレクトは自前でたどる。
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(HttpError::Build)?;
        Ok(Self {
            client,
            per_host_delay,
            max_body_bytes,
            product: user_agent
                .split(['/', ' '])
                .next()
                .unwrap_or_default()
                .to_string(),
            robots: Mutex::new(HashMap::new()),
            next_allowed: Mutex::new(HashMap::new()),
        })
    }

    pub fn from_config(c: &HttpConfig) -> Result<Self, HttpError> {
        Self::new(
            &c.user_agent,
            Duration::from_secs(c.timeout_secs),
            Duration::from_secs(c.per_host_delay_secs),
            c.max_body_bytes,
        )
    }

    /// フィード用。リダイレクトは `MAX_REDIRECTS` 回までたどる。最終的な応答が 2xx 以外ならエラーにする。
    pub async fn get(&self, url: &Url) -> Result<Fetched, HttpError> {
        self.fetch(url, false).await
    }

    /// 記事ページ用。各リクエストの前に（リダイレクト先も含めて）そのオリジンの robots.txt を
    /// 確かめ、禁止されていれば取得しない。
    pub async fn get_page(&self, url: &Url) -> Result<Fetched, HttpError> {
        self.fetch(url, true).await
    }

    async fn fetch(&self, url: &Url, obey_robots: bool) -> Result<Fetched, HttpError> {
        let mut current = url.clone();
        for _ in 0..=MAX_REDIRECTS {
            if obey_robots && !self.robots_allow(&current).await {
                return Err(HttpError::DisallowedByRobots {
                    url: current.to_string(),
                });
            }
            self.wait_for_turn(&current).await;
            let request_error = |source| HttpError::Request {
                url: current.to_string(),
                source,
            };
            let response = self
                .client
                .get(current.clone())
                .send()
                .await
                .map_err(request_error)?;
            let status = response.status();
            if is_redirect(status) {
                current = redirect_target(&current, &response)?;
                continue;
            }
            if !status.is_success() {
                return Err(HttpError::Status {
                    url: current.to_string(),
                    status,
                });
            }
            let body = self.read_body(&current, response).await?;
            return Ok(Fetched { url: current, body });
        }
        Err(HttpError::TooManyRedirects {
            url: url.to_string(),
        })
    }

    /// 本文を `max_body_bytes` まで読む。Content-Length で超過が分かれば読まずに、
    /// 分からなければ受信しながら、上限を超えた時点で打ち切る。
    async fn read_body(
        &self,
        url: &Url,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, HttpError> {
        let too_large = || HttpError::BodyTooLarge {
            url: url.to_string(),
            limit: self.max_body_bytes,
        };
        if response
            .content_length()
            .is_some_and(|n| n > self.max_body_bytes)
        {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| HttpError::Request {
                url: url.to_string(),
                source,
            })?
        {
            if (body.len() + chunk.len()) as u64 > self.max_body_bytes {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn robots_allow(&self, url: &Url) -> bool {
        let origin = url.origin().ascii_serialization();
        let cell = self
            .robots
            .lock()
            .await
            .entry(origin.clone())
            .or_default()
            .clone();
        let rules = cell.get_or_init(|| self.fetch_robots(&origin)).await;
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        rules.allows(&path)
    }

    /// RFC 9309：4xx なら全許可、5xx や通信エラーなら全拒否。
    async fn fetch_robots(&self, origin: &str) -> Rules {
        let Ok(url) = Url::parse(&format!("{origin}/robots.txt")) else {
            return Rules::disallow_all();
        };
        match Box::pin(self.fetch(&url, false)).await {
            Ok(f) => Rules::parse(&String::from_utf8_lossy(&f.body), &self.product),
            Err(HttpError::Status { status, .. }) if status.is_client_error() => Rules::allow_all(),
            Err(e) => {
                tracing::warn!(
                    %origin,
                    "robots.txt unavailable, disallowing: {}",
                    crate::errors::error_chain(&e)
                );
                Rules::disallow_all()
            }
        }
    }

    /// 同じホストへのアクセスが `per_host_delay` 以上空くよう、順番を予約してから待つ。
    /// 予約はロック内で行うので、同時に呼ばれても間隔が保たれる。
    async fn wait_for_turn(&self, url: &Url) {
        let key = format!(
            "{}:{}",
            url.host_str().unwrap_or_default(),
            url.port_or_known_default().unwrap_or_default()
        );
        let start = {
            let mut next = self.next_allowed.lock().await;
            let now = Instant::now();
            let start = next.get(&key).map_or(now, |&t| t.max(now));
            next.insert(key, start + self.per_host_delay);
            start
        };
        tokio::time::sleep_until(start).await;
    }
}

/// 転送を意味するステータスだけ（304 Not Modified や 300 Multiple Choices は含めない）。
fn is_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn redirect_target(from: &Url, response: &reqwest::Response) -> Result<Url, HttpError> {
    let bad = |reason: String| HttpError::BadRedirect {
        url: from.to_string(),
        reason,
    };
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .ok_or_else(|| bad(format!("{} without Location header", response.status())))?
        .to_str()
        .map_err(|e| bad(e.to_string()))?;
    from.join(location).map_err(|e| bad(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Route, Server};

    fn fetcher(delay: Duration) -> Fetcher {
        Fetcher::new("nucrawler-test/1", Duration::from_millis(500), delay, 1024).unwrap()
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[tokio::test]
    async fn returns_body_and_sends_user_agent() {
        let server = Server::start([("/feed", Route::ok("hello"))].into());
        let body = fetcher(Duration::ZERO)
            .get(&url(&server.url("/feed")))
            .await
            .unwrap();
        assert_eq!(body.body, b"hello");
        let reqs = server.requests();
        assert_eq!(reqs[0].user_agent.as_deref(), Some("nucrawler-test/1"));
    }

    #[tokio::test]
    async fn non_success_status_is_error() {
        let server = Server::start([("/forbidden", Route::status(403))].into());
        let err = fetcher(Duration::ZERO)
            .get(&url(&server.url("/forbidden")))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, HttpError::Status { status, .. } if status.as_u16() == 403),
            "{err}"
        );
    }

    /// 巨大な応答でメモリを使い果たさないよう、上限を超えたら読み込みをやめる。
    #[tokio::test]
    async fn body_over_limit_is_error() {
        for omit_length in [false, true] {
            let big = Route {
                omit_length,
                ..Route::ok(vec![b'x'; 2048])
            };
            let fits = Route {
                omit_length,
                ..Route::ok(vec![b'x'; 1024])
            };
            let server = Server::start([("/big", big), ("/fits", fits)].into());
            let f = fetcher(Duration::ZERO);
            let err = f.get(&url(&server.url("/big"))).await.unwrap_err();
            assert!(
                matches!(err, HttpError::BodyTooLarge { limit: 1024, .. }),
                "omit_length={omit_length}: {err}"
            );
            let ok = f.get(&url(&server.url("/fits"))).await.unwrap();
            assert_eq!(ok.body.len(), 1024);
        }
    }

    #[tokio::test]
    async fn slow_response_times_out() {
        let slow = Route {
            delay: Duration::from_secs(3),
            ..Route::ok("late")
        };
        let server = Server::start([("/slow", slow)].into());
        let err = fetcher(Duration::ZERO)
            .get(&url(&server.url("/slow")))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::Request { .. }), "{err}");
    }

    #[tokio::test]
    async fn waits_between_requests_to_same_host() {
        let server = Server::start([("/a", Route::ok("a")), ("/b", Route::ok("b"))].into());
        let f = fetcher(Duration::from_millis(300));
        f.get(&url(&server.url("/a"))).await.unwrap();
        f.get(&url(&server.url("/b"))).await.unwrap();
        let reqs = server.requests();
        let gap = reqs[1].at - reqs[0].at;
        assert!(gap >= Duration::from_millis(280), "{gap:?}");
    }

    /// リダイレクトも自前でたどり、転送先へのアクセスにも間隔を空ける。
    #[tokio::test]
    async fn follows_redirects_with_per_host_delay() {
        let server = Server::start(
            [
                ("/old", Route::redirect("/new")),
                ("/new", Route::ok("moved")),
            ]
            .into(),
        );
        let body = fetcher(Duration::from_millis(300))
            .get(&url(&server.url("/old")))
            .await
            .unwrap();
        assert_eq!(body.body, b"moved");
        assert_eq!(body.url.as_str(), server.url("/new"));
        let reqs = server.requests();
        let paths: Vec<_> = reqs.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["/old", "/new"]);
        let gap = reqs[1].at - reqs[0].at;
        assert!(gap >= Duration::from_millis(280), "{gap:?}");
    }

    /// 304 や 300 は転送ではないので、Location があってもたどらずステータスエラーにする。
    #[tokio::test]
    async fn non_redirect_3xx_is_status_error() {
        for status in [300, 304] {
            let route = Route {
                location: Some("/ok".into()),
                ..Route::status(status)
            };
            let server = Server::start([("/a", route), ("/ok", Route::ok("ok"))].into());
            let err = fetcher(Duration::ZERO)
                .get(&url(&server.url("/a")))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, HttpError::Status { status: s, .. } if s.as_u16() == status),
                "{status}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn redirect_loop_is_error() {
        let server =
            Server::start([("/a", Route::redirect("/b")), ("/b", Route::redirect("/a"))].into());
        let err = fetcher(Duration::ZERO)
            .get(&url(&server.url("/a")))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::TooManyRedirects { .. }), "{err}");
    }

    #[tokio::test]
    async fn redirect_without_location_is_error() {
        let server = Server::start([("/a", Route::status(302))].into());
        let err = fetcher(Duration::ZERO)
            .get(&url(&server.url("/a")))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::BadRedirect { .. }), "{err}");
    }

    #[tokio::test]
    async fn get_page_obeys_robots_txt_and_caches_it() {
        let robots =
            "User-agent: *\nDisallow: /private/\n\nUser-agent: nucrawler-test\nDisallow: /mine/\n";
        let server = Server::start(
            [
                ("/robots.txt", Route::ok(robots)),
                ("/private/a", Route::ok("p")),
                ("/news/1", Route::ok("n1")),
                ("/news/2", Route::ok("n2")),
                ("/mine/x", Route::ok("m")),
            ]
            .into(),
        );
        let f = fetcher(Duration::ZERO);
        // UA の製品名 nucrawler-test のグループが適用される（* のグループは使わない）
        assert_eq!(
            f.get_page(&url(&server.url("/news/1"))).await.unwrap().body,
            b"n1"
        );
        f.get_page(&url(&server.url("/private/a"))).await.unwrap();
        let err = f.get_page(&url(&server.url("/mine/x"))).await.unwrap_err();
        assert!(matches!(err, HttpError::DisallowedByRobots { .. }), "{err}");
        f.get_page(&url(&server.url("/news/2"))).await.unwrap();

        let paths: Vec<_> = server.requests().into_iter().map(|r| r.path).collect();
        assert_eq!(
            paths.iter().filter(|p| *p == "/robots.txt").count(),
            1,
            "{paths:?}"
        );
        assert!(!paths.contains(&"/mine/x".to_string()), "{paths:?}");
    }

    /// 許可されたページから禁止されたページへの転送もたどらない。
    #[tokio::test]
    async fn get_page_checks_robots_for_each_redirect_target() {
        let server = Server::start(
            [
                (
                    "/robots.txt",
                    Route::ok("User-agent: *\nDisallow: /private/\n"),
                ),
                ("/public", Route::redirect("/private/x")),
                ("/private/x", Route::ok("secret")),
            ]
            .into(),
        );
        let err = fetcher(Duration::ZERO)
            .get_page(&url(&server.url("/public")))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::DisallowedByRobots { .. }), "{err}");
        let paths: Vec<_> = server.requests().into_iter().map(|r| r.path).collect();
        assert!(!paths.contains(&"/private/x".to_string()), "{paths:?}");
    }

    /// 遅い robots.txt を待つ間も、別のオリジンの取得は止めない。
    #[tokio::test]
    async fn slow_robots_txt_does_not_block_other_origins() {
        let slow_robots = Route {
            delay: Duration::from_millis(1500),
            ..Route::ok("User-agent: *\nAllow: /\n")
        };
        let slow = Server::start([("/robots.txt", slow_robots), ("/a", Route::ok("a"))].into());
        let fast = Server::start([("/b", Route::ok("b"))].into());
        let f = Fetcher::new("t", Duration::from_secs(3), Duration::ZERO, 1024).unwrap();
        let (slow_url, fast_url) = (url(&slow.url("/a")), url(&fast.url("/b")));
        let started = tokio::time::Instant::now();
        let slow_task = f.get_page(&slow_url);
        let fast_task = async {
            // 遅い側が robots.txt の取得を始めてから呼ぶ
            tokio::time::sleep(Duration::from_millis(200)).await;
            f.get_page(&fast_url).await.unwrap();
            started.elapsed()
        };
        let (slow_result, fast_elapsed) = tokio::join!(slow_task, fast_task);
        slow_result.unwrap();
        assert!(
            fast_elapsed < Duration::from_millis(1000),
            "{fast_elapsed:?}"
        );
    }

    #[tokio::test]
    async fn missing_robots_txt_allows_everything() {
        let server = Server::start([("/a", Route::ok("a"))].into());
        f_ok(&server, "/a").await;
    }

    async fn f_ok(server: &Server, path: &str) {
        fetcher(Duration::ZERO)
            .get_page(&url(&server.url(path)))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn unreachable_robots_txt_disallows_everything() {
        let server =
            Server::start([("/robots.txt", Route::status(503)), ("/a", Route::ok("a"))].into());
        let err = fetcher(Duration::ZERO)
            .get_page(&url(&server.url("/a")))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::DisallowedByRobots { .. }), "{err}");
    }

    #[tokio::test]
    async fn feeds_are_fetched_without_robots_check() {
        let server = Server::start(
            [
                ("/robots.txt", Route::ok("User-agent: *\nDisallow: /\n")),
                ("/feed", Route::ok("f")),
            ]
            .into(),
        );
        fetcher(Duration::ZERO)
            .get(&url(&server.url("/feed")))
            .await
            .unwrap();
        let paths: Vec<_> = server.requests().into_iter().map(|r| r.path).collect();
        assert_eq!(paths, ["/feed"]);
    }

    #[tokio::test]
    async fn concurrent_requests_to_same_host_are_spaced() {
        let routes = [
            ("/a", Route::ok("a")),
            ("/b", Route::ok("b")),
            ("/c", Route::ok("c")),
        ];
        let server = Server::start(routes.into());
        let f = fetcher(Duration::from_millis(200));
        let (a, b, c) = (
            url(&server.url("/a")),
            url(&server.url("/b")),
            url(&server.url("/c")),
        );
        let (ra, rb, rc) = tokio::join!(f.get(&a), f.get(&b), f.get(&c));
        ra.unwrap();
        rb.unwrap();
        rc.unwrap();
        let mut at: Vec<_> = server.requests().iter().map(|r| r.at).collect();
        at.sort();
        for w in at.windows(2) {
            let gap = w[1] - w[0];
            assert!(gap >= Duration::from_millis(180), "{gap:?}");
        }
    }
}
