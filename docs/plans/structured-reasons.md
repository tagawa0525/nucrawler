# 推薦理由の構造化（計画 5 の詳細）

全体計画 [https-chatgpt-com-c-6ab90926-7618-83ee-b-enumerated-lagoon.md](https-chatgpt-com-c-6ab90926-7618-83ee-b-enumerated-lagoon.md) の 5 を実装に落とす。

## Context

推薦の点数には、LLM が書いた 1 文の理由（`reason`）しか付いていない。
なぜその点数なのかを、プロファイルと突き合わせて確かめる手段が無い。
一覧のカードには理由が出ず、点数だけが並ぶ。

PR #88 以降、点数はプロファイルだけで決まる。そこで、採点のたびにプロファイルのどの関心分野に当たったか（`matched`）と、どの「推薦しない話題」に当たったか（`excluded`）を LLM に返させる。
値はプロファイルの語だけに縛る（JSON Schema の enum）。こうすると、理由は LLM の自由な作文ではなく、判断の根拠であるプロファイルの項目そのものになる。
一覧で「炉心・燃料 / 規制」のように見えれば、点数の意味が読める。プロファイルの見直し（`profile suggest`）で、どの分野が効いているかを確かめる手がかりにもなる。

## 5a `feat/score-matches`：採点の出力と保存

- `prompt::score::schema(profile)`：各 item に `matched`（interest の topic の enum の配列）と `excluded`（exclude の enum の配列）を加える。
  - プロファイルに interest や exclude が無ければ、その配列は `maxItems: 0` にする（空の enum は JSON Schema として不正なため）。
- `system_prompt`：出力の節に、`matched`・`excluded` の意味と「上の一覧の名前をそのまま使う」ことを書く。
- `parse(output, requested, profile)`：`matched`・`excluded` がプロファイルの語でなければ、その item はスキーマ違反として捨てる（`missing` に回り、再試行される）。
  - 応答の 1 件は `Parsed.items` の要素 `Scored { id, score, reason, matched, excluded }` にする（今の 3 つ組から変える）。
- `PROMPT_VERSION = 3`。版 2 はまだデプロイしていないので、デプロイ時の再採点は 1 回で済む。
- migration `0021_score_matches.sql`：
  - `score_matches(score_id REFERENCES scores ON DELETE CASCADE, kind CHECK IN ('interest', 'exclude'), topic, PRIMARY KEY (score_id, kind, topic)) WITHOUT ROWID`
  - 分野での絞り込みや集計ができるよう、JSON の列ではなく行で持つ。
- `Db::insert_score` は当たった語も受け取り、採点と同じトランザクションで書く。
- 採点ステージは `parse` の結果をそのまま渡す。

## 5b `feat/show-score-matches`：表示

- `ListItem` に `matched: Vec<String>`、`excluded: Vec<String>` を加え、一覧・詳細の読み出しで `score_matches` から引く（`score_id` は既に選んでいる）。
- 一覧のカード：点数の横に当たった分野を小さく並べる。除外に当たったら「除外: 核融合」と出す。
- 詳細画面：点数・分野・理由を並べる。
- JSON API（`/api/articles`）と MCP の `ArticleSummary` にも同じ項目を出す（どちらも `reason` を出しているため、揃える）。

## 検証

- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`
- 5a：本番 DB の写しで `crawl --only score --max-llm-calls 1` を実行し、`score_matches` に行が入ることを確かめる。
- 5b：写しで `serve` し、一覧と詳細に分野が出ることを確かめる。
