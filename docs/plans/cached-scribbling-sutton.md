# リファクタリング計画：重複の解消とパイプライン実行部のテスト化

作成：2026-09-27（main 0d31275 の時点）

## Context

[肥大化ファイルの分割計画](robust-forging-moore.md)に沿って、web/server・web/html の分割（PR #72・#73）と、main.rs から crawl・redo を `src/cmd/` へ移す作業（PR #74）を終え、大きすぎるファイルはほぼ無くなった。分割計画は移動だけに限り、「crawl・redo の組み立てを lib へ寄せてテストできるようにする」ことを振る舞いに関わる別件として対象外にしていた。この計画の PR 5 がその別件に当たる。そのうえでコード全体を見直すと、まだ次の 2 種類の問題が残っている。

- **同じ処理が何度も書かれている。** LLM ステージの失敗の記録、プロファイルのハッシュの取得、LLM とクォータの用意が、ほぼ同じ形で 2〜5 か所ずつある。1 か所を直すと、ほかも直す必要がある。
- **パイプラインを回す部分にテストが無い。** crawl のステージの順序や、「LLM が使えなくなったら後続の LLM ステージを飛ばす」判定、終了理由の報告は、バイナリ側の `src/cmd/` にある。lib のテストから呼べないので、どこにもテストが無い。

この計画では、これらを 5 つの小さな PR に分けて直す。どの PR も利用者から見た振る舞いは変えない。

## 進め方

- 1 項目を 1 ブランチ・1 PR にし、同じ worktree で順に進める（前の PR をマージしてから次に着手する）
- 移動・置き換えだけの PR は、既存のテストがそのまま通ることを振る舞いを変えていない証拠にする
- 新しく振る舞いを持つ関数（PR 3 の `Db::profile_hash`）は TDD で、テスト（RED）→ 実装（GREEN）の順にコミットする
- 各 PR は CI と同じ `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` を通してから出し、自動レビューを `~/.claude/scripts/gh-wait-review.sh` で待つ

順序は影響の小さいものからにした。PR 4 のモジュール名の変更を PR 5 より先にするのは、PR 5 で書く新しいコードが最初から新しい名前を使えるようにするため。

---

## PR 1：一覧だけで使う `split_sections` を `web/html/list.rs` へ移す

**今の状態**

PR #73 で `src/web/html/` を画面ごとのサブモジュール（list, search, detail, reports, settings）に分けた。ただ、一覧の画面でしか使わない `split_sections` が `mod.rs` に残っている。この関数は一覧を「前回の訪問の後に届いた記事」と「それより前の未読の記事」に分ける。`mod.rs` は本来、全画面で共有する部品（`escape`、`layout`、`button` など）の置き場になっている。

**変更**

- `split_sections` と、そのテスト `splits_new_and_earlier_unread` を `list.rs` へ移す
- `mod.rs` の `pub use list::*` があるので、呼び出し元の `web/server/pages.rs` は `html::split_sections` のままでよい
- `button`（detail と list で使う）と `display_title`（detail・list・feed で使う）は共有の部品なので、`mod.rs` に残す

---

## PR 2：LLM ステージの「記事の失敗を記録する」処理を 1 つの関数にまとめる

**今の状態**

LLM を使うステージ（`src/pipeline/digest.rs`、`score.rs`、`translate.rs`）は、LLM の呼び出しが失敗したときや、応答が壊れていたときに、対象の記事を「失敗・後で再試行」として記録する。この処理が合わせて 8 か所に、次の同じ形で書かれている。

```rust
for &id in &ids {
    db.record_stage_failure(key(id), &message, now, false)?;
}
summary.failed += ids.len();
```

digest と score では、次の 3 つの場面それぞれにこのループがある。translate は 1 件ずつ処理するので、ループの代わりに 1 回の呼び出しになっている。

- LLM が止まった（`Halt::LlmFailed`）
- 応答がスキーマに合わない
- 応答に一部の記事が無い

応答に無かった記事のメッセージ `"missing or invalid in the llm output"` も、digest と score に同じ文字列が書かれている。

**変更**

- 各 LLM ステージが共有するモジュール `src/pipeline/llm_call.rs` に、次の関数を追加する

  ```rust
  /// 各記事の失敗を記録し、記録した件数を返す（記事は再試行に回る）。
  pub fn record_failures<'k>(
      db: &Db,
      keys: impl IntoIterator<Item = StageKey<'k>>,
      message: &str,
      now: DateTime<Utc>,
  ) -> Result<usize, DbError>
  ```

- 呼び出し側を `summary.failed += record_failures(db, ids.iter().map(|&id| key(id)), &message, now)?;` にそろえる。translate は `std::iter::once(key)` を渡す
- 応答に無かった記事のメッセージを、同じモジュールの `pub const MISSING: &str` にする

