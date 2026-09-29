# LLM のバックエンドを工程ごとに選べるようにする

## 背景

2026-09-30 に、LLM のバックエンドとして Claude Code（`claude-cli`、サブスクリプションの枠）に加え、GitHub Copilot CLI
（`copilot-cli`、AI Credits の月の予算）を選べるようにした（`docs/plans/005_copilot_backend.md`、#128〜#131）。
ただし `llm.backend` は 1 つだけで、すべての工程が同じバックエンドを使う。

同じ日に、本番の DB のコピーで sonnet（claude-cli）と gpt-6-luna（copilot-cli）を比べた。

- 要約・和訳・見出しの和訳：ほぼ同じ内容で、gpt-6-luna で足りる
- 採点：sonnet の同じ要約を採点した 26 件で、gpt-6-luna は平均 +6.7 点高く、一覧の既定の閾値（50 点）をまたいだのが 5 件。
  評価から学んだ推薦点の補正も sonnet の点を前提にしている

そこで、採点は Claude のまま、ほかの工程は Copilot で、のように工程ごとに選べるようにする。

## 方針

- 設定：`llm.backend` は既定として残し、工程ごとに `digest_backend`・`score_backend`・`translate_backend`・`title_backend`・
  `tidy_backend` で上書きする（省けば `backend`）。各工程のモデル名（`digest_model` など）と対にする
  - `profile suggest` と `eval --profile` は採点のモデル（`score_model`）を使うので、`score_backend` に従う
- 実行ファイル：2 つのバックエンドを同時に使うので、「選んだバックエンドのコマンド」という #131 の `command` の意味は成り立たない。
  `command` は #131 より前の意味（claude の実行ファイル、既定 `claude`）に戻し、`copilot_command`（既定 `copilot`）を足す。
  古い設定例の `command = "claude"` もそのまま動く
- `[copilot_quota]` は、どれかの工程が copilot-cli を使うときに必須にする

## 設計

### 工程ごとの LLM

- `config::LlmTask`（Digest・Score・Translate・Title・Tidy）と、`LlmConfig::backend_for(task)` を足す
- `llm::LlmSet` トレイト：`fn for_task(&self, task) -> &Self::Llm`。`Llm` を実装する型（テストの `FakeLlm` など）は、どの工程にも
  自分を返す（ブランケット実装）。`llm::Backends` は claude と copilot の `Backend` を持ち、設定に従って返す
- `RunEnv` の `llm` を `LlmSet` にし、`RunEnv::stage(task)` がその工程の LLM を `LlmStage` に入れる。各ステージのコードは変えない
  （ステージが `llm.backend()` で記録する成果物・失敗・予約のキーは、その工程のバックエンドになる）

### クォータ

- `Quota` は 1 回の実行で 1 つのまま、Claude の使用率と Copilot の月の予算の両方を持つ（どれかの工程が copilot-cli なら予算を持つ）
- 判定は呼び出すバックエンドで行う：`Quota::permit(backend, now)`。予算のバックエンドなら月の消費クレジット、それ以外は使用率
- `llm_call::permit` も呼び出すバックエンドを受け取り、そのバックエンドの消費（`credits_since`）か使用率（`latest_rate_limit`）を読む
- 1 回の実行の呼び出し回数の上限（`max_calls_per_run`）は、バックエンドをまたいで数える

### LLM が使えなくなったとき

- 今は、利用上限（`Halt::UsageLimit`）や認証切れなど（`Halt::LlmFailed`）で止まると、その実行の後続の LLM の工程をすべて飛ばす
  （`RunReport::llm_blocked`）
- これをバックエンドごとにする（`llm_blocked` を止まったバックエンドの一覧にする）。Claude が 5 時間枠の上限に達しても、
  Copilot の工程は続ける

## PR

1 つの PR で、次のコミットに分ける。

1. 計画（この文書）
2. リファクタ：`LlmTask`・`LlmSet`・`RunEnv::stage(task)`、`Quota::permit` と `llm_call::permit` にバックエンドを渡す、
   `llm_blocked` をバックエンドの一覧にする（バックエンドが 1 つなら振る舞いは変わらない）
3. RED → GREEN：工程ごとの設定、`Backends`、`Quota::from_config` の両持ち、バックエンドごとの判定と止め方
4. 設定例・README
