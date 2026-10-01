//! 同時実行の制御。
//!
//! - 取得（fetch・extract）は 1 つずつ（`fetch.lock` を排他）。同じホストへの間隔を守るため
//! - LLM を呼ぶ処理は並行してよい（ファイルのロックは取らない）。同じ記事の二重処理は作業の予約
//!   （`work_claims`）で、ほかの実行の呼び出しはクォータの判定のたびに DB の使用率を読むことで防ぐ
//! - 同時に動く LLM の CLI（claude・copilot）の数は、呼び出しの枠（`llm-slot-N.lock`）でプロセスをまたいで数える
//! - 語彙の整理は 1 つずつ（`tidy.lock` を排他）

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Cancel;

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another nucrawler process holds {path}")]
    Held { path: PathBuf },
    #[error("failed to open lock file {path}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// どの処理のロックか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    /// fetch・extract（1 つずつ）
    Fetch,
    /// LLM を呼ぶステージ（並行してよいので、ファイルのロックは取らない）
    Llm,
    /// 語彙の整理（1 つずつ。LLM を呼ぶほかの実行とは並行する）
    Tidy,
}

/// 取得したロック。drop すると解放される（プロセスが落ちても OS が解放する）。`Llm` はファイルを持たない。
#[derive(Debug)]
pub struct Lock {
    _file: Option<File>,
}

/// `dir` にある `kind` のロックを待たずに取る。既に取られていれば `Held`。`Llm` は何も取らず、
/// ほかの実行とは作業の予約と呼び出しの枠で分ける。
pub fn acquire(dir: &Path, kind: LockKind) -> Result<Lock, LockError> {
    let name = match kind {
        LockKind::Fetch => "fetch.lock",
        LockKind::Llm => return Ok(Lock { _file: None }),
        LockKind::Tidy => "tidy.lock",
    };
    Ok(Lock {
        _file: Some(try_lock(dir, name)?),
    })
}

/// 呼び出しの枠。drop すると空く。
#[derive(Debug)]
pub struct Slot {
    _file: File,
}