**やらないこと**：ステージのループそのものの共通化。translate は 1 件ずつ、tidy はバッチも記事ごとの失敗も無いので、ステージごとに形が違う。共通化すると、かえって読みにくくなる。

---

## PR 3：プロファイルのハッシュだけを読む `Db::profile_hash` を追加する

**今の状態**

関心プロファイル（`profiles` テーブル）にはハッシュの列がある。採点結果や検索の点数の条件は、このハッシュで今のプロファイルに対応付けている。ところが、ハッシュを取り出す専用の関数が無い。そのため、次の 6 か所で、プロファイル全体を読んで JSON を解釈してから、ハッシュ以外を捨てている。

```rust
db.load_profile(user)?.map(|(_, hash)| hash)
```

該当する 6 か所は次のとおり。

- `src/cmd/redo.rs`：作り直しの条件
- `src/cmd/crawl.rs`：`.is_some()` でプロファイルの有無を見る
- `src/main.rs`：search
- `src/mcp.rs`：`viewer`
- `src/web/server/mod.rs`：`viewer`
- `src/pipeline/translate.rs`

**変更**

1. RED：`src/db/vocab.rs` のテストに、次の 2 つを確かめるテストを追加する。`Db::profile_hash` は `todo!()` のスタブにしてコミットする
   - プロファイルの無い利用者なら `None` を返す
   - import した後なら `load_profile` と同じハッシュを返す
2. GREEN：`SELECT hash FROM profiles WHERE user_id = ?1` で実装する
3. REFACTOR：上の 6 か所を `profile_hash` に置き換える。mcp と web の 2 つの `viewer` は、利用者の決め方の説明が違う（stdio なのでオーナー／今は所有者だけ）。どちらも将来の変わり方が違いうるので、共通化はしない

---

## PR 4：LLM への依頼内容のモジュールを `src/prompt/` の下にまとめる

**今の状態**

LLM の処理は、それぞれ 2 つのモジュールに分かれている。

| 依頼内容（system prompt・スキーマ・応答の検証） | ステージの進行（DB から選ぶ・呼ぶ・保存する） |
| ----------------------------------------------- | --------------------------------------------- |
| `src/digest.rs`                                 | `src/pipeline/digest.rs`                      |
| `src/scoring.rs`                                | `src/pipeline/score.rs`                       |
| `src/translate.rs`                              | `src/pipeline/translate.rs`                   |
| `src/tidy.rs`                                   | `src/pipeline/tidy.rs`                        |

この分け方そのものは良い。ただ、次の点で読みにくい。

- 左の列のモジュールがクレート直下にあり、`http`、`robots`、`jst` などの基盤モジュールと並んでいる。直下のモジュールは合わせて 25 個ある
- 同じ名前が 2 つある。ステージのファイルでは `use crate::digest` と書き、`main` 側では `pipeline::digest` を `digest` として import する。読むたびに、どちらの `digest` かを確かめる必要がある
- 採点だけ `scoring` と `score` で名前がそろっていない

**変更**

- `src/prompt.rs`（外部由来のデータを無害化する `escape_data` だけがある）を `src/prompt/mod.rs` にする
- 左の列を `src/prompt/{digest,score,translate,tidy}.rs` へ `git mv` する（`scoring` は `score` に改名する）
- 呼び出し側は `use crate::prompt;` として `prompt::digest::build_prompt(..)` と書き、ステージのモジュールと見分けられるようにする
- 履歴を追えるように、2 つのコミットに分ける
  1. 中身を変えない rename
  2. `lib.rs` とパスの修正
- 動かさないもの
  - `src/extract.rs`：HTML から本文を取り出す処理で、プロンプトではない
  - `topics`・`glossary`・`profile`：ドメインのデータ

---

## PR 5：crawl・redo でステージを回す部分を lib へ移し、テストを書く

**今の状態**

`src/cmd/crawl.rs`（約 220 行）と `src/cmd/redo.rs`（約 120 行）はバイナリ側にあり、次のことをまとめて行っている。

1. 設定の読み込み、データディレクトリの決定、ロックの取得、DB を開く、シグナル処理の開始
2. LLM バックエンド（`ClaudeCli`）とクォータ（`Quota`）の用意。crawl と redo に同じコードがある
3. ステージを順に実行する。LLM ステージごとに `LlmStage { db, llm, quota: &mut quota, cancel }` を書いていて、この構造体リテラルが合わせて 6 か所にある
4. LLM が止まった理由の扱い
   - 認証切れなど利用者が対処すべき失敗は、最後にエラーにする
   - 利用上限に達したら、後続の LLM ステージを飛ばす
   - クォータの判定で止まったら、そのまま次へ進む
