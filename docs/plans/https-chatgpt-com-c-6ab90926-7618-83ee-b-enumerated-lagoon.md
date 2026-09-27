# 推薦の学習・評価ループと取得の健全性監視

## Context

2026-09-27、外部レビュー（ChatGPT に nucrawler の改善点を挙げさせたもの）を受けて、コードと照合して根本原因から改善案を練り直した。
レビューの主張は「機能は揃っている。未成熟なのは、推薦品質を感覚ではなくデータで改善する評価ループと、crawler の信頼性」。
照合で分かった事実（2026-09-27 時点の main、c9d4096）：

1. **反応が推薦に定着しない**。👍/👎 などの反応は `src/prompt/score.rs` の system prompt に「直近 20 件の見出し」（`SIGNALS = 20`、`src/pipeline/score.rs:17`）として入るだけ。
   21 件目より古い反応は忘れられ、関心プロファイル（`profile.toml`、分野・重み・note・exclude）は人が `profile import` しない限り変わらない。
2. **点数を再現できない**。`scores` の一意キー `(user, artifact, profile_hash, backend, model)` に反応が含まれないため、同じ記事でも採点時点の直近 20 件によって点数が変わる。
   score プロンプトには版（`PROMPT_VERSION`）も無く、プロンプトを変えても再採点されない（digest・translate には版がある）。
3. **評価の仕組みが無い**。ただし `events`（up/down/bookmark/dismiss/open_detail/open_translation）には正解ラベルになる反応が既に溜まっている。
   閾値（`web.min_score`、既定 50）未満の記事は表示されないので、見逃し（偽陰性）は観測できない。
4. **取りこぼしが成功扱い**。`src/pipeline/fetch.rs:43-47` は候補 0 件でも `record_source_success` を呼ぶ。
   件数（total/matched/new）はログに出るだけで保存されず、重複件数は数えていない。警告（`src/db/warnings.rs`）は `SourceFailing` と `LlmFailed` だけ。

目指す姿：**プロファイルが推薦の唯一の判断根拠**になり、反応はプロファイルの更新案として人の承認を経て定着する。更新案は過去の反応による**オフライン評価**で採否を判断する。
点数は（プロファイル・記事・prompt 版・モデル）で決まり、理由はプロファイルのどの関心分野に当たったかで説明できる。
crawler は「成功したが中身が空」を検出できる。

## 全体の順序（1 worktree で 1 PR ずつ、マージしてから次へ）

| # | PR                                                                                                                                                        | 依存 |
| - | --------------------------------------------------------------------------------------------------------------------------------------------------------- | ---- |
| 1 | score の prompt 版管理（1a 保存・再採点、1b 読み出しは最新版を優先）                                                                                      | —    |
| 2 | 取得の健全性（2a 件数の保存、2b status に表示、2c 件数急減・0 件の警告、2d 新着途絶の警告）                                                               | —    |
| 3 | オフライン評価 `nucrawler eval`                                                                                                                           | 1    |
| 4 | プロファイル更新案 `nucrawler profile suggest`（score プロンプトから直近の反応を外すのは 3b の前に前倒しした。`docs/plans/offline-eval.md` の 3b を参照） | 1, 3 |
| 5 | 推薦理由の構造化（当たった関心分野）                                                                                                                      | 1, 4 |
| 6 | 閾値未満の確認枠                                                                                                                                          | 3    |

- 各 PR は TDD。RED コミットでは新しい関数を `todo!()` のスタブにしてテストがコンパイルできるようにする（前例：`06835b9`）。GREEN でそれを実装する。
- migration は `src/db/mod.rs` の `MIGRATIONS` に追加する。番号はマージ順に振る（現在は 0017 まで）。migration 中は外部キーが無効なので、テーブルを作り直してよい（前例：0010、0017）。
- migration のテストは `src/db/mod.rs` の `migration_rebuilds_artifacts_keeping_references` を雛形にする。
- 3〜6 は着手前に、その PR 用の詳細計画を別ファイルで作る（ここでは設計判断までを書く）。

---

## 1. score の prompt 版管理

### 1a `feat/score-prompt-version`

- `src/prompt/score.rs` に `pub const PROMPT_VERSION: i64 = 1;` を置く。doc コメントは `src/prompt/digest.rs:10` に倣う。既存の行は今のプロンプトで作られたので 1 にする。
- migration `00NN_score_prompt_version.sql`：UNIQUE 制約を変えるので `ADD COLUMN` では足りない。0017 に倣ってテーブルを作り直す。
  - `scores_new` に `prompt_version INTEGER NOT NULL` を加え、`UNIQUE (user_id, artifact_id, profile_hash, backend, model, prompt_version)` にする。
  - 既存の行は `prompt_version = 1` で移す。
  - `scores_by_artifact` 索引を作り直す。`scores` を参照するテーブルは無い。
