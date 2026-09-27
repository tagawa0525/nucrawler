//! `nucrawler mcp` を起動し、stdio で initialize → tools/list → tools/call を送る。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use nucrawler::config::Lang;
use nucrawler::db::{ArtifactKind, ContentKind, ContentOrigin, Db, NewArticle, NewArtifact};
use serde_json::{Value, json};

/// テストごとの空のデータディレクトリ。
fn data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nucrawler-mcp-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 要約のある記事を 1 件登録し、その ID を返す。
fn seed(data: &std::path::Path) -> i64 {
    let db = Db::open(&data.join("nucrawler.db")).unwrap();
    let id = db
        .insert_article(&NewArticle {
            source_id: "wnn",
            url: "https://e.com/a",
            title: "Original Title",
            lang: Lang::En,
            published_at: None,
        })
        .unwrap()
        .unwrap();
    let body = db
        .insert_content(id, ContentKind::Body, ContentOrigin::Page, "body")
        .unwrap();
    db.insert_artifact(
        &NewArtifact {
            article_id: id,
            kind: ArtifactKind::Digest,
            backend: "claude-cli",
            model: "sonnet",
            prompt_version: 1,
            payload: &json!({
                "title_ja": "和訳の題", "summary_ja": "要約", "points_ja": ["点"],
                "implications_ja": "示唆", "lwr_relevant": true, "topics": ["規制・審査"],
            }),
            inputs: &[body],
        },
        chrono::Utc::now(),
    )
    .unwrap();
    id
}

#[test]
fn stdio_initialize_list_and_call() {
    let data = data_dir();
    let id = seed(&data);
    let config = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut child = Command::new(env!("CARGO_BIN_EXE_nucrawler"))
        .arg("--config-dir")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |message: Value| {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    };
    // stdout の行はすべて JSON-RPC のメッセージでなければならない
    let mut receive = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    };

    send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "e2e", "version": "0"},
        },
    }));
    let init = receive();
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "nucrawler");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let list = receive();
    assert_eq!(list["id"], 2);
    let mut names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["get_article", "search_articles"]);

    send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "get_article", "arguments": {"id": id}},
    }));
    let call = receive();
    assert_eq!(call["id"], 3);
    let result = &call["result"];
    assert_eq!(result["isError"], false, "{call}");
    assert_eq!(result["structuredContent"]["url"], "https://e.com/a");
    assert_eq!(
        result["structuredContent"]["digest"]["payload"]["title_ja"],
        "和訳の題"
    );

    // stdin を閉じると正常に終了する
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    let _ = std::fs::remove_dir_all(&data);
}
