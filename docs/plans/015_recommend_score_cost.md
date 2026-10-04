# 推薦点の計算を、記事の特徴の側から重みを引く形にする（パフォーマンス）

## 背景

2026-10-04 に、稼働中の DB（r995、記事 1594 件）の複製で、Web UI の応答と DB の問い合わせの時間を測った
（release ビルド、同じ問い合わせを 3 回以上流した値）。

| 対象                                            | 時間      |
| ----------------------------------------------- | --------- |
| 一覧 `/`（既定の 7 日）                         | 60〜83 ms |
| 検索 `/search?q=原子力`（100 件）               | 80 ms     |
| 検索 `/search?q=reactor`                        | 30 ms     |
| 一覧の問い合わせを全期間・上限なしで（1230 件） | 278 ms    |
| 記事の詳細・設定                                | 1〜10 ms  |

一覧と検索の時間は、問い合わせが組み立てる記事の数に比例して伸びる（1 件あたり約 0.23 ms）。一覧の 1 画面は、
表示する記事の問い合わせのほかに、隠れている記事の数（`hidden_counts`）のために同じ問い合わせを上限なしで最大
5 回流すので、期間内の記事が増えるほど重くなる。

同じ SQL（`run_items_query` が組み立てるもの）を、列の式を 1 つずつ定数に置き換えて測ると、推薦点の式
（`db::recommend::recommend_score_sql`）だけで全体の約 95% を占めていた（430 ms → 外すと 23 ms。この表は
Python の sqlite3 で測ったので、関数の呼び出しの分だけ Rust より遅い）。ほかの列（同じ報道・既読・閲覧の制限
`viewable` など）はどれも外して 1 割未満しか変わらない。

### 原因

推薦点の式は、重みの JSON（`:rec_weights`。今は 55 個、約 2 KB）を行ごとに `json_each` で展開し、重みの
1 つずつについて「その記事の特徴か」を判定している。

```sql
SELECT total(w.value) FROM json_each(:rec_weights) AS w
WHERE w.key = 'source:' || rows.source_id
   OR w.key IN (SELECT 'topic:' || t.name FROM artifact_topics ... WHERE at.artifact_id = rows.digest_id)
   OR w.key IN (SELECT kind || ':' || topic FROM score_matches WHERE score_id = s.id)
```

`IN` の副問い合わせは行に相関しているので、重みの数だけ（記事 1 件につき 55 回ずつ）流れ直す。記事 1 件の
特徴は数個しかないのに、重みの側から全部を当てている。重みは評価が増えるほど（評価した記事のトピックが
増えるほど）増えるので、記事の数と重みの数の積で重くなる。

## 決めたこと

### 1. 記事の特徴を並べ、重みを引く

重みは問い合わせの冒頭の CTE で 1 回だけ展開して実体化し、行ごとには記事の特徴（ソース・トピック・当たった
関心分野と除外）を `UNION` で並べて、重みの表と結合する。

```sql
WITH weights AS MATERIALIZED (SELECT key, value FROM json_each(:rec_weights)), ...
recommend_score(s.score, (
  SELECT total(w.value) FROM (
    SELECT 'source:' || rows.source_id AS k
    UNION SELECT 'topic:' || t.name FROM artifact_topics ... WHERE at.artifact_id = rows.digest_id
    UNION SELECT kind || ':' || topic FROM score_matches WHERE score_id = s.id) AS f
  JOIN weights AS w ON w.key = f.k))
```

`UNION` で特徴の重複を除き、重みのキーは JSON のオブジェクトなので一意だから、「キーが特徴に当たる重みを
1 回ずつ足す」という今の規則は変わらない。同じ複製で、全期間の一覧は 430 ms → 47 ms、検索（原子力）は
110 ms → 22 ms になり、結果の行は列まで一致した。

式（`recommend_score_sql`）と、それが参照する CTE（`weights`）は対になるので、どちらも `db/recommend.rs` に
置き、`run_items_query` は両方を埋め込む。

### 2. `items` の CTE を実体化する

1 の後に残る分では、記事ごとの最新の要約（`latest_digest`。閲覧の制限も見る）が 1 行につき 3 回評価されている。
`items` の CTE が呼び出し側に展開され、`digest_id` を参照する所（要約の結合・採点の選択・推薦点のトピック）
ごとに副問い合わせが流れ直すため。`items` を `MATERIALIZED` にして 1 回で済ませる（47 ms → 40 ms、
検索は 22 ms → 20 ms。結果は一致）。

### 確かめ方

挙動は変えないので、TDD の RED は無い。一覧・検索・確認枠・推薦点の補正の既存のテストがそのまま回帰テストに
なる。速さは PR の説明に、変更の前後で同じ複製を測った値を書く。

PR は 1 つで、1 と 2 を別のコミットにする。

## やらないこと

- **時間を測るテスト**：CI の負荷で揺れ、しきい値を緩めるしかなくなる。速さは PR ごとに実測で確かめる
- **問い合わせの準備の使い回し（`prepare_cached`）**：記事が 0 件でも 1 回約 1 ms（SQL の準備と推薦モデルの
  材料の読み出し）。一覧の 1 画面で数 ms で、1 の後の残りより小さい
- **`hidden_counts` の問い合わせを 1 つにまとめる**：1 の後は 1 回が数 ms になり、条件ごとに外した集合を
  1 つの SQL で数えると読みにくくなる
- **閲覧の制限（ビュー `artifact_access`）の書き換え**：外しても 47 ms → 43 ms しか変わらない（会員資格の
  ある本文がまだ無い）。会員資格の本文が増えて効いてきたら測り直す
- **パイプライン**：LLM と取得を除く各ステージの処理は数〜十数 ms で、取得は既にホストごとに並行している。
  時間はほぼ LLM の呼び出しと通信
