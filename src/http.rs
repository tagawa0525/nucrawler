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
    #[error("{url} returned {status}")]
    Status {
        url: String,
        status: reqwest::StatusCode,
    },
}

pub struct Fetcher {
    client: reqwest::Client,
    per_host_delay: Duration,
    /// ホストごとの、次にアクセスしてよい時刻
    next_allowed: Mutex<HashMap<String, Instant>>,
}

impl Fetcher {
    pub fn new(
        _user_agent: &str,
        _timeout: Duration,
        _per_host_delay: Duration,
    ) -> Result<Self, HttpError> {
        todo!()
    }

    pub fn from_config(c: &HttpConfig) -> Result<Self, HttpError> {
        Self::new(
            &c.user_agent,
            Duration::from_secs(c.timeout_secs),
            Duration::from_secs(c.per_host_delay_secs),
        )
    }

    /// 2xx 以外はエラーにする。
    pub async fn get(&self, _url: &Url) -> Result<Vec<u8>, HttpError> {
        todo!()
    }
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
        assert_eq!(body, b"hello");
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
