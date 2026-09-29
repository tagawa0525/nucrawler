# GitHub Copilot CLI を LLM のバックエンドに加える

## 背景

2026-09-30 時点で、LLM を呼ぶ処理（要約・採点・和訳・見出しの和訳・語彙の整理・profile suggest）は、
すべて Claude Code の headless モード（`claude -p`、`src/llm/claude_cli.rs`）で動いている。
Claude のサブスクリプションの枠だけに頼らず、GitHub Copilot の枠でも同じ処理を回せるようにしたい。

### Copilot CLI で確かめたこと（1.0.89）

- `copilot -p <text> --output-format json` で、非対話で 1 回呼んで終わる。出力は JSONL（1 行 1 イベント）
- 応答は `type: "assistant.message"` の `data.content` に文字列で入る。`type: "result"` の行が最後に出て、
  `exitCode` を持つ
- 使用量は `type: "session.usage_checkpoint"` の `data.totalNanoAiu`（AI Credits の 10^-9 単位）。
  見出し 1 本の和訳で 159,762,400（約 0.16 クレジット）だった。`result.usage.premiumRequests` は旧課金の名残で使わない
- `gpt-6-luna` で見出しを試し、`{"title":"インフレ鈍化でFRBが利下げ示唆も、タカ派が反発"}` と、
  自然な訳を指示どおりの JSON で返した。`gpt-6.1-luna` はこの版では選べない
- `--model auto` は `--reasoning-effort` と併用するとエラーで終わる

### `claude -p` との違い

| `claude_cli.rs` で使っているもの                             | Copilot CLI                                                                                |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| `--json-schema`（出力がスキーマに従う）                      | 無い。プロンプトで JSON の形を指示し、自前でパースする                                     |
| `--system-prompt`                                            | 無い。システムの指示は `-p` の本文に含める                                                 |
| `--tools ""`                                                 | `--available-tools` に存在しない名前だけを渡すと 0 個になる（「着手時に確かめたこと」）    |
| `--setting-sources ""`・`--strict-mcp-config`                | `--disable-builtin-mcps` はある。`~/.copilot` の instructions・skills を読まないかは未確認 |
| `--no-session-persistence`                                   | 無い。セッションが `$COPILOT_HOME` に残るので、呼び出しごとに一時ディレクトリを使う        |
| `rate_limit_event`（5 時間枠・週次枠の使用率とリセット時刻） | 無い。1 回ごとの消費クレジットだけが分かる                                                 |

### Copilot の課金

