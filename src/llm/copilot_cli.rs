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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{Llm, LlmError, LlmFailure, LlmRequest, LlmResponse, Usage};

/// `llm_calls` などに記録する名前
pub const BACKEND: &str = "copilot-cli";

/// `--available-tools` に渡す、存在しないツールの名前。これだけを許可してツールを 0 個にする。
const NO_TOOLS: &str = "nucrawler-no-tools";

/// 応答が JSON でなかったときに、エラーに載せる本文の最大文字数（和訳の全文を載せないため）
const MAX_ANSWER_IN_ERROR: usize = 200;

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

impl CopilotCli {
    /// 設定のコマンド・タイムアウト・同時に動かす数で、`cwd` を作業ディレクトリにし、呼び出しごとの
    /// `COPILOT_HOME` を `homes` の下に作って呼ぶ。
    pub fn from_config(
        c: &crate::config::LlmConfig,
        cwd: PathBuf,
        homes: PathBuf,
        slots: PathBuf,
    ) -> Self {
        Self {
            command: c.command_for(crate::config::LlmBackend::CopilotCli).into(),
            cwd,
            homes,
            timeout: Duration::from_secs(c.timeout_secs),
            slots,
            concurrency: c.concurrency,
        }
    }
}

impl Llm for CopilotCli {
    type Slot = crate::pipeline::lock::Slot;

    fn backend(&self) -> &'static str {
        BACKEND
    }

    async fn reserve(&self) -> Result<Self::Slot, LlmError> {
        crate::pipeline::lock::acquire_slot(&self.slots, self.concurrency)
            .await
            .map_err(LlmError::Slot)
    }

    async fn call(&self, req: LlmRequest<'_>) -> Result<LlmResponse, LlmFailure> {
        std::fs::create_dir_all(&self.cwd).map_err(LlmError::Io)?;
        // 子プロセスより後に消す（宣言の逆順に drop される）
        let home = Home::create(&self.homes).map_err(LlmError::Io)?;
        let mut command = tokio::process::Command::new(&self.command);
        command
            .args(["--output-format", "json"])
            .args(["--model", req.model])
            .args(["--available-tools", NO_TOOLS])
            .arg("--disable-builtin-mcps")
            .arg("-C")
            .arg(&self.cwd)
            .env("COPILOT_HOME", &home.0)
            // Nix で入れた版から勝手に変わらないようにする
            .env("COPILOT_AUTO_UPDATE", "false")
            .env_remove("COPILOT_CUSTOM_INSTRUCTIONS_DIRS")
            .current_dir(&self.cwd);
        // 消費は、結果が得られなくても、知らせた分をクォータに数える
        super::process::call_cli(command, compose(&req).as_bytes(), self.timeout, |stdout| {
            let (parsed, credits) = parse_events(stdout);
            (parsed, credits.map(|nano_aiu| Usage::Credits { nano_aiu }))
        })
        .await
    }
}

/// 呼び出しごとの `COPILOT_HOME`。drop で消す。
struct Home(PathBuf);

impl Home {
    fn create(root: &Path) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            tracing::warn!(dir = %self.0.display(), "failed to remove the copilot home: {e}");
        }
    }
}

/// copilot に渡すプロンプト。システムの指示、出力の形（JSON Schema）、依頼の本文の順にまとめる。
pub fn compose(req: &LlmRequest<'_>) -> String {
    format!(
        "{system}\n\n\
         ## 出力の形\n\n\
         次の JSON Schema に従う JSON を 1 つだけ返す。前後に説明や ``` などの囲みを付けない。\n\n\
         {schema}\n\n\
         ## 依頼\n\n\
         {prompt}",
        system = req.system,
        schema = req.schema,
        prompt = req.prompt,
    )
}

/// JSONL の出力から、応答の JSON と消費した AI Credits（10^-9 単位）を取り出す。消費は、応答が
/// 得られなくてもそれまでに見えた分を返す。
pub fn parse_events(stdout: &str) -> (Result<serde_json::Value, LlmError>, Option<i64>) {
    let mut credits = None;
    let result = read_events(stdout, &mut credits);
    (result, credits)
}

fn read_events(stdout: &str, credits: &mut Option<i64>) -> Result<serde_json::Value, LlmError> {
    let mut answer = None;
    let mut exit_code = None;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let event: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| LlmError::Protocol(format!("invalid json line ({e}): {line}")))?;
        match event["type"].as_str() {
            Some("assistant.message") => {
                if let Some(content) = event["data"]["content"].as_str().filter(|c| !c.is_empty()) {
                    answer = Some(content.to_string());
                }
            }
            Some("session.usage_checkpoint") => {
                // 累計なので、最後の値がその呼び出しの消費
                if let Some(n) = event["data"]["totalNanoAiu"].as_i64() {
                    *credits = Some(n);
                }
                warn_if_tools_offered(&event["data"]);
            }
            Some("result") => exit_code = Some(event["exitCode"].as_i64()),
            _ => {}
        }
    }
    let exit_code = exit_code.ok_or_else(|| LlmError::Protocol("no result event".into()))?;
    if exit_code != Some(0) {
        return Err(LlmError::Reported {
            subtype: "exit".into(),
            message: format!("copilot finished with exit code {exit_code:?}"),
        });
    }
    let answer = answer.ok_or(LlmError::NoStructuredOutput)?;
    serde_json::from_str(&answer).map_err(|e| {
        let head: String = answer.chars().take(MAX_ANSWER_IN_ERROR).collect();
        LlmError::Protocol(format!("answer is not json ({e}): {head}"))
    })
}

