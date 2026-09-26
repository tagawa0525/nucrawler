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
pub fn acquire(_dir: &Path) -> Result<Lock, LockError> {
    todo!()
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
        let first = acquire(&dir).unwrap();
        let err = acquire(&dir).unwrap_err();
        assert!(matches!(err, LockError::Held { .. }), "{err}");
        drop(first);
        acquire(&dir).unwrap();
    }

    #[test]
    fn missing_dir_is_io_error() {
        let dir = temp_dir("lock-missing").join("nope");
        let err = acquire(&dir).unwrap_err();
        assert!(matches!(err, LockError::Io { .. }), "{err}");
    }
}