2026-06-01 に、リクエスト数での課金（Premium Requests）から、トークン量に応じて AI Credits を消費する方式に変わった
（[GitHub Blog](https://github.blog/news-insights/company-news/github-copilot-is-moving-to-usage-based-billing/)）。
入力・出力・キャッシュのトークンをモデルごとの API 単価で換算して差し引く。プランごとに毎月のクレジットがある
（Pro は月 1,500 クレジット、1 クレジット＝1 セント相当）。毎月 1 日 00:00 UTC に戻る。

したがって、呼び出しをまとめても分けても、処理するトークン量が同じなら消費は同じ。バッチの大きさは
Copilot のために変えない。

## 方針

- バックエンドを設定で選べるようにする（`llm.backend = "claude-cli" | "copilot-cli"`）。既定は今と同じ `claude-cli`
- クォータはサービスごとに持つ。Claude の使用率による判定（`src/quota.rs`）は流用できないので、Copilot 用に
  **月の消費クレジットを、月の経過割合 × 0.8 までに抑える**判定を作る。上限と係数は設定値にする
- JSON が崩れたときは再試行しない。回数を設定できるようにして既定 0 回とする案もあったが、動かない仕組みを先に作らず、
  モデルの比較で崩れる頻度を見てから決める（「再試行」の節）
- まず動くことを優先し、モデルを比べるのはその後にする（「モデルの比較」の節）

## 設計

### 呼び出し（`src/llm/copilot_cli.rs`）

- 引数：`--output-format json`、`--model <model>`、`--disable-builtin-mcps`、`--available-tools <存在しない名前>`（ツールを 0 にする）、
  `-C <中立な作業ディレクトリ>`（プロジェクトの AGENTS.md などを読ませない。`ClaudeCli` の `cwd` と同じ `llm-cwd` を使う）
- プロンプトは stdin で渡す（`-p` は付けない）
- 環境：呼び出しごとに作る一時ディレクトリを `COPILOT_HOME` にし、終わったら消す（セッションを残さない）。
  `COPILOT_AUTO_UPDATE=false`。`COPILOT_CUSTOM_INSTRUCTIONS_DIRS` は外す
- プロンプト：システムの指示、出力の JSON の形（`LlmRequest.schema` をそのまま載せ、「この JSON Schema に従う JSON だけを返す」と指示）、
  本文の順に 1 つの文字列にまとめる
- 出力の解釈：最後の `assistant.message` の `content` を JSON としてパースする。パースできなければ `LlmError::Protocol`。
  中身がスキーマに合うかは、今と同じく各ステージが記事ごとに確かめる（合わない記事は `MISSING` の失敗になる）。
  スキーマの検証のために外部クレートを足さない
- 使用量：`session.usage_checkpoint` の `totalNanoAiu` の最後の値を、その呼び出しの消費とする。同じ行の `tool_count` が 0 でなければ警告する
- 失敗：`result` 行が無ければ終了コードと stderr で `LlmError::Exit`。タイムアウト・`kill_on_drop`・枠（`reserve`）は `ClaudeCli` と同じ
- `backend()` は `"copilot-cli"`

### バックエンドの切り替え

- `Llm` トレイトは `impl Future` を返すので `dyn` にできない。`enum Backend { Claude(ClaudeCli), Copilot(CopilotCli) }` に
  `Llm` を実装して振り分ける。`Slot` はどちらも `pipeline::lock::Slot`
- `src/cmd/crawl.rs`・`redo.rs`・`suggest.rs`・`eval.rs` は `ClaudeCli::from_config` の代わりに `Backend::from_config` を、
  `Quota::new` の代わりに `Quota::from_config`（バックエンドに合わせて使用率か月の消費クレジットで判定する）を使う
- Web UI の認証切れの案内は、失敗した呼び出しの backend に合わせる（copilot-cli なら `copilot login`）
- 設定：`llm.backend` を足す。`llm.command` の既定はバックエンドに合わせる（`claude` / `copilot`）。
  （のちに工程ごとにバックエンドを選べるようにしたとき、`command` は claude の実行ファイルに戻し、`copilot_command` を足した。
  `docs/plans/006_per_stage_backend.md`）
  ステージごとのモデル名（`digest_model` など）はそのまま使う。切り替えるときはモデル名も書き換える
- 成果物（`artifacts`）・失敗の記録（`stage_errors`）・作業の予約（`work_claims`）は backend と model をキーに持つので、
  スキーマは変えない。要約・和訳・見出しの和訳は、backend によらず成果物があれば処理済みとみなす
  （`pending_digest`・`pending_translate`・`pending_titles` が backend と model で絞るのは `stage_errors` と `work_claims` だけ）ので、
  切り替えても作り直しは起きない
- 採点（`pending_score`、`src/db/score.rs`）は、同じ backend と model の採点が無い要約を対象にする。したがって切り替えると、
  `backlog_days` 以内の要約がもう一度採点される（今 `score_model` を変えたときと同じ振る舞い）。1 回だけのことで、
  クォータの範囲で進むので、そのままにする

### 使用量の記録

- `LlmResponse.rate_limit: Option<RateLimit>` を `usage: Option<Usage>` に変える。
  `enum Usage { Subscription(RateLimit), Credits { nano_aiu: i64 } }`
- `llm_calls` に `credits_nano INTEGER` 列を足す（マイグレーション）。`rate_limit` 列は Claude 用にそのまま残す
- 失敗した呼び出しも使用量を運ぶ（`Llm::call` の失敗は `LlmFailure { error, usage }`）。JSON が崩れた呼び出しもクレジットを
  消費するので、残さないと月の消費を少なく数える。`LlmError::RateLimited` が持っていた使用率もこの `usage` に移す
- `CopilotCli` は、`totalNanoAiu` を読めた後の失敗（`Protocol` など）にも使用量を付けて返す

### Copilot のクォータ

- 設定 `[copilot_quota]`：`monthly_credits`（プランの月のクレジット。Pro は 1500。既定値は置かず必須）、`pace`（既定 0.8）
- 判定：今月の消費（`llm_calls` の `backend = 'copilot-cli'` の `credits_nano` の合計）が
  `monthly_credits × 月の経過割合 × pace` 以上なら止める。月は UTC の暦月（毎月 1 日 00:00 UTC から）。止めた理由は `quota::Stop` に新しい種類として足す
- `max_calls_per_run`（`--max-llm-calls`）は両方のバックエンドに効かせる
- 判定のたびに DB から合計を読む（今の `latest_rate_limit` と同じく、並行する実行の消費も入れるため）。
  実行中の呼び出しの分だけ遅れて見えるのも今と同じ（最大 `llm.concurrency` 回分）
- `Quota` はバックエンドに応じて、Claude の判定（5 時間枠・週次枠）か Copilot の判定かを使う
- 限界：対話で使う Copilot の消費は nucrawler から見えない。`pace` の 0.8 がその分の余裕になる

### 再試行

作らない（2026-09-30 に判断）。既定 0 回では一度も動かない仕組みになるため。今は JSON が崩れた呼び出しは
`LlmError::Protocol` で失敗し、消費したクレジットを記録したうえでステージを止め、次の実行で同じ記事をもう一度処理する。

モデルの比較で `llm_calls` の `Protocol` エラーが無視できない頻度で出たら、次の形で足す。

- `llm.max_retries` を足す。`LlmError::Protocol`・`NoStructuredOutput` のときだけ、同じ依頼で呼び直す
- タイムアウト・異常終了・利用上限では再試行しない（原因が記事の出力の形ではないため）
- 呼び直すたびに `llm_calls` に 1 行記録し、クォータの判定もやり直す（再試行も消費に数える）

## PR の分け方（どこで止めても壊れない順）

1. 使用量の一般化：`LlmResponse.usage`、`Usage` enum、`llm_calls.credits_nano` のマイグレーション。振る舞いは変わらない
2. `CopilotCli` の追加：引数の組み立て、JSONL の解釈、使用量の取り出し。テストは `claude_cli.rs` と同じく、
   偽のスクリプトを実行ファイルとして置いて確かめる。まだどこからも使わない
3. Copilot のクォータ：`[copilot_quota]`、月の消費の集計（`Db`）、`Quota` の判定の切り替え
4. バックエンドの切り替え：`llm.backend`、`Backend` enum、`cmd` の配線。ここで Copilot で動かせるようになる
   （クォータより後にするのは、上限の無い状態で動かせる期間を作らないため）
5. （見送り）再試行の回数。「再試行」の節

PR 1〜4 は #128〜#131 でマージ済み（2026-09-30）。

各 PR は TDD（テストのコミット → 実装のコミット）で進める。

## 着手時に確かめたこと（2026-09-30、Copilot CLI 1.0.89、`gpt-6-luna`）

| 項目                           | 結果                                                                                                                                                                                                                                                                                                                               | 設計への反映                                                                                                                                                                                               |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ツールを外す                   | `--available-tools=`・`--available-tools ""`・`--deny-tool` では 18 個のまま。`--excluded-tools` は名前を挙げた分だけ減る。**存在しない名前だけを許可する（`--available-tools nonexistent_tool`）と 0 個**になり、stderr にも何も出ない。消費も 1 回 0.159 → 0.037 クレジットに下がった（ツール定義の約 7,500 トークンが無くなる） | `--available-tools` に存在しない名前を 1 つだけ渡す。CLI の更新で振る舞いが変わりうるので、`usage_checkpoint` の `tool_count` が 0 でなければ警告を出す                                                    |
| プロンプトを stdin で渡す      | `-p` を付けずに stdin で渡すと動く。約 60 KB（20,043 文字）の日本語も全文届いた                                                                                                                                                                                                                                                    | `ClaudeCli` と同じく stdin で渡す。引数の長さの上限は気にしなくてよい                                                                                                                                      |
| instructions・skills・memories | 中立なディレクトリで `copilot instruction list` は「No instruction sources found」。skills は組み込みのものがあるが、ツールを 0 にすると `skill` ツールも無いので使われない。memories は `-p` では既定で無効                                                                                                                       | 追加の指定は要らない。`COPILOT_CUSTOM_INSTRUCTIONS_DIRS` は子プロセスの環境から外す                                                                                                                        |
| セッションの保存               | 呼ぶたびに `$COPILOT_HOME/session-state/<id>` が 1 つ（50〜180 KB）でき、`session-store.db` も増える。保存しない指定は無い。**`COPILOT_HOME` を空のディレクトリにしても認証は通る**（認証情報は `COPILOT_HOME` の外にある）                                                                                                        | 呼び出しごとに一時ディレクトリを作って `COPILOT_HOME` にし、終わったら消す（並行する呼び出しどうしも干渉しない）。あわせて `COPILOT_AUTO_UPDATE=false` にし、Nix で入れた版から勝手に変わらないようにする  |
| クレジットを使い切ったとき     | 実際には再現できていない。個人プランでは、追加利用の予算を設定しない限り使えなくなる（ドキュメントにエラーの形は書かれていない）                                                                                                                                                                                                   | 当面は `result` 行の無い異常終了（`LlmError::Exit`）として扱い、ステージを止める。実際に起きたら出力を記録し、`RateLimited` に振り分けるかを決める。自前のクォータ（`pace` 0.8）で、通常はそこまで使わない |
| 課金の月の区切り               | **暦月。毎月 1 日 00:00:00 UTC** に戻る。契約日にはよらない。繰り越しは無い（[GitHub Docs](https://docs.github.com/en/copilot/concepts/billing-and-usage/individuals/billing)）                                                                                                                                                    | 月の経過割合は UTC の暦月で計算する（JST では毎月 1 日 9:00 が区切り）                                                                                                                                     |
| 月のクレジット                 | Pro 1,500（基本 1,000 ＋ flex 500）、Pro+ 7,000、Max 20,000（同ドキュメント）                                                                                                                                                                                                                                                      | `monthly_credits` の既定値は置かず、設定を必須にする（プランで 5 倍以上違うため）                                                                                                                          |
| 使用量を問い合わせる API       | ドキュメントには見当たらない（使用量のダッシュボードだけ）                                                                                                                                                                                                                                                                         | 自前で数える方針のまま。対話で使った分は見えない                                                                                                                                                           |

## モデルの比較（PR 4 の後、運用で行う）

- 本番の DB をコピーした別の data ディレクトリで、Copilot の設定にして `redo` を流す（本番の一覧に影響させない）
- 対象は最近の記事 20〜30 件。比べるのは今の `sonnet`（claude-cli）と `gpt-6-luna`（copilot-cli）
- 見るもの：
  - 要約（digest）と採点（score）の質。判断が入る工程なので差が出やすい
  - 和訳・見出しの和訳・語彙の整理の質。変換に近いので `gpt-6-luna` で足りる見込み
  - 1 記事あたりの消費クレジット（`llm_calls.credits_nano`）
  - JSON が崩れた回数（`llm_calls` の `error` が `Protocol` のもの）
- 結果に応じて、工程ごとにモデル（とバックエンド）を選ぶ必要が出たら、そのときに別の計画にする