- `src/db/score.rs`：
  - `ScoreKey` に `pub prompt_version: i64` を加える。
  - `pending_score` の `NOT EXISTS` に `AND s.prompt_version = ?` を加える。
  - `insert_score` で版を書く。
  - `src/pipeline/score.rs:56` で `prompt::score::PROMPT_VERSION` を渡す。
- 版を上げたときの再採点は、既存の `cutoff`（`pipeline.backlog_days`）の範囲に限られる。
- `score_stage` のキーには版を入れない（digest の `stage_errors` と同じ扱い）。その結果、古い版で諦めた記事は新しい版でも再試行されない。このことを doc コメントに書く。
- RED テスト：
  - `mod.rs` `migration_adds_prompt_version_to_scores`：既存の行が版 1 になる。同じキーでも版 2 なら入り、同じ版の 2 回目は失敗する。
  - `score.rs` `pending_score_rescores_when_prompt_version_changes`。
  - `pipeline/score.rs` の既存テスト（275 行付近）で、保存された版が `PROMPT_VERSION` であることを確かめる。
  - `test_support` の `score_key` ヘルパを更新する。

### 1b `feat/read-latest-score-version`

- 読み出しでは (user, profile, digest) ごとに**最も新しい版**の点数を使い、その中でモデル間の最大値を取る。
  - 現行の版だけに絞ると、`backlog_days` より古い記事や再採点待ちの記事の点数が消え、一覧から見えなくなる。
  - 版をまたいだ最大値にすると、古い版の高い点数が新しいプロンプトの結果を隠してしまう。
  - digest の読み出しも、最新の成果物を使い版では絞らないので、それと揃う。
- 変更箇所は 3 つ（ヘルパは作らない）：
  - `src/db/read.rs:498-502` の `score_id` サブクエリ
  - `src/db/translate.rs:117` の `max(s.score)`
  - `src/db/redo.rs:228` の `REDO_FILTER`
  - いずれも `ORDER BY s.prompt_version DESC, s.score DESC ... LIMIT 1` にする。
- RED テスト：`read.rs` `list_prefers_latest_score_prompt_version`（v1 で 90 点、v2 で 40 点なら 40 点を表示）。translate・redo にも同じ形のテストを 1 件ずつ加える。

---

## 2. 取得の健全性

### 2a `feat/fetch-runs`：取得 1 回ごとの件数を保存する

- migration `00NN_fetch_runs.sql`：
  - `fetch_runs(id, source_id, fetched_at, total, matched, new, duplicate)` を作る。
  - CHECK 制約：`matched <= total`、`new + duplicate <= matched`。URL が不正な候補は飛ばすので、等号にはならないことがある。
  - 索引 `(source_id, fetched_at)` を張る。外部キーは付けない（`source_state` と同じ）。
- `src/db/sources.rs`：
  - `pub struct FetchCounts { total, matched, new, duplicate }`
  - `record_fetch_run(&self, source_id, &FetchCounts, at: DateTime<Utc>)`。時刻は引数で受け取る（`record_llm_call` と同じ）。
- `src/pipeline/fetch.rs`：
  - `store()` は `Stored { new, duplicate }` を返し、`Ok(None)` のときに duplicate を数える。
  - `fetch_sources` は `now` を受け取る（`src/pipeline/run.rs:91` から `(env.clock)()` を渡す）。
  - 成功したら `record_fetch_run` を呼び、ログにも matched と duplicate を出す。失敗した回は行を作らない。
- RED テスト：
  - `sources.rs` `records_fetch_runs`
  - `fetch.rs` の `rerun_adds_nothing_new` を拡張する。2 回取得すると (n,1,1,0)、次に (n,1,0,1) が入る。失敗したソースの行は作られない。

### 2b `feat/status-fetch-counts`

- `SourceOverview` に `last_run: Option<FetchCounts>` を加える。
- `source_overview()` はソースごとに最新の行を `ROW_NUMBER() OVER (PARTITION BY source_id ORDER BY fetched_at DESC, id DESC)` で取る。
- `status::render` は各行に `last fetch 25/3 (new 1, dup 2)` を付ける。
- RED テスト：`overview_combines_articles_and_state` と `renders_sources_in_config_order_with_state` を拡張する。

