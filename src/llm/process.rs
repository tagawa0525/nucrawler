//! LLM の CLI（子プロセス）とのやり取り：プロンプトを stdin に渡し、終わるまでの出力を集める。

use std::process::Output;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::LlmError;

/// `child`（stdin・stdout・stderr がパイプのもの）に `input` を渡し、`timeout` まで終わるのを待つ。
pub(super) async fn run(
    mut child: tokio::process::Child,
    input: &[u8],
    timeout: Duration,
) -> Result<Output, LlmError> {
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let run = async {
        // 書き込みと読み取りを並行させ、パイプが詰まって互いに待ち続けないようにする。
        let write = async {
            match stdin.write_all(input).await {
                // 子が入力を読まずに終了した（認証エラーなど）。原因は終了コードと stderr で報告する
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                r => r?,
            }
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let (written, output) = tokio::join!(write, child.wait_with_output());
        written?;
        output
    };
    tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| LlmError::Timeout {
            secs: timeout.as_secs(),
        })?
        .map_err(LlmError::Io)
}
