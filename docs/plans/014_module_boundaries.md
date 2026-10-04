# モジュールの境界を整える（高凝集・疎結合）

## 背景

2026-10-04 時点のコード（Rust 約 5 万行、テスト込み）を、高凝集・疎結合の観点で見直した。トップレベルの
モジュールどうしが `crate::` で参照し合う向きを機械的に数え、大きいファイル（`db/read.rs`、`web/html/list.rs`、
`web/server/pages.rs`、`pipeline/*.rs`）は中の関数を責務ごとに分けて確かめた。

全体としては整っている。`web/html`（描画）は DB を呼ばず、`web/server`（ハンドラ）は HTML を組み立てない。
`cli.rs` は引数の解析だけ、`main.rs` と `src/cmd/` の分担（LLM を流すサブコマンドは `cmd/`、短い DB 操作は
`main.rs`）も一貫している。手を入れるのは、次の 2 種類に限る。

- **依存の逆流**：下の層（`llm`・`pipeline`・`search`・`mcp`）が上の層や、たまたま隣にあった別の責務の
  モジュールを参照している。片方を変えると無関係な側のビルドやテストに波及する
- **1 ファイルに別の責務が同居**：ほかのファイルが「本来そこに無いはずのもの」を取りに来ている。
  ファイル名から中身を予想できず、変更の影響範囲を見誤る

どれも挙動は変えない。既存のテストがそのまま回帰テストになるので、TDD の RED は無い（リファクタリングのみ）。

## 決めたこと

PR は 5 つに分け、上から順にマージする。各 PR の中も、1 コミット 1 つの移動にする。

### PR 1：依存の逆流をなくす

