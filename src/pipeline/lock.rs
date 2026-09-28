//! 同時実行の防止。timer から起動した crawl と手動の crawl・redo が重ならないようにする。
//! 取得（fetch・extract）と LLM を呼ぶ処理は別のロックを取り、互いを待たずに並行して動ける。
//! LLM を呼ぶ処理を 1 つずつにするのは、同じ記事を二重に処理せず、クォータの判定が他の
//! 実行の呼び出しを見落とさないようにするため。取得は LLM を使わず、記事と本文を書くだけ。

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
    /// fetch・extract
    Fetch,
    /// LLM を呼ぶステージと redo・suggest・eval の採点
    Llm,
}

impl LockKind {
    fn file_name(self) -> &'static str {
        match self {
            LockKind::Fetch => "fetch.lock",
            LockKind::Llm => "llm.lock",
        }
    }
}

/// 取得したロック。drop すると解放される（プロセスが落ちても OS が解放する）。
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

/// `dir` にある `kind` のロックを待たずに取る。既に取られていれば `Held`。
pub fn acquire(dir: &Path, kind: LockKind) -> Result<Lock, LockError> {
    let (path, file) = open(dir, kind)?;
    match file.try_lock() {
        Ok(()) => Ok(Lock { _file: file }),
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

fn open(dir: &Path, kind: LockKind) -> Result<(PathBuf, File), LockError> {
    let path = dir.join(kind.file_name());
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
        let first = acquire(&dir, LockKind::Llm).unwrap();
        let err = acquire(&dir, LockKind::Llm).unwrap_err();
        assert!(matches!(err, LockError::Held { .. }), "{err}");
        drop(first);
        // 並行するテストが子プロセスを fork すると、exec までの一瞬だけロックの fd を引き継ぎ、
        // 解放後もロックが残って見える（flock はオープンファイル記述単位）。その間だけ待つ。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match acquire(&dir, LockKind::Llm) {
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
        let first = acquire(&dir, LockKind::Llm).unwrap();
        let cancel = Cancel::default();
        let waiting = acquire_waiting(&dir, LockKind::Llm, &cancel);
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
        let _first = acquire(&dir, LockKind::Llm).unwrap();
        let cancel = Cancel::default();
        let waiting = acquire_waiting(&dir, LockKind::Llm, &cancel);
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

    /// 更新前の版の実行（`crawl.lock` を排他で取る）とは、どちらのロックも重ならない。
    #[test]
    fn waits_for_a_run_of_the_previous_version() {
        let dir = temp_dir("lock-legacy");
        let legacy = File::create(dir.join("crawl.lock")).unwrap();
        legacy.try_lock().unwrap();
        for kind in [LockKind::Fetch, LockKind::Llm] {
            let err = acquire(&dir, kind).unwrap_err();
            assert!(matches!(err, LockError::Held { .. }), "{err}");
        }
    }

    #[test]
    fn missing_dir_is_io_error() {
        let dir = temp_dir("lock-missing").join("nope");
        let err = acquire(&dir, LockKind::Llm).unwrap_err();
        assert!(matches!(err, LockError::Io { .. }), "{err}");
    }
}