### 2c `feat/warn-source-drop`：件数の急減と 0 件を警告する

- 規則。対象はソースの最新の回で、それが `since` 以後のもの（Web は 24 時間を渡すので、設定から消したソースの警告は自然に消える）。
  - `total == 0` なら履歴が無くても警告する。一覧やフィードが空なのは、ほぼ確実に壊れているため。
  - それ以外は、直前の最大 20 回（既定の 1 日 4 回で約 5 日分）のうち 8 回以上があれば、`latest.total * 3 < 中央値` で警告する。
    - total は絞り込み前の件数なので、ページやフィードの大きさを表し、普段は安定している。
    - 中央値は一時的な増減に強い。1/3 という閾値は、週末などの半減は無視しつつ、急減は捉える。
- 新しい列挙子 `Warning::SourceDropped { source_id, total, median: Option<i64>, at }`。median が None なのは履歴不足で 0 件だったとき。
- 判定は純粋関数 `fn dropped(latest: i64, previous: &[i64]) -> Option<Drop>` に置き、定数を持たせる。
- 表示は既存の経路を使う（`src/web/server/pages.rs:25` → `src/web/html/mod.rs` の `warning_banner`）。
  - 文言：`⚠ {source} の取得件数が減っています（{at}）：{total} 件（直近の中央値 {median} 件）`
  - 0 件で履歴不足のとき：`⚠ {source} の一覧が 0 件でした（{at}）`
- RED テスト：
  - `dropped` の単体テスト：履歴不足、全部 0、通常の減少、0 件への急減、偶数個の中央値、履歴なしで 0 件
  - `warnings.rs` `warnings_report_dropped_sources`
  - バナーの文言

### 2d `feat/warn-source-stale`：新着が普段より長く途絶えたら警告する

- total が正常でも、日付の書式が変わったり絞り込みが外れたりすると新着は 0 件のままになる。2c では拾えないので別に判定する。
- 新しいテーブルは作らず、既存の `articles.fetched_at` から判定する。そのため過去の履歴がすぐに使える。
- 規則（ソースごと）：
  - 直近 60 日の、新着があった日の間隔の中央値 g を求める。間隔が 5 個未満なら判定しない。
  - 最後の新着から `max(7 日, 3 × g)` を超えたら警告する。
  - 定数は 2c と同じく、純粋関数と定数に置く。
- 新しい列挙子 `Warning::SourceStale { source_id, last_new_at, typical_gap_days }`。
- 文言：`⚠ {source} の新着が {n} 日ありません（普段は {g} 日おき）`
- RED テスト：純粋関数（毎日更新されるソース、週 1 回のソース、履歴不足）と、`warnings.rs` の統合テスト。

---

## 3. オフライン評価 `nucrawler eval`（設計判断）

- **ラベル**：記事ごとに、最後に付いた明示的な反応で決める。
  - 正例：up・bookmark。bookmark は現在も付いているもの。
  - 負例：down・dismiss。
  - open_detail・open_translation は使わない。点数が高い記事ほど開かれやすいので、点数の結果であって正解にならない。
- **指標**：評価するキー（profile_hash, backend, model, prompt_version）ごとに次を出す。
  - AUC：正例と負例の組のうち、正例の点数が高い割合。同点は 0.5 とする。
  - 点数帯（10 点刻み）ごとの正例と負例の件数。
  - カバー率：ラベルのうち、そのキーで採点済みのものの割合。
  - 既定は現行のプロファイルと版で、`--all` で過去のキーも並べて比べる。
- **候補プロファイルの評価**：`nucrawler eval --profile FILE` は、ラベルの付いた記事を候補のプロファイルで採点してから比較する。
  - 採点は既存の score の仕組み（`pipeline/score.rs`、quota）を使い、`backlog_days` の制限は外す。
  - 結果は候補の profile_hash で `scores` に保存する。一覧は現行の hash で引くので表示には影響しない。候補を採用すればそのまま使われる。
- **漏れの注意**：直近の反応を score プロンプトに入れている間（4 の前）は、再採点のとき記事自身の見出しがシグナルに入りうる。その場合の AUC は高く出る。
  eval の出力に注記し、4 でプロンプトからシグナルを外すと解消する。
- 実装場所：集計は `src/db/eval.rs`、表示は `src/eval.rs` の純粋関数 `render`（`src/status.rs` と同じ形）。CLI は `src/cli.rs` に `Command::Eval` を加える。

## 4. プロファイル更新案 `nucrawler profile suggest`（設計判断）

