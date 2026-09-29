//! `copilot`（GitHub Copilot CLI）を子プロセスとして呼ぶバックエンド。
//!
//! - 出力を JSON Schema に従わせる指定が無いので、プロンプトで形を指示し、応答の本文を JSON として読む。
//!   中身がスキーマに合うかは、各ステージが記事ごとに確かめる。
//! - システムの指示を渡す指定も無いので、プロンプトの先頭に含める。
//! - ツールをすべて無効にする。`--available-tools` は空では外れず、存在しない名前だけを渡すと 0 個になる。
//! - 状態（セッションなど）は呼び出しごとの一時ディレクトリ（`COPILOT_HOME`）に置き、終わったら消す。
//!   保存しない指定が無く、呼ぶたびに溜まるため。認証情報は `COPILOT_HOME` の外にあるので、空でも通る。
//! - cwd は中立なディレクトリにして、プロジェクトの AGENTS.md などを読ませない。
//! - 出力は JSONL。`assistant.message` から応答を、`session.usage_checkpoint` から消費した
//!   AI Credits を、`result` から終了コードを得る。

use std::path::PathBuf;
use std::time::Duration;

use super::{Llm, LlmError, LlmFailure, LlmRequest, LlmResponse};

pub struct CopilotCli {
    pub command: PathBuf,
    /// 子プロセスの作業ディレクトリ（無ければ作る）
    pub cwd: PathBuf,
    /// 呼び出しごとの `COPILOT_HOME` を作るディレクトリ（無ければ作る）
    pub homes: PathBuf,
    pub timeout: Duration,
    /// 呼び出しの枠（`llm-slot-N.lock`）を置くディレクトリ。`ClaudeCli` と同じ
    pub slots: PathBuf,
    pub concurrency: usize,
}

impl Llm for CopilotCli {
    type Slot = crate::pipeline::lock::Slot;

    fn backend(&self) -> &'static str {
        "copilot-cli"
    }

    async fn reserve(&self) -> Result<Self::Slot, LlmError> {
        crate::pipeline::lock::acquire_slot(&self.slots, self.concurrency)
            .await
            .map_err(LlmError::Slot)
    }

    async fn call(&self, _req: LlmRequest<'_>) -> Result<LlmResponse, LlmFailure> {
        todo!()
    }
}

/// copilot に渡すプロンプト。システムの指示、出力の形（JSON Schema）、依頼の本文の順にまとめる。
pub fn compose(_req: &LlmRequest<'_>) -> String {
    todo!()
}