/// モデルにツールが渡っていれば警告する。ツールを外す指定（存在しない名前だけを許可する）は
/// CLI の振る舞いに頼っているので、更新で効かなくなったときに気づけるように。
fn warn_if_tools_offered(checkpoint: &serde_json::Value) {
    let offered = checkpoint["promptCacheBreakState"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["models"].as_object())
        .flat_map(|models| models.values())
        .filter_map(|m| m["tool_count"].as_u64())
        .max()
        .unwrap_or(0);
    if offered > 0 {
        tracing::warn!(
            tools = offered,
            "copilot offered tools to the model; --available-tools {NO_TOOLS} no longer disables them"
        );
    }
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

    /// 実物の copilot で、見出しの和訳の依頼が検証に通る応答を返し、消費を記録できること。
    /// `cargo test -- --ignored real_copilot` で実行する（ログイン済みの copilot と AI Credits を使う）。
    #[tokio::test]
    #[ignore = "uses the real copilot and consumes AI Credits"]
    async fn real_copilot_translates_titles() {
        let dir =
            std::env::temp_dir().join(format!("nucrawler-{}-real-copilot", std::process::id()));
        let cli = CopilotCli {
            command: "copilot".into(),
            cwd: dir.join("cwd"),
            homes: dir.join("homes"),
            timeout: Duration::from_secs(120),
            slots: dir.clone(),
            concurrency: 1,
        };
        let inputs = [crate::db::TitleInput {
            article_id: 1,
            title: "Fed signals rate cut as inflation cools, but hawks push back".into(),
        }];
        let system = crate::prompt::title::system_prompt(&[]);
        let schema = crate::prompt::title::schema();
        let prompt = crate::prompt::title::build_prompt(&inputs);
        let resp = cli
            .call(LlmRequest {
                system: &system,
                prompt: &prompt,
                schema: &schema,
                model: "gpt-6-luna",
            })
            .await
            .unwrap_or_else(|f| panic!("{} ({:?})", f.error, f.usage));
        let parsed = crate::prompt::title::parse(&resp.output, &[1]).unwrap();
        assert_eq!(parsed.missing, Vec::<i64>::new(), "{}", resp.output);
        assert!(
            matches!(resp.usage, Some(Usage::Credits { nano_aiu }) if nano_aiu > 0),
            "{:?}",
            resp.usage
        );
        assert!(
            std::fs::read_dir(dir.join("homes"))
                .unwrap()
                .next()
                .is_none()
        );
        eprintln!("{:?} {:?}", parsed.items, resp.usage);
    }

    /// 消費を知らせた後に止まってタイムアウトしても、消費した分を失敗に付けて返す。
    #[tokio::test]
    async fn timeout_after_a_checkpoint_keeps_the_consumed_credits() {
        let (script, dir) = fake_copilot(
            "copilot-hang",
            &format!(
                "cat >/dev/null\nprintf '%s' '{}'\nsleep 5",
                checkpoint(7).trim()
            ),
        );
        let cli = cli(script, &dir, Duration::from_millis(500));
        let schema = serde_json::json!({});
        let failure = cli.call(request(&schema)).await.unwrap_err();
        assert!(
            matches!(failure.error, LlmError::Timeout { .. }),
            "{}",
            failure.error
        );
        assert_eq!(
            failure.usage,
            Some(crate::llm::Usage::Credits { nano_aiu: 7 })
        );
    }

    /// タイムアウトした子プロセスは、止めて回収してから返す（ゾンビを残さず、`COPILOT_HOME` は
    /// 子プロセスが終わってから消す）。
    #[tokio::test]
    async fn timeout_reaps_the_process() {
        let (script, dir) = fake_copilot(
            "copilot-reap",
            "cat >/dev/null\necho $$ > \"$PWD/pid.txt\"\nsleep 5",
        );
        let cli = cli(script, &dir, Duration::from_millis(300));
        let schema = serde_json::json!({});
        let failure = cli.call(request(&schema)).await.unwrap_err();
        assert!(
            matches!(failure.error, LlmError::Timeout { .. }),
            "{}",
            failure.error
        );
        let pid = std::fs::read_to_string(dir.join("cwd/pid.txt")).unwrap();
        let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
        assert!(!proc.exists(), "{} is not reaped", proc.display());
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
