//! プロセスをまたぐ排他に使う、ファイルのロック（flock）。ロックの名前と使い方は、使う側
//! （実行のロックは `pipeline::lock`、LLM の呼び出しの枠は `llm::slot`）で決める。

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// 取られているロックを取り直すまでの間隔。
pub const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// `dir` の `name` のロックを、待たずに排他で取る。返したファイルを drop すると解放される
/// （プロセスが落ちても OS が解放する）。
pub fn try_lock(dir: &Path, name: &str) -> Result<File, LockError> {
    let (path, file) = open(dir, name)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockError::Held { path }),
        Err(std::fs::TryLockError::Error(source)) => Err(LockError::Io { path, source }),
    }
}

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
