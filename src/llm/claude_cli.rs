//! `claude -p` を子プロセスとして呼ぶバックエンド。
//!
//! - `--bare` は API キー認証を強制するので使わない（サブスクの OAuth で動かすため）。
//! - ツールをすべて無効にし、MCP・スラッシュコマンド・設定ファイルを読まない。
//! - cwd は中立なディレクトリにして、プロジェクトの CLAUDE.md などを読ませない。
//! - 出力は stream-json。`rate_limit_event` から使用率を、`result` から構造化出力を得る。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::{Llm, LlmError, LlmRequest, LlmResponse, RateLimit, Window};

pub struct ClaudeCli {
    pub command: PathBuf,
    /// 子プロセスの作業ディレクトリ（無ければ作る）
    pub cwd: PathBuf,
    pub timeout: Duration,
}

impl Llm for ClaudeCli {
    fn backend(&self) -> &'static str {
        "claude-cli"
    }

    async fn call(&self, req: LlmRequest<'_>) -> Result<LlmResponse, LlmError> {
        std::fs::create_dir_all(&self.cwd).map_err(LlmError::Io)?;
        let schema = req.schema.to_string();
        let mut child = tokio::process::Command::new(&self.command)
            .args(["-p", "--output-format", "stream-json", "--verbose"])
            .args(["--json-schema", &schema])
            .args(["--tools", ""])
            .args([
                "--no-session-persistence",
                "--strict-mcp-config",
                "--disable-slash-commands",
            ])
            .args(["--setting-sources", ""])
            .args(["--system-prompt", req.system])
            .args(["--model", req.model])
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // タイムアウトや中断で future を捨てたときに子プロセスも止める。
            .kill_on_drop(true)
            // 端末の Ctrl-C（プロセスグループへの SIGINT）を直接受けないようにする。止めるときは
            // nucrawler が future を捨てて止める
            .process_group(0)
            .spawn()
            .map_err(|source| LlmError::Spawn {
                command: self.command.display().to_string(),
                source,
            })?;
        // claude は自分のプロセスグループの長なので、グループ ID はその PID
        let _group = child.id().map(ProcessGroup);
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let prompt = req.prompt.as_bytes();
        let run = async {
            // 書き込みと読み取りを並行させ、パイプが詰まって互いに待ち続けないようにする。
            let write = async {
                match stdin.write_all(prompt).await {
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
        let output = tokio::time::timeout(self.timeout, run)
            .await
            .map_err(|_| LlmError::Timeout {
                secs: self.timeout.as_secs(),
            })?
            .map_err(LlmError::Io)?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        match parse_stream(&stdout) {
            // 結果行が無い（途中で落ちた）ときだけ、終了コードと stderr で報告する。
            // 結果行があれば、終了コードに関わらずそちらが結果と原因を正確に表す。
            Err(LlmError::Protocol(_)) if !output.status.success() => Err(LlmError::Exit {
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }),
            parsed => {
                let (output, rate_limit) = parsed?;
                Ok(LlmResponse { output, rate_limit })
            }
        }
    }
}

/// claude のプロセスグループ。drop されたら（完了・タイムアウト・中断のいずれでも）グループ全体を
/// 止め、claude が起動した子プロセスを残さない（`kill_on_drop` は claude 本体しか止めない）。
struct ProcessGroup(u32);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let Ok(pgid) = libc::pid_t::try_from(self.0) else {
            return;
        };
        // SAFETY: killpg はシグナルを送るだけで、メモリを扱わない。グループが既に無ければ
        // ESRCH で失敗するだけなので、結果は見ない
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
}

/// stream-json の出力から、構造化出力と最後の使用率を取り出す。
pub fn parse_stream(stdout: &str) -> Result<(serde_json::Value, Option<RateLimit>), LlmError> {
    let mut rate_limit = None;
    let mut rejected = None;
    let mut result = None;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let event: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| LlmError::Protocol(format!("invalid json line ({e}): {line}")))?;
        match event["type"].as_str() {
            Some("rate_limit_event") => {
                let info = &event["rate_limit_info"];
                rate_limit = Some(parse_rate_limit(info));
                // 最後のイベントの状態で判断する（途中で拒否されても、後で許可されれば上限ではない）。
                rejected = (info["status"] == "rejected").then(|| info["resetsAt"].as_i64());
            }
            Some("result") => result = Some(event),
            _ => {}
        }
    }
    let result = result.ok_or_else(|| LlmError::Protocol("no result event".into()))?;
    if result["is_error"].as_bool().unwrap_or(false) {
        if let Some(resets_at) = rejected {
            return Err(LlmError::RateLimited {
                resets_at,
                rate_limit,
            });
        }
        return Err(LlmError::Reported {
            subtype: result["subtype"].as_str().unwrap_or_default().to_string(),
            message: result["result"].as_str().unwrap_or_default().to_string(),
        });
    }
    match result.get("structured_output") {
        Some(output) if !output.is_null() => Ok((output.clone(), rate_limit)),
        _ => Err(LlmError::NoStructuredOutput),
    }
}

