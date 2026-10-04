//! `eval --profile` は候補を embedding で計算するので、`[embedding]` が無ければ黙って候補を落とさずに失敗する。

use std::path::PathBuf;
use std::process::Command;

fn dirs(name: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("nucrawler-eval-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (config, data) = (root.join("config"), root.join("data"));
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(config.join("sources.toml"), "").unwrap();
    (config, data)
}

#[test]
fn eval_profile_needs_embedding() {
    let (config, data) = dirs("no-embedding");
    let candidate = config.join("candidate.toml");
    std::fs::write(&candidate, "[[interest]]\ntopic = \"燃料\"\nweight = 1.0\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_nucrawler"))
        // エラーはログで出すので、テストを流す環境の RUST_LOG（ビルドのサンドボックスなど）に左右されないようにする
        .env("RUST_LOG", "info")
        .arg("--config-dir")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .args(["eval", "--profile"])
        .arg(&candidate)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[embedding]"), "{output:?}");
}

/// 評価がまだ無ければ、embedding の有無より先に評価が要ると知らせる（`crawl --only embed` では直らない）。
#[test]
fn eval_profile_needs_ratings_first() {
    let (config, data) = dirs("no-ratings");
    std::fs::write(
        config.join("config.toml"),
        "[embedding]\nurl = \"http://127.0.0.1:9/v1/embeddings\"\nmodel = \"m\"\n",
    )
    .unwrap();
    let candidate = config.join("candidate.toml");
    std::fs::write(&candidate, "[[interest]]\ntopic = \"燃料\"\nweight = 1.0\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_nucrawler"))
        // エラーはログで出すので、テストを流す環境の RUST_LOG（ビルドのサンドボックスなど）に左右されないようにする
        .env("RUST_LOG", "info")
        .arg("--config-dir")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .args(["eval", "--profile"])
        .arg(&candidate)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no ratings yet"), "{output:?}");
}
