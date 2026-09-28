//! 同時実行の防止。timer から起動した crawl と手動の crawl が重ならないようにする。

use std::fs::File;
use std::path::{Path, PathBuf};

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

/// 取得したロック。drop すると解放される（プロセスが落ちても OS が解放する）。
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

/// `dir/crawl.lock` の排他ロックを待たずに取る。既に取られていれば `Held`。
pub fn acquire(dir: &Path) -> Result<Lock, LockError> {
    let (path, file) = open(dir)?;
    match file.try_lock() {
        Ok(()) => Ok(Lock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockError::Held { path }),
        Err(std::fs::TryLockError::Error(source)) => Err(LockError::Io { path, source }),
    }
}

/// `dir/crawl.lock` の排他ロックを、取れるまで待って取る（スレッドをブロックする）。
pub fn acquire_waiting(dir: &Path) -> Result<Lock, LockError> {
    let (path, file) = open(dir)?;
    file.lock()
        .map_err(|source| LockError::Io { path, source })?;
    Ok(Lock { _file: file })
}

fn open(dir: &Path) -> Result<(PathBuf, File), LockError> {
    let path = dir.join("crawl.lock");
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
    use crate::pipeline::Cancel;

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

    #[test]
    fn missing_dir_is_io_error() {
        let dir = temp_dir("lock-missing").join("nope");
        let err = acquire(&dir, LockKind::Llm).unwrap_err();
        assert!(matches!(err, LockError::Io { .. }), "{err}");
    }
}