- 3 のラベルを、記事の要約に付いた topics ごとに集計する（正例と負例の件数、代表の見出しを数件）。
- それを現行のプロファイルと一緒に LLM に渡し、**同じ形の新しいプロファイル**（分野・重み・note・exclude）と、変更点ごとの根拠（件数）を返させる。
- 案は TOML ファイルに書き出し、差分と根拠を表示するだけにとどめる。DB には保存しない。
- 人の手順：`profile suggest --out new.toml` → `eval --profile new.toml` で AUC を比べる → `profile import new.toml`。
  - 取り込むと profile_hash が変わり、既存の仕組みで `backlog_days` の範囲が再採点される。
- score の system prompt から直近の反応（`recent_signals`）を外し、score の `PROMPT_VERSION` を 2 にする。
  - これで点数は (プロファイル, 記事, 版, モデル) で決まる。
  - 反応はプロファイルを通してだけ推薦に効くようになる。
  - 当初は `suggest` と `eval` がそろってから外す予定だった（反応が推薦に全く効かない期間を作らないため）。
  - 2026-09-27、`eval --profile`（3b）の評価にラベルが漏れないよう、利用者と相談して 3b の前に前倒しした。ラベルは 10 件と少なく、効かない期間の影響は小さいと判断した。
- 自動で定期的に提案する仕組みは作らない。手動で回して、必要になったら考える。

## 5. 推薦理由の構造化（設計判断）

- score の出力スキーマに `matched: [string]` を加える。`schema()` がプロファイルを受け取り、interest の topic を enum に列挙する。exclude に当たった場合は `excluded: [string]` とする。自由文の `reason` は残す。
- 保存は、一覧で当たった分野から絞り込みに使えるよう、正規化したテーブル `score_matches(score_id, topic, kind)` にする。
- 表示：一覧のカードには点数の横に当たった分野を小さく並べる。詳細画面では分野と reason を出す。
- score の `PROMPT_VERSION` を上げる。4 のあとなので、分野はプロファイルと一致していて、判断根拠そのものを示す。

## 6. 閾値未満の確認枠（設計判断）

- 一覧に「確認枠」を設け、`web.min_score` 未満の記事から 1 日 `web.explore_per_day` 件（既定 2）を**無作為に**選んで出す。
  - 閾値のすぐ下の記事だけを選ぶと、境界付近しか確かめられない。無作為に選ぶので、見逃し率を偏りなく見積もれる。
- 選んだ記事は `explore_picks(user_id, article_id, picked_at)` に保存する。同じ日のうちは同じ記事を出し、再読み込みで変わらないようにする。
- 評価（3）では、確認枠の記事を別に集計し、そこで正例が付いた割合を見逃し率として出す。
- 確認枠で付いた反応も、通常のラベルとして 4 の更新案に入る。

---

## 今回採らなかったレビューの提案と理由

- **LLM 以外の特徴量で点数を作る仕組み**：特徴量の重みを決めるデータが無いうちは作れない。4 でプロファイルが学習するようになったあと、3 の評価で不足が見えたら検討する。
- **推薦ログ（impression・CTR）**：利用者が 1 人なので件数が少なく、閾値未満は表示されないので偏る。評価は 3（既存のラベル）と 6（確認枠）で代わりに行う。
- **MCP の拡張**：get_digest と get_translation は、既存の `get_article` が要約と和訳を返している。関連記事を返すには類似記事の判定が先に要る。
- **「今日の5分」ビュー**：既存の一覧（スコア順・min_score・前回訪問以降）と役割が重なる。3 の指標を見て不足があれば考える。
- **非ループバックへの bind 警告、URL 正規化の強化、抽出品質の判定**：どれも妥当な小さな改善。今回の範囲には入れず、別の機会に扱う。

## 検証

- 各 PR：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（CI と同じ）、`nix build`（Nix の sandbox でもテストが通るか）。
- 1：migration のテストに加え、r995 の DB の写しで `Db::init` が通り、`scores` の行数が変わらないことを確かめる。
- 2：`crawl --only fetch` のあとに `nucrawler status` で件数が出ること。テスト用の設定で selector をわざと壊し、Web のバナーに警告が出ること。
- 3：r995 の DB の写しで `nucrawler eval` を実行し、ラベルの件数、カバー率、AUC が出ることを確かめる。ラベルが少なすぎる場合はその事実を記録する。
- 4：`profile suggest` の案を `eval --profile` で比べ、採否を人が判断できる出力になっていること。
- 5・6：`nucrawler serve` で一覧と詳細の表示を確かめる。
