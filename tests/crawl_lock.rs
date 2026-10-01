//! 取得（fetch・extract）は `fetch.lock`、語彙の整理は `tidy.lock` で 1 つずつ動く。LLM のステージはロックを
//! 取らないので、取得の最中でも動ける。

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

/// テストごとの設定ディレクトリ（ソースは空）とデータディレクトリ。
fn dirs(name: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("nucrawler-lock-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (config, data) = (root.join("config"), root.join("data"));
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(config.join("sources.toml"), "").unwrap();
    (config, data)
}

/// 別のプロセスが実行中であるかのように、`data/<file>` のロックを取っておく。
fn hold(data: &Path, file: &str) -> File {
    let lock = File::create(data.join(file)).unwrap();
    lock.try_lock().unwrap();
    lock
}

fn crawl(config: &Path, data: &Path, args: &[&str]) -> Option<i32> {
    Command::new(env!("CARGO_BIN_EXE_nucrawler"))
        .arg("--config-dir")
        .arg(config)
        .arg("--data-dir")
        .arg(data)
        .arg("crawl")
        .args(args)
        .output()
        .unwrap()
        .status
        .code()
}

#[test]
fn extract_waits_for_another_fetch() {
    let (config, data) = dirs("extract-vs-fetch");
    let _fetch = hold(&data, "fetch.lock");
    assert_eq!(crawl(&config, &data, &["--only", "extract"]), Some(75));
}

#[test]
fn requested_translations_run_while_fetching() {
    let (config, data) = dirs("requests-vs-fetch");
    let _fetch = hold(&data, "fetch.lock");
    assert_eq!(crawl(&config, &data, &["--requests-only"]), Some(0));
}

/// 語彙の整理は同時に 1 つだけ。
#[test]
fn tidy_waits_for_another_tidy() {
    let (config, data) = dirs("tidy-vs-tidy");
    let _tidy = hold(&data, "tidy.lock");
    assert_eq!(crawl(&config, &data, &["--only", "tidy"]), Some(75));
}
