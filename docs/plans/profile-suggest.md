# プロファイルの更新案 `nucrawler profile suggest`（計画 4 の詳細）

全体計画 [https-chatgpt-com-c-6ab90926-7618-83ee-b-enumerated-lagoon.md](https-chatgpt-com-c-6ab90926-7618-83ee-b-enumerated-lagoon.md) の 4 を実装に落とす。

## Context

PR #88 で、score のプロンプトから直近の反応を外した。点数は関心プロファイルと記事だけで決まり、👍・ブックマーク・👎・見送りといった反応は採点に効かない。
反応を推薦に効かせる経路は、プロファイルの見直しだけになった。しかし今は、人が TOML を手で直して `profile import` するしかなく、反応のどこから何を直せばよいかは見えない。

`profile suggest` は、溜まった反応を根拠に LLM がプロファイルの更新案を作り、ファイルに書き出す。

- 案は取り込まない。人が差分と根拠を読み、`eval --profile`（#89）で現行と比べてから `profile import` する。
- 反応を推薦に効かせる経路を、「提案 → 評価 → 人の承認 → 取り込み」の順にそろえる。取り込むと profile_hash が変わり、既存の仕組みで `backlog_days` の範囲が再採点される。

## 手順（利用者から見た流れ）

```sh
nucrawler profile suggest --out new.toml   # 差分と根拠を表示し、案を new.toml に書く
nucrawler eval --profile new.toml          # 現行と AUC などを比べる
nucrawler profile import new.toml          # 採るなら取り込む
```

## 根拠の集計

- ラベルは `eval` と同じ `Db::eval_labels`：記事ごとに、残っている明示的な反応のうち最後のもの。up・bookmark は関心、down・dismiss は不要。
- 記事ごとに、利用者が閲覧できる最新の digest の見出し（title_ja）とトピックを付ける。digest の無い記事は数えない。
- トピックごとに関心と不要の件数を数える（1 記事が複数のトピックを持てば、それぞれに数える）。
- LLM には次を渡す。
  - 現行のプロファイル（TOML）
  - トピックごとの件数
  - 反応した記事の見出しとトピック（新しい順に最大 100 件）
- 見出しは外部由来のデータなので、`<reaction>` タグで区切って `escape_data` で無害化し、中の指示に従わないよう system prompt に書く（score・digest と同じ扱い）。

## LLM への依頼（`src/prompt/suggest.rs`）

- 出力は、同じ形の新しいプロファイル全体と、変更ごとの根拠。
  - `interests: [{topic, weight, note}]`（weight は 0〜1、note は空でもよい）
  - `exclude: [string]`
  - `reasons: [{change, evidence}]`（何を変えたか、どの件数・見出しに基づくか）
- 規則として system prompt に書くこと。
  - 反応の件数を根拠にした変更だけをする。反応が無いことは関心が無いことの根拠にしない（表示されていない記事には反応できないため）。
  - 件数が少ないうちは控えめに変える（重みは一度に大きく動かさない、分野を消さない）。
  - 不要が続く話題は、重みを下げるか exclude に加える。
  - 関心が続くのに既存の分野に当たらない話題は、分野として加える。
  - 既存の分野の名前と note は、変える根拠が無ければそのまま残す。
- 応答の検証は `profile::parse` と同じ規則に通す（topic は空でなく重複しない、weight は 0〜1）。通らなければ案を書かずにエラーにする。

## 差分の表示（`src/profile.rs` に純粋関数 `diff`）

LLM の説明をそのまま信じず、現行と案の差分は Rust で計算して表示する。

- 追加・削除した分野、重みの変化（0.9 → 0.7）、note の変化、exclude の追加・削除。
- そのあとに LLM が挙げた根拠を並べ、最後に次の手順（`eval --profile FILE`）を案内する。
- 差分が無ければ「変更の提案はありません」と出し、ファイルは書かない。

## 実行

- `profile suggest --out FILE [--max-llm-calls N]`。`--out` は必須で、既存のファイルは上書きしない（誤って手元の TOML を消さないため）。
- LLM の呼び出しは 1 回。モデルは `llm.score_model`（採点と同じ判断をするため）。`llm_calls` にステージ名 `suggest` で記録する。
- ロック・クォータ・シグナルは `eval --profile`（`src/cmd/eval.rs`）と同じ。
- ラベルが 0 件ならエラーにする（根拠が無い）。正例・負例のどちらかが 5 件未満なら、`eval` と同じく参考程度だと注記する。
- プロファイルが未登録ならエラーにする（見直す元が無い）。

## PR の分け方

### 4a `feat/profile-suggest-prompt`：根拠の集計・依頼内容・差分（LLM は呼ばない）

- `Db::label_evidence(user_id, limit) -> Vec<Evidence { article_id, positive, title_ja, topics, at }>`（`src/db/eval.rs`）
- `src/prompt/suggest.rs`：`system_prompt`、`build_prompt(profile, evidence)`、`schema`、`parse -> Suggestion { profile, reasons }`
- `profile::diff(current, proposed) -> Vec<Change>` と表示用の `render_diff`
- テスト：集計（最新の digest のトピック、digest の無い記事は除く、上限と新しい順）、プロンプト（件数・区切り・無害化）、parse（規則違反を拒む）、diff（追加・削除・重み・note・exclude、変更なし）

### 4b `feat/profile-suggest-command`：呼び出しとコマンド

- `pipeline::suggest::suggest_profile`（`call_recorded` で 1 回呼ぶ。`tidy` と同じ形）
- `src/cmd/profile.rs` などに `profile suggest` を置き、CLI の解釈、README を追加する
- テスト：FakeLlm で案が返ること、形の崩れた応答はエラーにして書かないこと、CLI の解釈（`--out` 必須、上書きしない）

## 検証

- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`
- 本番 DB の写しで `profile suggest --out` を実行し、差分と根拠が読めること、案を `eval --profile` に渡せることを確かめる（LLM の呼び出しは suggest の 1 回と、eval の採点の数回）。
