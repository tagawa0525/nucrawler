//! テスト用の最小限の HTTP サーバ（std のみ）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Route {
    pub status: u16,
    pub body: Vec<u8>,
    /// 応答を返すまでの待ち時間（タイムアウトの確認用）
    pub delay: Duration,
    /// リダイレクト先（Location ヘッダ）
    pub location: Option<String>,
    /// Content-Length を付けず、接続を閉じて本文の終わりを示す
    pub omit_length: bool,
    pub content_type: &'static str,
}

impl Route {
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            delay: Duration::ZERO,
            location: None,
            omit_length: false,
            content_type: "text/plain; charset=utf-8",
        }
    }

    pub fn redirect(location: &str) -> Self {
        Self {
            location: Some(location.to_string()),
            ..Self::status(302)
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
            delay: Duration::ZERO,
            location: None,
            omit_length: false,
            content_type: "text/plain; charset=utf-8",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub path: String,
    pub user_agent: Option<String>,
    pub at: Instant,
}

pub struct Server {
    pub base: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    /// 未登録のパスには 404 を返す。
    pub fn start(routes: HashMap<&str, Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Arc<HashMap<String, Route>> = Arc::new(
            routes
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let routes = routes.clone();
                let recorded = recorded.clone();
                std::thread::spawn(move || handle(stream, &routes, &recorded));
            }
        });
        Self { base, requests }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn handle(stream: TcpStream, routes: &HashMap<String, Route>, recorded: &Mutex<Vec<Request>>) {
    let at = Instant::now();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    let mut user_agent = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("user-agent")
        {
            user_agent = Some(v.trim().to_string());
        }
    }
    recorded.lock().unwrap().push(Request {
        path: path.clone(),
        user_agent,
        at,
    });
    let route = routes.get(&path).cloned().unwrap_or(Route::status(404));
    std::thread::sleep(route.delay);
    let mut stream = stream;
    let location = route
        .location
        .as_deref()
        .map(|l| format!("Location: {l}\r\n"))
        .unwrap_or_default();
    let length = if route.omit_length {
        String::new()
    } else {
        format!("Content-Length: {}\r\n", route.body.len())
    };
    let head = format!(
        "HTTP/1.1 {} X\r\n{location}{length}Content-Type: {}\r\nConnection: close\r\n\r\n",
        route.status, route.content_type
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&route.body);
}

pub fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