fn parse_rate_limit(info: &serde_json::Value) -> RateLimit {
    let window = |name: &str| {
        let w = &info["unifiedWindows"][name];
        Some(Window {
            utilization: w["utilization"].as_f64()?,
            resets_at: w["resetsAt"].as_i64()?,
        })
    };
    RateLimit {
        five_hour: window("five_hour"),
        seven_day: window("seven_day"),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn fixture() -> String {
        String::from_utf8(crate::testutil::fixture("claude-success.jsonl")).unwrap()
    }

    #[test]
    fn parses_structured_output_and_rate_limit() {
        let (output, rate) = parse_stream(&fixture()).unwrap();
        assert_eq!(
            output,
            serde_json::json!({"items": [{"id": 1, "ok": true}]})
        );
        assert_eq!(
            rate,
            Some(RateLimit {
                five_hour: Some(Window {
                    utilization: 0.12,
                    resets_at: 1790457000
                }),
                seven_day: Some(Window {
                    utilization: 0.06,
                    resets_at: 1790650800
                }),
            })
        );
    }

    fn result_line(is_error: bool, subtype: &str, result: &str) -> String {
        serde_json::json!({"type": "result", "subtype": subtype, "is_error": is_error, "result": result})
            .to_string()
    }

    #[test]
    fn reported_error_carries_message() {
        let err = parse_stream(&result_line(
            true,
            "error_during_execution",
            "Not logged in",
        ))
        .unwrap_err();
        assert!(
            matches!(&err, LlmError::Reported { message, .. } if message == "Not logged in"),
            "{err}"
        );
    }

    #[test]
    fn rejected_rate_limit_is_rate_limited() {
        let rate = serde_json::json!({"type": "rate_limit_event", "rate_limit_info": {
            "status": "rejected", "resetsAt": 1790457000, "rateLimitType": "five_hour",
            "unifiedWindows": {"five_hour": {"utilization": 1.0, "resetsAt": 1790457000}}}});
        let out = format!(
            "{rate}\n{}\n",
            result_line(true, "error", "You've hit your session limit")
        );
        let err = parse_stream(&out).unwrap_err();
        let LlmError::RateLimited {
            resets_at,
            rate_limit,
        } = err
        else {
            panic!("{err}");
        };
        assert_eq!(resets_at, Some(1790457000));
        // 拒否されたときの使用率も失わない
        assert_eq!(
            rate_limit.and_then(|r| r.five_hour).map(|w| w.utilization),
            Some(1.0)
        );
    }

    /// 判定には最後の rate_limit_event を使う。
    #[test]
    fn later_allowed_event_clears_rejection() {
        let event = |status: &str| {
            serde_json::json!({"type": "rate_limit_event", "rate_limit_info": {
                "status": status, "resetsAt": 1790457000,
                "unifiedWindows": {"five_hour": {"utilization": 0.9, "resetsAt": 1790457000}}}})
        };
        let out = format!(
            "{}\n{}\n{}\n",
            event("rejected"),
            event("allowed"),
            result_line(true, "error_during_execution", "boom")
        );
        let err = parse_stream(&out).unwrap_err();
        assert!(matches!(err, LlmError::Reported { .. }), "{err}");
    }

    #[test]
    fn missing_result_or_output_is_error() {
        let err = parse_stream("{\"type\":\"system\"}\n").unwrap_err();
        assert!(matches!(err, LlmError::Protocol(_)), "{err}");
        let err = parse_stream(&result_line(false, "success", "plain text")).unwrap_err();
        assert!(matches!(err, LlmError::NoStructuredOutput), "{err}");
        let err = parse_stream("not json\n").unwrap_err();
        assert!(matches!(err, LlmError::Protocol(_)), "{err}");
    }

    /// テストごとの一時ディレクトリに、偽の claude（シェルスクリプト）を置く。
    fn fake_claude(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nucrawler-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        let probe = "[ \"$1\" = --nucrawler-probe ] && exit 0";
        std::fs::write(&script, format!("#!/bin/sh\n{probe}\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // 並行するテストが fork した直後の子プロセスは、exec するまで書き込み用の fd を
        // 引き継いでいる。その間に実行すると ETXTBSY になるので、実行できるまで待つ。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match std::process::Command::new(&script)
                .arg("--nucrawler-probe")
                .status()
            {
                Ok(_) => break,
                Err(e)
                    if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("cannot run {}: {e}", script.display()),
            }
        }
        (script, dir)
    }

    fn request(schema: &serde_json::Value) -> LlmRequest<'_> {
        LlmRequest {
            system: "SYSTEM",
            prompt: "PROMPT 日本語",
            schema,
            model: "sonnet",
        }
    }

    #[tokio::test]
    async fn passes_arguments_and_stdin_and_parses_output() {
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude-success.jsonl");
        let (script, dir) = fake_claude(
            "cli-ok",
            &format!(
                "printf '%s\\n' \"$@\" > \"$PWD/args.txt\"\ncat > \"$PWD/stdin.txt\"\ncat '{}'",
                fixture_path.display()
            ),
        );
        let cwd = dir.join("cwd");
        let cli = ClaudeCli {
            command: script,
            cwd: cwd.clone(),
            timeout: Duration::from_secs(10),
        };
        let schema = serde_json::json!({"type": "object"});
        let resp = cli.call(request(&schema)).await.unwrap();
        assert_eq!(resp.output["items"][0]["id"], 1);
        assert!(resp.rate_limit.is_some());

        let args: Vec<String> = std::fs::read_to_string(cwd.join("args.txt"))
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        let after = |flag: &str| {
            let i = args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} in {args:?}"));
            args[i + 1].clone()
        };
        assert!(args.contains(&"-p".to_string()), "{args:?}");
        assert_eq!(after("--output-format"), "stream-json");
        assert!(args.contains(&"--verbose".to_string()));
        assert_eq!(after("--json-schema"), schema.to_string());
        assert_eq!(after("--tools"), "");
        assert_eq!(after("--model"), "sonnet");
        assert_eq!(after("--system-prompt"), "SYSTEM");
        assert_eq!(after("--setting-sources"), "");
        for flag in [
            "--no-session-persistence",
            "--strict-mcp-config",
            "--disable-slash-commands",
        ] {
            assert!(args.contains(&flag.to_string()), "{flag} in {args:?}");
        }
        assert!(!args.contains(&"--bare".to_string()));
        assert_eq!(
            std::fs::read_to_string(cwd.join("stdin.txt")).unwrap(),
            "PROMPT 日本語"
        );
    }

    /// 結果行が成功していれば、終了コードが 0 以外でも結果を使う。
    #[tokio::test]
    async fn result_line_wins_over_nonzero_exit() {
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude-success.jsonl");
        let (script, dir) = fake_claude(
            "cli-exit-with-result",
            &format!("cat >/dev/null\ncat '{}'\nexit 1", fixture_path.display()),
        );
        let cli = ClaudeCli {
            command: script,
            cwd: dir.join("cwd"),
            timeout: Duration::from_secs(10),
        };
        let schema = serde_json::json!({});
        let resp = cli.call(request(&schema)).await.unwrap();
        assert_eq!(resp.output["items"][0]["id"], 1);
        assert!(resp.rate_limit.is_some());
    }

    #[tokio::test]
    async fn nonzero_exit_reports_stderr() {
        let (script, dir) = fake_claude("cli-exit", "echo boom >&2\nexit 3");
        let cli = ClaudeCli {
            command: script,
            cwd: dir.join("cwd"),
            timeout: Duration::from_secs(10),
        };
        let schema = serde_json::json!({});
        let err = cli.call(request(&schema)).await.unwrap_err();
        assert!(
            matches!(&err, LlmError::Exit { stderr, .. } if stderr.contains("boom")),
            "{err}"
        );
    }

    /// 端末の Ctrl-C（プロセスグループへの SIGINT）が claude に直接届かないよう、別の
    /// プロセスグループで起動する。止めるときは nucrawler が自分で止める。
    #[tokio::test]
    async fn runs_in_its_own_process_group() {
        let (script, dir) = fake_claude(
            "cli-pgid",
            "echo \"pgid $(cut -d' ' -f5 /proc/$$/stat)\" >&2\nexit 1",
        );
        let cli = ClaudeCli {
            command: script,
            cwd: dir.join("cwd"),
            timeout: Duration::from_secs(10),
        };
        let schema = serde_json::json!({});
        let err = cli.call(request(&schema)).await.unwrap_err();
        let LlmError::Exit { stderr, .. } = &err else {
            panic!("{err}");
        };
        let ours = std::fs::read_to_string("/proc/self/stat").unwrap();
        let ours = ours
            .rsplit(')')
            .next()
            .unwrap()
            .split_whitespace()
            .nth(2)
            .unwrap();
        let child = stderr.trim().strip_prefix("pgid ").unwrap();
        assert_ne!(child, ours, "{stderr}");
    }

    /// stdin を読まずに終了されると、書き込みが Broken pipe になる。その場合も、終了コードと
    /// stderr で原因を報告する（認証エラーで即終了した場合などに原因を失わないため）。
    #[tokio::test]
    async fn early_exit_without_reading_stdin_reports_stderr() {
        let (script, dir) = fake_claude("cli-early-exit", "echo 'Not logged in' >&2\nexit 1");
        let cli = ClaudeCli {
            command: script,
            cwd: dir.join("cwd"),
            timeout: Duration::from_secs(10),
        };
        let schema = serde_json::json!({});
        // パイプのバッファより大きいので、書き込みの途中で子プロセスが終わる
        let prompt = "x".repeat(1 << 20);
        let err = cli
            .call(LlmRequest {
                prompt: &prompt,
                ..request(&schema)
            })
            .await
            .unwrap_err();
        assert!(
            matches!(&err, LlmError::Exit { status, stderr }
                if status.ends_with(": 1") && stderr.contains("Not logged in")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn slow_process_times_out() {
        let (script, dir) = fake_claude("cli-slow", "cat >/dev/null\nsleep 5");
        let cli = ClaudeCli {
            command: script,
            cwd: dir.join("cwd"),
            timeout: Duration::from_millis(300),
        };
        let schema = serde_json::json!({});
        let started = std::time::Instant::now();
        let err = cli.call(request(&schema)).await.unwrap_err();
        assert!(matches!(err, LlmError::Timeout { .. }), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    /// タイムアウトや中断で止めるときは、claude が起動した子プロセスも残さない。
    #[tokio::test]
    async fn stopping_kills_the_whole_process_group() {
        let (script, dir) = fake_claude(
            "cli-group",
            "sleep 600 &\necho $! > \"$(dirname \"$0\")/grandchild\"\ncat >/dev/null\nsleep 5",
        );
        let cli = ClaudeCli {
            command: script,
            cwd: dir.join("cwd"),
            timeout: Duration::from_millis(500),
        };
        let schema = serde_json::json!({});
        let err = cli.call(request(&schema)).await.unwrap_err();
        assert!(matches!(err, LlmError::Timeout { .. }), "{err}");
        let pid = std::fs::read_to_string(dir.join("grandchild")).unwrap();
        let stat = format!("/proc/{}/stat", pid.trim());
        let alive = || {
            std::fs::read_to_string(&stat)
                .ok()
                .and_then(|s| s.rsplit(')').next().map(|r| r.trim_start().to_string()))
                .is_some_and(|rest| !rest.starts_with('Z'))
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while alive() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(), "grandchild {} is still running", pid.trim());
    }

    /// claude が終わって回収した後は、グループに触れない（グループ ID は再利用されうるので、
    /// 無関係なプロセスを止めかねない）。
    #[tokio::test]
    async fn finished_claude_group_is_left_alone() {
        let (script, dir) = fake_claude(
            "cli-group-done",
            // 孫が出力のパイプを握ったままだと、終了を待ち続けてしまうので閉じておく
            "sleep 600 >/dev/null 2>&1 &\necho $! > \"$(dirname \"$0\")/grandchild\"\necho boom >&2\nexit 3",
        );
        let cli = ClaudeCli {
            command: script,
            cwd: dir.join("cwd"),
            timeout: Duration::from_secs(10),
        };
        let schema = serde_json::json!({});
        let err = cli.call(request(&schema)).await.unwrap_err();
        assert!(matches!(err, LlmError::Exit { .. }), "{err}");
        let pid = std::fs::read_to_string(dir.join("grandchild")).unwrap();
        let pid = pid.trim();
        // シグナルの配送は非同期なので、送られていれば止まるだけの時間を置いてから確かめる
        std::thread::sleep(Duration::from_millis(300));
        let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| s.rsplit(')').next().map(|r| r.trim_start().to_string()))
            .is_some_and(|rest| !rest.starts_with('Z'));
        let _ = std::process::Command::new("kill")
            .args(["-KILL", pid])
            .status();
        assert!(alive, "the finished group must not be signalled");
    }

    #[tokio::test]
    async fn missing_command_is_spawn_error() {
        let cli = ClaudeCli {
            command: "/nonexistent/claude".into(),
            cwd: std::env::temp_dir(),
            timeout: Duration::from_secs(1),
        };
        let schema = serde_json::json!({});
        let err = cli.call(request(&schema)).await.unwrap_err();
        assert!(matches!(err, LlmError::Spawn { .. }), "{err}");
    }
}
