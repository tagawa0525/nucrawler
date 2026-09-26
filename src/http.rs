//! 行儀の良い HTTP 取得：UA を名乗り、タイムアウトを設け、同じホストへの連続アクセスに間隔を空ける。

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

use crate::config::HttpConfig;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("failed to build http client: {0}")]
    Build(#[source] reqwest::Error),
    #[error("request to {url} failed: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("too many redirects starting from {url}")]
    TooManyRedirects { url: String },
    #[error("bad redirect from {url}: {reason}")]
    BadRedirect { url: String, reason: String },
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
    /// ホストごとの、次にアクセスしてよい時刻
    next_allowed: Mutex<HashMap<String, Instant>>,
}

impl Fetcher {
    pub fn new(
        user_agent: &str,
        timeout: Duration,
        per_host_delay: Duration,
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
            next_allowed: Mutex::new(HashMap::new()),
        })
    }

    pub fn from_config(c: &HttpConfig) -> Result<Self, HttpError> {
        Self::new(
            &c.user_agent,
            Duration::from_secs(c.timeout_secs),
            Duration::from_secs(c.per_host_delay_secs),
        )
    }

    /// リダイレクトは `MAX_REDIRECTS` 回までたどる。最終的な応答が 2xx 以外ならエラーにする。
    pub async fn get(&self, url: &Url) -> Result<Fetched, HttpError> {
        let mut current = url.clone();
        for _ in 0..=MAX_REDIRECTS {
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
            let body = response.bytes().await.map_err(request_error)?;
            return Ok(Fetched {
                url: current,
                body: body.to_vec(),
            });
        }
        Err(HttpError::TooManyRedirects {
            url: url.to_string(),
        })
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
        Fetcher::new("nucrawler-test/1", Duration::from_millis(500), delay).unwrap()
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
