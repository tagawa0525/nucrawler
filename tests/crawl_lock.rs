//! 取得（fetch・extract）と LLM のステージは別のロックを取り、互いを待たずに並行して動ける。

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
fn extract_runs_while_llm_stages_hold_their_lock() {
    let (config, data) = dirs("extract-vs-llm");
    let _llm = hold(&data, "llm.lock");
    assert_eq!(crawl(&config, &data, &["--only", "extract"]), Some(0));
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

/// LLM を呼ぶ実行どうしは待たない（同じ記事は作業の予約で分ける）。
#[test]
fn requested_translations_run_alongside_other_llm_runs() {
    let (config, data) = dirs("requests-vs-llm");
    let other = File::create(data.join("llm.lock")).unwrap();
    other.try_lock_shared().unwrap();
    assert_eq!(crawl(&config, &data, &["--requests-only"]), Some(0));
}

/// 更新前の版の LLM の実行（llm.lock を排他で取る）とは重ならない。
#[test]
fn requested_translations_wait_for_llm_runs_of_the_previous_version() {
    let (config, data) = dirs("requests-vs-previous-llm");
    let _llm = hold(&data, "llm.lock");
    assert_eq!(crawl(&config, &data, &["--requests-only"]), Some(75));
}

/// 語彙の整理は同時に 1 つだけ。
#[test]
fn tidy_waits_for_another_tidy() {
    let (config, data) = dirs("tidy-vs-tidy");
    let _tidy = hold(&data, "tidy.lock");
    assert_eq!(crawl(&config, &data, &["--only", "tidy"]), Some(75));
}

/// 更新前の版の crawl（`crawl.lock` を排他で取る）が動いている間は、どのステージも始めない。
#[test]
fn waits_for_a_crawl_of_the_previous_version() {
    let (config, data) = dirs("legacy");
    let _legacy = hold(&data, "crawl.lock");
    assert_eq!(crawl(&config, &data, &["--only", "extract"]), Some(75));
    assert_eq!(crawl(&config, &data, &["--requests-only"]), Some(75));
    // 古い版が使っている DB にマイグレーションを当てない
    assert!(!data.join("nucrawler.db").exists());
}