| 今                                                                                                     | 困ること                                                                                    | 変更                                                                                                                                                 |
| ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pipeline::run` が作り直す成果物の種類 `RedoKind` を `cli` から取る                                    | パイプラインが引数の解析器に依存する。`cli` は既に `pipeline::Stage` を使っており、循環する | `RedoKind` を `pipeline` に置き、`cli` が使う                                                                                                        |
| LLM のバックエンド（`llm::claude_cli` など）が呼び出しの枠 `Slot` を `pipeline::lock` から取る         | `llm` が、自分を呼ぶ側の `pipeline` に依存する                                              | flock の部品（`try_lock`・`LockError`）を `filelock` に、呼び出しの枠を `llm::slot` に移す。`pipeline::lock` は実行のロック（fetch・tidy）だけを持つ |
| `mcp` がソースの表示名の表 `SourceLabels` を `web::html` から取り、`main.rs` が同じ組み立てを 2 回書く | MCP サーバが Web 画面の描画モジュールに依存する                                             | `SourceLabels` を設定（`config`）側に置き、`Sources::labels()` で作る                                                                                |
| CLI の検索結果の 1 行（`search::result_line`）が見出しの選び方 `display_title` を `web::html` から取る | CLI の出力が Web の描画に依存する                                                           | 一覧の 1 行 `db::ListItem` のメソッドにする（要約の見出し・原題・URL の順に空でないもの）                                                            |

あわせて、テスト用の一時ディレクトリの補助が 3 箇所目（`config`・`pipeline::lock`・`llm::slot`）になるので、
`testutil::temp_dir` にまとめる。

### PR 2：DB の共通の SQL 断片を「画面向けの読み出し」から出す

`db/read.rs`（テストを除いて約 1000 行）は冒頭に「画面向けの読み出し（一覧・検索・記事の詳細）」とあるが、
DB のほぼすべてのファイルが使う SQL の断片もここにある。

- `latest_digest`・`latest_title_translation`・`title_ja`・`matched_topics`・`linked_topics`・`latest_digests`・
  `viewable`・`lang_code`：`db/` の 10 ファイル以上が使う。→ 新しい `db/sql.rs` に移す
- `ContentSet`・`public_contents`（要約・和訳の LLM への入力にする本文）：使うのは `artifacts`・`translate`・
  `redo` で、入力の型 `InputContent` は `db/artifacts.rs` にある。→ `db/artifacts.rs` に移す

`read.rs` には一覧・検索・詳細・確認枠の問い合わせだけが残る。

### PR 3：Web の部品を、使う画面から独立させる

`web/html/list.rs`（テストを除いて約 830 行）に、一覧専用でない部品が 2 つ同居しており、検索と記事の詳細が
`super::list::` 経由で取りに来ている。

- 上部のバー（`BarView`・`bar`・`home_bar`・点数と ★ の選択・バーの script など）→ `web/html/bar.rs`
- 記事のカードと印（`card`・`marks`・`matches`・`score_badge`・`MARKS_SCRIPT` など）→ `web/html/card.rs`

`web/server/pages.rs` は前半の約 300 行が一覧のハンドラと、その引数の解釈（`ListParams`・`list_items` など。
JSON の API も使う）で、後半のフィード・検索・詳細・設定とは関係が無い。→ `web/server/list.rs` に移す。
一覧のハンドラの中に直書きされている確認枠の選び方（閾値・同じ報道の除外・既読とブックマークでの絞り込み）は
関数に切り出し、ハンドラは引数の解釈と組み立てだけにする。

一覧の URL の値（`hide-low`・`any` など）は、書き出しが `html/list.rs`、読み取りが `server/pages.rs` に分かれて
いて、片方だけ変えると食い違う。書き出しの隣に読み取りを置き、server は誤りを `BadRequest` に変えるだけにする。

### PR 4：LLM のステージの「予約を持つ記事だけ書く」手順を 1 箇所にする

要約・採点・和訳・見出しの和訳・同じ報道の判定の 5 ステージが、同じ手順を同じコメントごと手で書いている。

```rust
let outcome = workers.call(Call { .. }).await?;
// 結果を書く前に予約を延長する。呼び出しの最中に期限が切れてほかの実行に取り直された記事は
// 延長できないので、以降は保存も失敗の記録もしない（予約を持っている実行だけが書く）
let held = claim.renew(clock(), claim_ttl(llm_cfg))?;
let Some(response) = workers.settle(outcome, &mut summary.tally, held.iter().map(..), now)? else { break };
```

「呼び出しの後、settle の前に延長し、延長できた記事だけを書く」は二重書き込みを防ぐ正しさの条件だが、
型では守られず、5 箇所のどれかで順序を誤っても気づけない。

この手順（呼び出し・延長・settle）を `pipeline::llm_call::Workers` の 1 つのメソッドにまとめ、延長できた記事の
集合と応答を一緒に返す。応答を解釈できなかったときの失敗の記録も、同じ所にまとめる。何を選ぶか・プロンプト・
結果の保存は、ステージごとに違うので各ステージに残す（ステージ全体を 1 つの trait に押し込むと、かえって
読みにくくなる）。

### PR 5：小さな重複

- DB を開く `data_dir(data)?.join("nucrawler.db")` が `main.rs` と `src/cmd/` に 12 箇所ある。→ 開く関数を 1 つにする
- 外から見たサイトの URL `format!("http://{}", request_host(..))` が、フィード・設定・ログイン後の戻り先の
  3 箇所にある。`request_host` は認証に固有ではない。→ `web/server/mod.rs` の `base_url` にする

## やらないこと

- **`web/server/auth.rs` の分割**：約 450 行あるが、セッション・同一オリジンの確認・IP ごとの制限・
  パスワード・フィードのトークンと、すべて認証の責務に収まっている
- **要約と和訳の「訳語集の変更による作り直し」の共通化**：2 箇所だけ。3 つ目のステージが対応したときに考える
- **`pipeline::run` の crawl のステージごとの分岐**：数える項目がステージごとに違い、まとめると読みにくくなる
- **`profile` が名前に使えない文字の規則を `prompt` から取る**：プロファイルはプロンプトに 1 行ずつ埋め込むので、
  規則の持ち主は `prompt` で正しい
- **`recommend` が評価の型 `db::Rating` を使う（`db` も `recommend` の特徴を使う）**：値の型を共有しているだけで、
  処理の依存ではない
- **`db/` の各ファイルの `use super::*`**：DB の内部の書き方としてそろっており、変えると差分が大きいわりに得るものが少ない
