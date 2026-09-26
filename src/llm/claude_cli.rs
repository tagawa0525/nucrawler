//! `claude -p` を子プロセスとして呼ぶバックエンド。
//!
//! - `--bare` は API キー認証を強制するので使わない（サブスクの OAuth で動かすため）。
//! - ツールをすべて無効にし、MCP・スラッシュコマンド・設定ファイルを読まない。
//! - cwd は中立なディレクトリにして、プロジェクトの CLAUDE.md などを読ませない。
//! - 出力は stream-json。`rate_limit_event` から使用率を、`result` から構造化出力を得る。

use std::path::PathBuf;
use std::time::Duration;

use super::{Llm, LlmError, LlmRequest, LlmResponse, RateLimit};

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

    async fn call(&self, _req: LlmRequest<'_>) -> Result<LlmResponse, LlmError> {
        todo!()
    }
}

/// stream-json の出力から、構造化出力と最後の使用率を取り出す。
pub fn parse_stream(_stdout: &str) -> Result<(serde_json::Value, Option<RateLimit>), LlmError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::llm::Window;

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
        assert!(
            matches!(
                err,
                LlmError::RateLimited {
                    resets_at: Some(1790457000)
                }
            ),
            "{err}"
        );
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
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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