/// `dir` にある `n` 個の呼び出しの枠（`llm-slot-N.lock`）のうち空いているものを取る。すべて
/// 埋まっていれば、間隔を置いて空くまで待つ（future を捨てれば待つのをやめる）。
pub async fn acquire_slot(dir: &Path, n: usize) -> Result<Slot, LockError> {
    loop {
        for i in 0..n {
            match try_lock(dir, &format!("llm-slot-{i}.lock")) {
                Ok(file) => return Ok(Slot { _file: file }),
                Err(LockError::Held { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

/// `dir` の `name` のロックを、待たずに排他で取る。
fn try_lock(dir: &Path, name: &str) -> Result<File, LockError> {
    let (path, file) = open(dir, name)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockError::Held { path }),
        Err(std::fs::TryLockError::Error(source)) => Err(LockError::Io { path, source }),
    }
}

/// `dir` にある `kind` のロックを、取れるまで待って取る。待っている間に中断を要求されたら
/// （systemd の停止など）取らずに `None` を返す。スレッドをブロックして待つと、止められても
/// 待ち続けてしまうので、間隔を置いて取り直す。
pub async fn acquire_waiting(
    dir: &Path,
    kind: LockKind,
    cancel: &Cancel,
) -> Result<Option<Lock>, LockError> {
    loop {
        match acquire(dir, kind) {
            Err(LockError::Held { .. }) => {
                tokio::select! {
                    () = tokio::time::sleep(RETRY_INTERVAL) => {}
                    () = cancel.requested() => return Ok(None),
                }
            }
            lock => return lock.map(Some),
        }
    }
}

const RETRY_INTERVAL: Duration = Duration::from_secs(1);

fn open(dir: &Path, name: &str) -> Result<(PathBuf, File), LockError> {
    let path = dir.join(name);
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|source| LockError::Io {
            path: path.clone(),
            source,
        })?;
    Ok((path, file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nucrawler-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn second_acquire_fails_until_first_is_dropped() {
        let dir = temp_dir("lock");
        let first = acquire(&dir, LockKind::Fetch).unwrap();
        let err = acquire(&dir, LockKind::Fetch).unwrap_err();
        assert!(matches!(err, LockError::Held { .. }), "{err}");
        drop(first);
        // 並行するテストが子プロセスを fork すると、exec までの一瞬だけロックの fd を引き継ぎ、
        // 解放後もロックが残って見える（flock はオープンファイル記述単位）。その間だけ待つ。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match acquire(&dir, LockKind::Fetch) {
                Ok(_) => break,
                Err(LockError::Held { .. }) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// timer から起動した実行は、実行中の別の実行が終わるのを待ってから始める。
    #[tokio::test]
    async fn acquire_waiting_waits_until_released() {
        let dir = temp_dir("lock-wait");
        let first = acquire(&dir, LockKind::Fetch).unwrap();
        let cancel = Cancel::default();
        let waiting = acquire_waiting(&dir, LockKind::Fetch, &cancel);
        tokio::pin!(waiting);
        let short = std::time::Duration::from_millis(200);
        let early = tokio::time::timeout(short, &mut waiting).await;
        assert!(early.is_err(), "must wait while held");
        drop(first);
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .unwrap();
        assert!(matches!(got, Ok(Some(_))), "{got:?}");
    }

    /// 待っている間に止められたら（systemd の停止など）、ロックを取らずに終わる。
    #[tokio::test]
    async fn acquire_waiting_stops_when_cancelled() {
        let dir = temp_dir("lock-wait-cancel");
        let _first = acquire(&dir, LockKind::Fetch).unwrap();
        let cancel = Cancel::default();
        let waiting = acquire_waiting(&dir, LockKind::Fetch, &cancel);
        tokio::pin!(waiting);
        let short = std::time::Duration::from_millis(200);
        assert!(tokio::time::timeout(short, &mut waiting).await.is_err());
        cancel.request();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .unwrap();
        assert!(matches!(got, Ok(None)), "{got:?}");
    }

    /// 取得と LLM のステージは互いのロックを待たない。
    #[test]
    fn kinds_do_not_block_each_other() {
        let dir = temp_dir("lock-kinds");
        let _fetch = acquire(&dir, LockKind::Fetch).unwrap();
        let _llm = acquire(&dir, LockKind::Llm).unwrap();
        let err = acquire(&dir, LockKind::Fetch).unwrap_err();
        assert!(matches!(err, LockError::Held { .. }), "{err}");
    }

    #[test]
    fn missing_dir_is_io_error() {
        let dir = temp_dir("lock-missing").join("nope");
        let err = acquire(&dir, LockKind::Fetch).unwrap_err();
        assert!(matches!(err, LockError::Io { .. }), "{err}");
    }

    /// LLM を呼ぶ実行どうしは待たない（同じ記事は作業の予約で分ける）。
    #[test]
    fn llm_runs_do_not_block_each_other() {
        let dir = temp_dir("lock-llm");
        let _first = acquire(&dir, LockKind::Llm).unwrap();
        let _second = acquire(&dir, LockKind::Llm).unwrap();
    }

    /// 語彙の整理は同時に 1 つだけ。LLM を呼ぶほかの実行とは並行する。
    #[test]
    fn tidy_runs_one_at_a_time_alongside_other_llm_runs() {
        let dir = temp_dir("lock-tidy");
        let _llm = acquire(&dir, LockKind::Llm).unwrap();
        let _tidy = acquire(&dir, LockKind::Tidy).unwrap();
        let err = acquire(&dir, LockKind::Tidy).unwrap_err();
        assert!(matches!(err, LockError::Held { .. }), "{err}");
    }

    /// 呼び出しの枠は `n` 個まで。空けば次が取れる。
    #[tokio::test]
    async fn slots_limit_concurrent_calls() {
        let dir = temp_dir("lock-slots");
        let short = std::time::Duration::from_millis(200);
        let first = tokio::time::timeout(short, acquire_slot(&dir, 2))
            .await
            .unwrap()
            .unwrap();
        let _second = tokio::time::timeout(short, acquire_slot(&dir, 2))
            .await
            .unwrap()
            .unwrap();
        let third = acquire_slot(&dir, 2);
        tokio::pin!(third);
        assert!(
            tokio::time::timeout(short, &mut third).await.is_err(),
            "must wait"
        );
        drop(first);
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), third)
            .await
            .unwrap();
        assert!(got.is_ok(), "{got:?}");
    }
}
