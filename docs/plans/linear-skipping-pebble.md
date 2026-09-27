# 記事の全文検索

## Context

記事が増えると（手元の DB で 1,278 件）、一覧は「直近 N 日・点数順」なので過去の記事や
隠された記事を探せない。題名・要約・本文・和訳を語で引き、発行日の期間・トピック・ソース・
言語・状態（和訳あり・👍・未読・点数）と組み合わせて絞れるようにする。
入口は Web UI、JSON API、CLI の 3 つ。今後は論文の全文が増える見込み。

## 方式：SQLite FTS5 + trigram トークナイザ

- 同梱の SQLite（libsqlite3-sys 0.38.2 = 3.53.2）は `SQLITE_ENABLE_FTS5` 付きでビルドされ、
  trigram トークナイザ（3.34+）が使える。**依存の追加なし**
- trigram は形態素解析なしで日本語・英語とも部分一致で引ける。辞書（lindera 等）が不要
- tantivy + lindera や Meilisearch は、DB と別の索引の同期・辞書・常駐プロセスが増えるので採らない
- 弱点：trigram の MATCH は 3 文字未満の語（「炉心」「規制」）を引けない。
  3 文字以上の語は `MATCH '"語"'`（フレーズ、`"` は二重化）、3 文字未満は索引の本文への
  `instr(lower(text), lower(?)) > 0` にする（走査。論文の扱いは「規模の見通し」）
- 空白区切りの複数語は AND

## 索引：migration `0006_search.sql`

```sql
CREATE VIRTUAL TABLE search_docs USING fts5(
    article_id UNINDEXED,
    content_id UNINDEXED,   -- contents 由来なら id、それ以外は NULL
    artifact_id UNINDEXED,  -- artifacts 由来なら id、それ以外は NULL
    text,
    tokenize = 'trigram'
);
```

- 1 行 = 1 つの文書（記事の原題 / 本文の部分 1 つ / 要約の版 1 つ / 和訳の版 1 つ）。
  権限の判定を元の行に委ねるため、由来の id を持たせる
- **同期はトリガで行う**（アプリ側で書き忘れる余地をなくす）。3 テーブルとも UPDATE は
  コードに無く追記のみなので、INSERT と DELETE のトリガだけでよい。
  外部キーの CASCADE による削除でもトリガは発火する
  - `articles`：AFTER INSERT で `title`、AFTER DELETE で `article_id` の行を削除
  - `contents`：AFTER INSERT で `text`、AFTER DELETE で `content_id` の行を削除
  - `artifacts`：kind が `digest` なら `title_ja || char(10) || summary_ja`、
    `translation` なら `json_extract(payload, '$.body_ja')`。`judgment` は入れない
- 同じ migration で既存行を `INSERT ... SELECT` で投入する

## 閲覧権限（会員限定の本文を漏らさない）

一致した行を元の行に戻して判定し、利用者が読めるものだけ記事の一致に数える。

- contents 由来：`access_membership_id IS NULL OR IN (利用者の資格)`
- artifacts 由来：既存の `viewable("r")`（`src/db/mod.rs:387`）をそのまま使う
- 原題由来：常に可

## 問い合わせ：複合検索 `src/db/mod.rs`

全文の語は「一致した記事 id に絞る」条件の 1 つにすぎず、他の条件と AND で重ねる。

```rust
pub struct SearchQuery {
    pub terms: Vec<String>,          // 全文の語（AND）。空なら全文条件なし
    pub from: Option<NaiveDate>,     // 発行日（published_at、無ければ fetched_at）の下限・上限。JST の日で解釈
    pub to: Option<NaiveDate>,
    pub topics: Vec<String>,         // 最新の閲覧可能な digest の topics に完全一致（AND）
    pub sources: Vec<String>,        // source_id（OR）
    pub lang: Option<Lang>,          // en / ja
    pub translated: bool,            // 和訳あり
    pub liked: bool,                 // 👍 した
    pub unread: bool,                // 未読
    pub min_score: Option<u8>,
    pub limit: usize,
}
```

- `ItemScope` に `Search(SearchQuery)` を追加し、`query_items`（`src/db/mod.rs:1376`）の
  `items` / 最終 `WHERE` に、指定された条件の分だけ SQL 片を足す。値はすべて bind する。
  状態系（和訳・👍・未読・点数）は `rows` に既にある列をそのまま条件にする
- topics は後述の `artifact_topics` で判定する（`SearchQuery.topics` は語彙の id）
- 検索時の既定：隠す条件（👎・非軽水炉・閾値未満）を適用しない、**新しい順**、件数は `web.list_limit`
- 公開 API は `Db::search_articles(user_id, profile_hash, &SearchQuery)`。
  語の分割（空白・全角空白）は入口側の共通関数 `search::parse_terms` に置く

## トピックの語彙を DB で管理する

現状は LLM が `topics` を自由記述で付けており（`src/digest.rs:117` は例示のみ）、要約 122 件で
約 200 種、`新設`/`新設炉`/`新設・建設`/`新規建設`、`浜岡`/`浜岡原子力発電所` のように揺れる。
分野・炉型・国・組織・固有名も混在している。後から名寄せしても再発するので、**語彙を表で持ち、
LLM には語彙の中から選ばせる**。

- 語彙の正本は `examples/topics.toml`（`[[topic]] name = "規制・審査", facet = "分野"`）。
  facet は `分野` `炉型` `地域` `組織` に限る。発電所名などの固有名は語彙に入れず全文検索で引く
