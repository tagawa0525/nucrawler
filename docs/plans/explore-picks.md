# 閾値未満の確認枠（計画 6 の詳細）

全体計画 [https-chatgpt-com-c-6ab90926-7618-83ee-b-enumerated-lagoon.md](https-chatgpt-com-c-6ab90926-7618-83ee-b-enumerated-lagoon.md) の 6 を実装に落とす。

## Context

一覧は `web.min_score`（既定 50）未満の記事を既定で隠す。
そのため、点数が低すぎて表示されなかった記事に本当は関心があったかどうか（見逃し）は、反応として観測できない。
`eval` の AUC も、表示された記事の反応だけで測るので偏る。
`profile suggest` も、表示されなかった記事の反応は得られないので、見逃しを根拠にできない。

閾値未満の記事から毎日少しだけ無作為に選び、一覧の「確認枠」に出して反応を集める。
閾値のすぐ下だけを選ぶと境界付近しか確かめられないので、無作為に選ぶ。無作為なら見逃し率を偏りなく見積もれる。

## 6a `feat/explore-picks`：確認枠を出す

- 設定 `web.explore_per_day`（既定 2、0 なら出さない）。
- migration `0022_explore_picks.sql`：`explore_picks(user_id, article_id, picked_on, PRIMARY KEY (user_id, article_id))`
  - `picked_on` は日本時間の日付（YYYY-MM-DD）。
  - 1 記事は 1 回しか選ばない。
- `Db::explore(q, per_day, today)`：その日の確認枠の記事を返す。
  - その日に選んだ記事が `per_day` 件に満たなければ、候補から足りない分を `random()` で選んで記録する。同じ日のうちは同じ記事を出し、再読み込みで変わらない。
  - 候補の条件は次のとおり。
    - 一覧と同じ期間（`list_days`）の記事である。
    - 軽水炉に関係する。
    - 採点済みで、点数が `min_score` 未満である。
    - まだ選んでいない。
    - 明示的な反応（`eval_labels`）が無い。
  - 返す記事は、その日に選んだもののうち、まだ明示的な反応が無いもの。反応すれば枠から消える。
- 一覧の画面：既定の一覧（「すべて表示」でないとき）の下に「確認枠」の節として出す。
  - カードは他と同じく振り分け（ブックマーク・見ない）の対象にする。
  - 説明の文を添える：「おすすめの閾値に届かなかった記事から無作為に選んでいます。関心があれば 🔖、無ければ見送ってください」。

## 6b `feat/eval-explore`：見逃し率を出す

- `eval` の出力に、確認枠で選んだ記事のうち反応が付いたものの内訳（関心・不要）を出す。
- 関心の割合を見逃し率の見積もりとして示す。例：`explore: 12 picked, 5 with reactions (1 positive, 4 negative)`

## 検証

- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`
- 本番 DB の写しで `serve` し、一覧の下に確認枠が 2 件出ること、再読み込みで変わらないことを確かめる。
