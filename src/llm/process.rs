//! LLM の CLI（子プロセス）とのやり取り：プロンプトを stdin に渡し、終わるまでの出力を集める。

use std::process::Output;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 子プロセスの終わり方。
pub(super) enum Ran {
    Exited(Output),
    /// 時間内に終わらなかった（子プロセスは止めた）。それまでに出した stdout を残す
    /// （使用量を知らせた後に止まっても、その使用量を失わないように）
    TimedOut {
        stdout: Vec<u8>,
    },
}

/// `child`（stdin・stdout・stderr がパイプのもの）に `input` を渡し、`timeout` まで終わるのを待つ。
pub(super) async fn run(
    mut child: tokio::process::Child,
    input: &[u8],
    timeout: Duration,
) -> std::io::Result<Ran> {
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut stdout_pipe = child.stdout.take().expect("stdout is piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr is piped");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let finished = tokio::time::timeout(timeout, async {
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
        let (written, out, err) = tokio::join!(
            write,
            stdout_pipe.read_to_end(&mut stdout),
            stderr_pipe.read_to_end(&mut stderr)
        );
        written?;
        out?;
        err?;
        child.wait().await
    })
    .await;
    match finished {
        Ok(status) => Ok(Ran::Exited(Output {
            status: status?,
            stdout,
            stderr,
        })),
        Err(_) => {
            // 止めて回収まで待つ（ゾンビを残さず、呼び出し側の後片付けを子プロセスの終了後にする）。
            // 読みかけの stdout は、読み取りを捨てても読んだ分が残る
            if let Err(e) = child.kill().await {
                tracing::warn!("failed to kill the timed-out llm process: {e}");
            }
            Ok(Ran::TimedOut { stdout })
        }
    }
}
