//! LLM の CLI（子プロセス）とのやり取り：プロンプトを stdin に渡し、終わるまでの出力を集める。

use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{LlmError, LlmFailure, LlmResponse, Usage};

/// CLI のバックエンドを 1 回動かす：`command` を起動して `input` を stdin に渡し、出力を `parse`（構造化出力と、
/// それまでに分かった使用量を返す）で読む。時間内に終わらなくても、失敗しても、分かった使用量は残す（次回の
/// 判定やクォータに使う）。
pub(super) async fn call_cli(
    mut command: tokio::process::Command,
    input: &[u8],
    timeout: Duration,
    parse: impl Fn(&str) -> (Result<serde_json::Value, LlmError>, Option<Usage>),
) -> Result<LlmResponse, LlmFailure> {
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // タイムアウトで future を捨てたときに子プロセスも止める。
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| LlmError::Spawn {
            command: command
                .as_std()
                .get_program()
                .to_string_lossy()
                .into_owned(),
            source,
        })?;
    let output = match run(child, input, timeout).await.map_err(LlmError::Io)? {
        Ran::Exited(output) => output,
        Ran::TimedOut { stdout } => {
            let (_, usage) = parse(&String::from_utf8_lossy(&stdout));
            return Err(LlmFailure {
                error: LlmError::Timeout {
                    secs: timeout.as_secs(),
                },
                usage,
            });
        }
    };
    let (parsed, usage) = parse(&String::from_utf8_lossy(&output.stdout));
    match parsed {
        Ok(output) => Ok(LlmResponse { output, usage }),
        // 結果行が無い（起動時に失敗した、途中で落ちた）ときだけ、終了コードと stderr で報告する。
        // 結果行があれば、終了コードに関わらずそちらが結果と原因を正確に表す。
        Err(LlmError::Protocol(_)) if !output.status.success() => Err(LlmFailure {
            error: LlmError::Exit {
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                interrupted: interrupted(output.status),
            },
            usage,
        }),
        // 上限での拒否やエラーの報告、応答の形の崩れでも、分かった使用量は残す
        Err(error) => Err(LlmFailure { error, usage }),
    }
}

/// SIGINT・SIGTERM で終わったか。シグナルで殺された場合と、シグナルを受けて 128 + 番号で
/// 終了した場合の両方を含む。
pub(super) fn interrupted(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    let signal = status
        .signal()
        .or_else(|| status.code().and_then(|c| c.checked_sub(128)));
    matches!(signal, Some(SIGINT | SIGTERM))
}

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