5. 最後の判定（中断・LLM の失敗・取得に失敗したソース）をエラーにして、終了コードにする。この判定も crawl と redo に同じコードがある

3 と 4 はパイプラインの振る舞いの中心なのに、バイナリの中にあるので lib のテスト（`FakeLlm` を使う）から呼べず、テストが 1 つも無い。

**変更（lib 側）**

- `src/llm/claude_cli.rs` に `ClaudeCli::from_config(cfg: &LlmConfig, cwd: PathBuf) -> Self` を追加する。HTTP クライアントの `Fetcher::from_config` と同じ形にする
- 新しいモジュール `src/pipeline/run.rs` を作る
  - `RunEnv<'a, L> { db, llm, quota: &'a mut Quota, cancel }`：`fn stage(&mut self) -> LlmStage<'_, L>` を持たせ、6 か所の構造体リテラルをなくす
  - `RunReport { failed_sources: usize, llm_failure: Option<String>, cancelled: bool }`：実行の結果。これをどのエラー・終了コードにするかは、バイナリが決める
  - `RunError`：`Db` と、各ステージのエラー型（`FetchError`、`ExtractStageError`、`DigestStageError`、`ScoreStageError`、`TranslateStageError`、`TidyStageError`）を `#[from]` でまとめる
  - `pub async fn crawl<L: Llm>(env, stages: &[Stage], opts: CrawlOptions { requests_only, force_tidy }, config: &Config, sources: &[Source], fetcher: &Fetcher) -> Result<RunReport, RunError>`：今の `cmd/crawl.rs` のステージのループを、中身を変えずに移す
  - `pub async fn redo<L: Llm>(env, kind: RedoKind, model: String, target: Target, config: &Config) -> Result<RunReport, RunError>`
  - 止まった理由を扱う `report_halt` を `cmd/mod.rs` からここへ移す

**変更（バイナリ側）**

- `cmd/crawl.rs` と `cmd/redo.rs` には、上の 1（設定・ロック・DB・シグナル）を用意して `pipeline::run` を呼ぶことだけを残す
- `cmd/mod.rs` に `fn finish(report: RunReport) -> Result<(), Error>` を置き、crawl と redo で共有する。優先順位は今と同じにする
  1. 中断 → `Interrupted`（終了コード 130）
  2. LLM の失敗 → `LlmFailed`
  3. 取得に失敗したソースがある → `SourcesFailed`
- `main.rs` のエラー型 `Error` から、ステージごとの 6 つの variant を消し、`Run(#[from] RunError)` にまとめる。ロックはバイナリで取るので、`Lock` は残す（2 重起動は終了コード 75）

**コミット**

1. `ClaudeCli::from_config` を追加し、crawl・redo の重複を置き換える
2. ステージのループを `pipeline::run` へ移す（移動のみ）
3. 移した振る舞いを固めるテストを追加する。移動後の振る舞いを記録するテストなので、最初から通る

| 状況                                  | 確かめること                                                                                             |
| ------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| digest が `Halt::LlmFailed` で止まる  | score・translate・tidy は LLM を呼ばず（`FakeLlm` が受け取った依頼の数が増えない）、`llm_failure` が残る |
| digest が `Halt::Quota` で止まる      | 後続の LLM ステージは実行される                                                                          |
| 実行前に `Cancel::request()` しておく | `cancelled` になり、LLM を呼ばない                                                                       |
| redo の digest                        | 採点のための予約（`score_reserved_calls`）を 0 にして、指定したモデルで要約する                          |

   テストでは、ネットワークが要る Fetch と Extract をステージの一覧から外し、`Db::open_in_memory` と `llm::fake::FakeLlm` を使う。種データは `src/pipeline/digest.rs` などの既存のテストと、`src/db/test_support.rs` の補助関数で作る。

---

## 検討して対象外にしたもの

- `web/feed.rs` と `web/html/mod.rs` にある 2 つの `escape`：XML は `'` を `&apos;` にし、XML 1.0 に書けない制御文字を落とす。HTML は `&#39;` にする。仕様が違うので、分けたままにする
- `src/db/read.rs`（1318 行）：実装は約 600 行で、残りはテスト。分ける必要はまだない
- `src/db/` の各サブモジュールを `pub use x::*` で平らに公開していること：全体で一貫しているので変えない

## 確認

- 各 PR：`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
- PR 4 の後：`git log --follow src/prompt/digest.rs` で、改名前の履歴がつながっていることを確かめる
- PR 5 の後：r995 の DB のコピーに対して、次を確かめる
  - `nucrawler crawl --until digest --max-llm-calls 1` と `nucrawler redo digest ...` を流し、ログと終了コードが移す前と同じ
  - 2 重起動は 75、Ctrl-C は 130 で終わる