- `nucrawler topics import <file>` で DB に取り込む（`profile import` と同じ流儀）
- 表（migration `0007_topics.sql`）：

  ```sql
  CREATE TABLE topics (
      id    INTEGER PRIMARY KEY,
      name  TEXT NOT NULL UNIQUE,
      facet TEXT NOT NULL CHECK (facet IN ('分野', '炉型', '地域', '組織'))
  );
  -- 要約の版ごとの付与。版の閲覧権限は artifacts 側で判定できる。
  CREATE TABLE artifact_topics (
      artifact_id INTEGER NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
      topic_id    INTEGER NOT NULL REFERENCES topics (id) ON DELETE RESTRICT,
      PRIMARY KEY (artifact_id, topic_id)
  ) WITHOUT ROWID;
  CREATE INDEX artifact_topics_by_topic ON artifact_topics (topic_id);
  ```

  使われている語の削除は RESTRICT で止める。改名は `name` の更新だけで済む
- 要約のプロンプトに語彙を facet ごとに列挙し、出力スキーマの `topics` を語彙の `enum` にする
  （`src/digest.rs` のスキーマ組み立て）。`prompt_version` を上げる。
  検証で語彙外が来たら失敗として扱う（黙って捨てない）
- `write_artifact`（`src/db/mod.rs:1642`）で digest を書くとき、同じトランザクションで
  `artifact_topics` に入れる
- 既存の要約（手元で 122 件）は `redo digest` で新しいプロンプトから作り直す。
  別名の対応表は作らない（旧版は検索のトピック条件に出ないだけで、残る）
- 検索のトピック条件は、最新の閲覧可能な digest の `artifact_topics` で判定する。
  Web の選択肢は `Db::topics()` が facet ごと・使用回数順に返す
- 関心プロファイルの `interest.topic`（`src/profile.rs`）も同じ語彙に揃えると採点と検索の
  言葉が一致するが、今回は範囲外（必要になったら別 PR）

## 規模の見通し（論文の全文が増えたとき）

- trigram の MATCH は転置索引の積なので、本文が数百 MB〜1GB 程度でも数十 ms 級で返る見込み。
  索引の大きさは本文の 3〜4 倍程度（1GB の本文で数 GB）。いずれも見積もりなので下記の計測で確かめる
- 弱いのは 2 点：
  1. **2 文字語の走査**：本文量に比例して遅くなる（数百 MB で秒単位）。
     → 対策：3 文字未満の語は PDF 由来（`origin = 'pdf'`）と `fulltext` の本文を走査しない。
       論文本文は 3 文字以上の語で引く（PDF の本文は現状 `kind = 'body', origin = 'pdf'` で入る）
  2. **関連度順**：trigram の bm25 は語の区切りを知らず質が低い。今回は新しい順なので影響なし
- 検索は `search_articles` と `search_docs` の背後に閉じ込めるので、関連度順や形態素解析が
  必要になったら tantivy + lindera に差し替えても入口と条件の組み立ては変わらない
- PR 1 の検証で、合成データ（論文相当 50KB × 1 万件 ≒ 500MB）を入れて
  3 文字語・2 文字語・複合条件の応答時間と DB サイズを計測し、PR 本文に数字を残す

## 入口

3 つの入口で同じ名前のパラメータを使う：`q` `from` `to`（`YYYY-MM` または `YYYY-MM-DD`）
`topic`（複数可）`source`（複数可）`lang` `translated=1` `liked=1` `unread=1` `min_score`。
クエリ文字列 → `SearchQuery` の変換は共通関数にして、Web・API・CLI から使う。

- Web：`/search?...` を新設。上部に `<form method="get">`（語・期間・トピックとソースの選択・
  チェックボックス）、結果は `html` の一覧の行の描画を流用して 1 区分で出す。
  `begin_visit` は呼ばない（訪問ではない）。一覧ページから検索へのリンクを置く
- API：`/api/search?...` で同じ結果を `api::ArticleList` で返す
- CLI：`src/cli.rs` に `Command::Search`（`nucrawler search [--from ..] [--topic ..] ... <語>...`）。
  1 行 1 件で「日付・点数・title_ja（無ければ原題）・URL」を出す

## PR の分け方（TDD：RED → GREEN をコミットに残す）

1. 索引と全文の語の検索（migration 0006、`SearchQuery` の `terms` と `limit` だけ）＋合成データでの計測
   - テスト：日本語 2 文字語・3 文字以上の語・英語の大小文字・複数語 AND・
     会員限定の本文にだけ出る語は資格の無い利用者に出ない・要約の版が閲覧不可なら一致しない・
     記事削除で索引から消える・migration 前の既存行が引ける・2 文字語は PDF の本文に当たらない
2. トピックの語彙（migration 0007、`topics import`、プロンプトとスキーマの enum、`artifact_topics` への書き込み）
   - テスト：語彙外のトピックは検証で失敗・digest の保存で付与が入る・版の削除で付与が消える・
     使用中の語彙は削除できない・import の重複や不正な facet は拒否
   - マージ後、`redo digest` で既存の要約を作り直す（運用作業）
3. 絞り込み条件（期間・トピック・ソース・言語・状態）と `Db::topics()`
   - テスト：各条件単独と、全文の語との組み合わせ。期間の境界（JST の月末）・
     閲覧できない版のトピックでは一致しない
4. Web と JSON API（`/search`、`/api/search`）
5. CLI の `search`

## 検証

- `cargo test`（各 PR のテストは `src/db/mod.rs` と `src/web/server.rs` の既存のテストの流儀に合わせる）
- 手元 DB のコピーで migration を当て、`nucrawler search 炉心` / `search 再稼働` / `search NRC` を実行して目視
- `serve` を起動し、スマホ幅で `/search?q=...` の表示と、`/api/search?q=...` の JSON を確認
