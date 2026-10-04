//! 呼び出しの枠：同時に動く LLM の CLI（claude・copilot）の数を、ファイルのロック（`llm-slot-N.lock`）で
//! プロセスをまたいで数える。

use std::fs::File;
use std::path::Path;

use crate::filelock::{LockError, RETRY_INTERVAL, try_lock};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

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