/// JSONL の出力から、応答の JSON と消費した AI Credits（10^-9 単位）を取り出す。消費は、応答が
/// 得られなくてもそれまでに見えた分を返す。
pub fn parse_events(_stdout: &str) -> (Result<serde_json::Value, LlmError>, Option<i64>) {
    todo!()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn fixture() -> String {
        String::from_utf8(crate::testutil::fixture("copilot-success.jsonl")).unwrap()
    }

    fn line(value: serde_json::Value) -> String {
        format!("{value}\n")
    }

    fn answer(content: &str) -> String {
        line(serde_json::json!({"type": "assistant.message", "data": {"content": content}}))
    }

    fn checkpoint(nano_aiu: i64) -> String {
        line(
            serde_json::json!({"type": "session.usage_checkpoint", "data": {"totalNanoAiu": nano_aiu}}),
        )
    }

    fn result(exit_code: i64) -> String {
        line(serde_json::json!({"type": "result", "exitCode": exit_code}))
    }

    #[test]
    fn parses_answer_and_consumed_credits() {
        let (output, credits) = parse_events(&fixture());
        assert_eq!(
            output.unwrap(),
            serde_json::json!({"items": [{"id": 1, "ok": true}]})
        );
        assert_eq!(credits, Some(37_405_000));
    }

    /// 応答の本文が JSON でなくても、消費した分は返す（クォータに数えるため）。
    #[test]
    fn answer_that_is_not_json_is_a_protocol_error_keeping_credits() {
        let out = format!(
            "{}{}{}",
            answer("```json\n{}\n```"),
            checkpoint(5),
            result(0)
        );
        let (output, credits) = parse_events(&out);
        assert!(matches!(output, Err(LlmError::Protocol(_))), "{output:?}");
        assert_eq!(credits, Some(5));
    }

    #[test]
    fn missing_answer_is_no_structured_output() {
        let out = format!("{}{}", checkpoint(5), result(0));
        let (output, credits) = parse_events(&out);
        assert!(
            matches!(output, Err(LlmError::NoStructuredOutput)),
            "{output:?}"
        );
        assert_eq!(credits, Some(5));
    }

    /// 応答が複数あれば最後のものを使う。
    #[test]
    fn last_answer_wins() {
        let out = format!(
            "{}{}{}",
            answer("{\"n\": 1}"),
            answer("{\"n\": 2}"),
            result(0)
        );
        assert_eq!(parse_events(&out).0.unwrap(), serde_json::json!({"n": 2}));
    }

    /// 消費は累計なので、最後の値を使う。
    #[test]
    fn last_checkpoint_wins() {
        let out = format!(
            "{}{}{}{}",
            checkpoint(5),
            answer("{}"),
            checkpoint(9),
            result(0)
        );
        assert_eq!(parse_events(&out).1, Some(9));
    }

    #[test]
    fn missing_result_or_broken_line_is_a_protocol_error() {
        let (output, _) = parse_events(&answer("{}"));
        assert!(matches!(output, Err(LlmError::Protocol(_))), "{output:?}");
        let (output, credits) = parse_events(&format!("{}not json\n", checkpoint(5)));
        assert!(matches!(output, Err(LlmError::Protocol(_))), "{output:?}");
        assert_eq!(credits, Some(5));
    }

    #[test]
    fn nonzero_exit_code_in_the_result_is_reported() {
        let out = format!("{}{}{}", answer("{}"), checkpoint(5), result(1));
        let (output, credits) = parse_events(&out);
        assert!(
            matches!(output, Err(LlmError::Reported { .. })),
            "{output:?}"
        );
        assert_eq!(credits, Some(5));
    }

    #[test]
    fn prompt_carries_the_system_the_schema_and_the_request() {
        let schema = serde_json::json!({"type": "object", "required": ["items"]});
        let prompt = compose(&request(&schema));
        let system = prompt.find("SYSTEM").unwrap();
        let shape = prompt.find(&schema.to_string()).unwrap();
        let body = prompt.find("PROMPT 日本語").unwrap();
        assert!(system < shape && shape < body, "{prompt}");
        assert!(prompt.contains("JSON"), "{prompt}");
    }

    /// テストごとの一時ディレクトリに、偽の copilot（シェルスクリプト）を置く。
    fn fake_copilot(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nucrawler-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("copilot");
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

    fn cli(script: PathBuf, dir: &std::path::Path, timeout: Duration) -> CopilotCli {
        CopilotCli {
            command: script,
            cwd: dir.join("cwd"),
            homes: dir.join("homes"),
            timeout,
            slots: dir.to_path_buf(),
            concurrency: 1,
        }
    }

    fn request(schema: &serde_json::Value) -> LlmRequest<'_> {
        LlmRequest {
            system: "SYSTEM",
            prompt: "PROMPT 日本語",
            schema,
            model: "gpt-6-luna",
        }
    }

    #[tokio::test]
    async fn passes_arguments_environment_and_stdin_and_parses_output() {
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/copilot-success.jsonl");
        let (script, dir) = fake_copilot(
            "copilot-ok",
            &format!(
                "printf '%s\\n' \"$@\" > \"$PWD/args.txt\"\n\
                 cat > \"$PWD/stdin.txt\"\n\
                 [ -d \"$COPILOT_HOME\" ] && echo \"$COPILOT_HOME\" > \"$PWD/home.txt\"\n\
                 echo \"$COPILOT_AUTO_UPDATE\" > \"$PWD/auto_update.txt\"\n\
                 cat '{}'",
                fixture_path.display()
            ),
        );
        let cwd = dir.join("cwd");
        let cli = cli(script, &dir, Duration::from_secs(10));
        let schema = serde_json::json!({"type": "object"});
        let resp = cli.call(request(&schema)).await.unwrap();
        assert_eq!(resp.output["items"][0]["id"], 1);
        assert_eq!(
            resp.usage,
            Some(crate::llm::Usage::Credits {
                nano_aiu: 37_405_000
            })
        );

        let read = |name: &str| std::fs::read_to_string(cwd.join(name)).unwrap();
        let args: Vec<String> = read("args.txt").lines().map(String::from).collect();
        let after = |flag: &str| {
            let i = args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} in {args:?}"));
            args[i + 1].clone()
        };
        assert_eq!(after("--output-format"), "json");
        assert_eq!(after("--model"), "gpt-6-luna");
        assert_eq!(after("--available-tools"), "nucrawler-no-tools");
        assert_eq!(after("-C"), cwd.display().to_string());
        assert!(
            args.contains(&"--disable-builtin-mcps".to_string()),
            "{args:?}"
        );
        // プロンプトは引数ではなく stdin で渡す（長い記事でも引数の長さの上限に当たらないように）
        assert!(!args.contains(&"-p".to_string()), "{args:?}");
        assert_eq!(read("stdin.txt"), compose(&request(&schema)));
        assert_eq!(read("auto_update.txt").trim(), "false");
        // 呼び出しの間だけ一時ディレクトリがあり、終わったら消える
        let home = PathBuf::from(read("home.txt").trim());
        assert!(home.starts_with(dir.join("homes")), "{}", home.display());
        assert!(!home.exists(), "{}", home.display());
    }

    /// 結果行が無い（起動時に失敗した）ときは、終了コードと stderr で報告する。
    #[tokio::test]
    async fn nonzero_exit_without_a_result_reports_stderr() {
        let (script, dir) = fake_copilot(
            "copilot-exit",
            "cat >/dev/null\necho 'Error: Model \"x\" from --model flag is not available.' >&2\nexit 1",
        );
        let cli = cli(script, &dir, Duration::from_secs(10));
        let schema = serde_json::json!({});
        let failure = cli.call(request(&schema)).await.unwrap_err();
        assert!(
            matches!(&failure.error, LlmError::Exit { stderr, .. } if stderr.contains("not available")),
            "{}",
            failure.error
        );
        assert!(
            std::fs::read_dir(dir.join("homes"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    /// 応答の形が崩れても、消費した分を失敗に付けて返す。
    #[tokio::test]
    async fn malformed_answer_carries_the_consumed_credits() {
        let out = format!("{}{}{}", answer("not json"), checkpoint(7), result(0));
        let (script, dir) = fake_copilot(
            "copilot-malformed",
            &format!(
                "cat >/dev/null\nprintf '%s' '{}'",
                out.replace('\'', "'\\''")
            ),
        );
        let cli = cli(script, &dir, Duration::from_secs(10));
        let schema = serde_json::json!({});
        let failure = cli.call(request(&schema)).await.unwrap_err();
        assert!(
            matches!(failure.error, LlmError::Protocol(_)),
            "{}",
            failure.error
        );
        assert_eq!(
            failure.usage,
            Some(crate::llm::Usage::Credits { nano_aiu: 7 })
        );
    }

    #[tokio::test]
    async fn slow_process_times_out() {
        let (script, dir) = fake_copilot("copilot-slow", "cat >/dev/null\nsleep 5");
        let cli = cli(script, &dir, Duration::from_millis(200));
        let schema = serde_json::json!({});
        let started = std::time::Instant::now();
        let failure = cli.call(request(&schema)).await.unwrap_err();
        assert!(
            matches!(failure.error, LlmError::Timeout { .. }),
            "{}",
            failure.error
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            std::fs::read_dir(dir.join("homes"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
