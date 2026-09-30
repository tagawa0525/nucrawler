# 同じ報道をまとめる（ストーリー）

## 背景

同じ出来事を複数のソースが報じると、一覧に同じ話が何件も並ぶ。手元の DB（2026-09-30 時点、1,531 件）で見ると、
言語やソースが違っても同じ出来事を報じている例が多い。

- 英語と日本語をまたぐ：WNN「欧州投資銀行、初のSMR向け融資を…Steady Energyに」（09-15）と JAIF「欧州投資銀行、フィンランドのSMR開発に初融資」（09-29）。
  公開日が 14 日ずれている
- 当事者の発表と報道：中部電力「浜岡3号機・4号機の…取り下げを提出」、JAIF「中部電力、浜岡3、4号機の申請取り下げを規制委に提出」、WNN の同じ話
- 国際機関と報道：IAEA「IAEAと米州開発銀行、…協定に署名」と WNN「米州開発銀行とIAEAが…協力覚書を締結」

「続報」もある。同じ案件で出来事が別のもの（例：WNN「イタリア上院、原子力復帰法案を可決」→ JAIF「イタリア、原子力発電再開に向けた法律が成立」）は、
一覧でまとめずに、詳細ページで関連記事として辿れるようにする。

今は重複を URL（`normalize_url`）でしか判定していない。題や内容の比較、埋め込み、グループの仕組みは無い。

## 方針

- **粒度**
  - 同じ出来事は「同じ報道（same）」とし、一覧で 1 件にまとめる
  - 同じ案件の別の出来事（続報・前段階・同じ案件の別の発表）は「関連（related）」とし、詳細ページにリンクを出すだけにする
- **判定**：文字の類似度で候補を絞り、LLM が same / related / 無関係 を判定する
  - 手元の DB で試算すると、日本語の見出しと要約（`title_ja` + `summary_ja`）の文字 bigram TF-IDF コサインは次の値になった
    - 本当に同じ報道の組は 0.18〜1.0（伊法案 0.21、IAEA-米州開銀 0.28、輸送白書 0.29）
    - 別件の組も 0.2〜0.4 に多く出る（各社の「定期検査を開始」「防災業務計画を修正」、IAEA の「ウクライナ情勢 第n報」の連番）
  - 閾値だけで決めるとまとめ間違いが多いので、LLM に確認させる
  - 候補が無い記事では LLM を呼ばない。試算では、別ソースどうしで ±21 日以内かつ 0.15 以上の組は全期間で 149 組しかない
- **記録**
  - LLM の出力は、ほかと同じく `artifacts` に残す。新しい種類 `story` で、backend・model・prompt_version を付ける
  - 判定した組は `story_links` に置く
  - 一覧で使うグループ（`article_stories`）は、same の辺の連結成分として作る派生データで、ステージの終わりに作り直す
- **3 件以上の同じ報道・複数の関連**
  - same も related も、1 件の記事に対して何件でも付けられる（応答は ID の配列）
  - 3 件以上のグループは、same の辺をつないでできる（A=B、C=B なら A・B・C が 1 つになる）
  - 候補は、記事ではなく「既存のグループ単位」で数え、LLM にも渡す
    - 候補の記事がグループに入っていれば、そのグループの全員（見出し・ソース・日付）をひとまとまりで見せ、
      same か related かをグループに対して判定させる
    - 理由 1：多くのソースが報じた話でも、候補の枠（別ソース 5）を同じグループの記事が埋めない
    - 理由 2：A と同じだが、A と同じグループの B とは違う、という食い違いが起きない
  - 辺を推移的につなぐと、別の出来事どうしが鎖状にまとまるおそれがある（IAEA 総会の一連の記事など）
    - グループ単位で判定させるので、新しい記事はグループの全員と比べてから加わる
    - 同じ組を両方の向きで判定していて、相手の最新の判定が same でなければ（related か無関係なら）つながない
      - 手元の DB のコピーで試すと、片方は same、もう片方は related や無関係と割れる組があった
        （美浜3号機の手動停止とその調査結果、電事連の会見と中間貯蔵の搬入計画など）
      - 判定が割れる組は迷いのある組なので、「迷ったらまとめない」に合わせる
      - そのため、候補にしたが same でも related でもない記事も、無関係（unrelated）の組として残す
    - 加えて、グループの大きさの上限（定数、8 件）を超える same は related に落として記録し、警告のログを出す
  - related は推移的につながない
    - 詳細ページでは、関連の相手をグループごとに 1 件へまとめる（代表と「他 n 件」）
    - グループ内の全員の related を合わせて出す
- **一覧**
  - グループは、`LIMIT` を掛ける前に 1 件へまとめる。代表は推薦点が最も高い記事
  - どれか 1 件が既読なら、グループ全体を既読として扱う
  - どれか 1 件の評価が ★1〜2 なら、グループ全体を隠す
  - これで、読み終えた話が 2 週間後に別ソースから来ても、一覧に戻ってこない
- **検索・MCP**：まとめない（検索は全件を見たい）。カードに同じ報道の件数だけ出す

## 変更

1. **マイグレーション `0029_stories.sql`**
   - `artifacts.kind` の CHECK に `story` を足す。0023 と同じくテーブルを作り直し、索引と全文検索のトリガーを戻す。story は検索の索引に入れない
   - `story_links (artifact_id REFERENCES artifacts ON DELETE CASCADE, other_id REFERENCES articles ON DELETE CASCADE, relation CHECK IN ('same','related','unrelated'), similarity REAL NOT NULL, PRIMARY KEY (artifact_id, other_id))`。similarity は `components` でつなぐ順に使う
   - `article_stories (article_id PRIMARY KEY REFERENCES articles ON DELETE CASCADE, story_id INTEGER NOT NULL)`。索引は `story_id`
   - `src/db/mod.rs` の `MIGRATIONS` に追加する
2. **`ArtifactKind::Story`**
   - payload は `{"candidates":[id…], "same":[id…], "related":[id…]}`
   - `inputs` は空にする（title と同じ）。payload は ID だけで、本文を含まない
3. **`src/story.rs`（新規。I/O なし）**
   - `normalize`：全角英数を半角にし、小文字にし、空白と記号を除く。依存は増やさない（NFKC は使わない）
   - `bigrams`：文字 bigram を数える
   - `Index::new(pool)`：プール内で IDF を計算する
   - `candidates(target, pool, stories) -> Vec<Candidate>`
     - `coalesce(published_at, fetched_at)` の差が ±21 日以内で、類似度 0.15 以上の記事を拾う
     - 拾った記事は、既存のグループごとにまとめる。グループの類似度は、そのグループの記事の最大値とする
     - グループまたは単独記事を単位に、別ソースから最大 5 単位、同じソースから最大 3 単位を選ぶ。同じソースの連番が候補を占めないようにするためで、値は定数で持つ
     - 対象の記事自身のグループは除く
   - `components(same_edges, max_size) -> Vec<(article_id, story_id)>`
     - union-find でまとめる。`story_id` は成分の最小の記事 ID
     - 辺を類似度の高い順につなぎ、つなぐと `max_size` を超える辺は捨てる（捨てた辺は呼び出し元が related 扱いにして警告のログを出す）
     - 2 件以上の成分だけを返す
4. **`Db`（新規 `src/db/stories.rs`）**
   - `pending_stories(now, backend, model, limit)`
     - 対象：`backlog_days` 内の記事で、次のどちらかを満たし、`story` の成果物が無いもの。新しい順
       - 要約がある
       - 公開の本文が無く、もう要約されない（`pending_titles` と同じ条件）。この場合は見出しの和訳か、日本語の原題があるもの
     - `stage_errors` と `work_claims` の除外は `pending_titles`（`src/db/artifacts.rs:155`）と同じにする
   - `story_pool(from, to)`：期間内の記事の `(id, source_id, at, text)`
     - text は「最新の閲覧できる要約の title_ja + summary_ja → 最新の title の title_ja → 日本語の原題」の順
   - `insert_story(NewArtifact, links)`：`insert_artifact` と `story_links` の挿入を、1 つのトランザクションで行う
   - `rebuild_stories()`：記事ごとに最新の story 成果物の same 辺を読み、`story::components` で `article_stories` を作り直す
     - 相手の最新の判定が同じ組を same 以外にしている辺は使わない
5. **`prompt::story`（新規。`prompt/title.rs` と同じ形）**
   - `PROMPT_VERSION`
   - `system_prompt`：same と related の定義と例を示す
     - same：同じ事実の報道。当事者の発表、それを報じる記事、翻訳を含む
     - related：同じ案件で出来事が別。続報・前段階・同じ案件の別の発表
     - 迷ったら無関係にする
     - 複数の出来事をまとめた記事（週報・特集など）は、その中の 1 つの出来事と same にせず related にする
   - `schema`：`{items:[{id, same:[候補ID], related:[候補ID]}]}`
     - 候補 ID は単独記事の ID か、グループの `story_id`
     - same も related も 0 件以上の配列にする
   - `build_prompt`
     - 対象記事ごとに `<article id source date>` で包み、その候補を並べる
     - グループの候補は `<story id>` の中に全員を並べる
     - `escape_data` を使う
     - 候補の中の複数と同じ報道になりうることを、system prompt で明示する
   - 保存するときは、グループ ID への same / related を、そのグループの全員への辺に展開して `story_links` に置く
   - `parse(output, requested)`
     - その対象の候補にない ID は無視する
     - same と related の両方に出る ID は拒否する
     - 欠けた対象は `missing` に入れる
6. **`pipeline::story`（新規。`pipeline/title.rs` の `translate_titles` と同じ流れ）**
   - 流れ：reserve → permit → `claim_selected(pending_stories)` → プールを読んで候補を計算 → 候補のある対象だけを 1 回の呼び出しで判定 → renew → 保存
   - 候補の無い対象は、LLM を呼ばずに空の story を保存する
   - バッチごとに `rebuild_stories` する
   - クォータ・中断・失敗（`record_failures`、`MISSING`）の扱いは title と同じ
7. **ステージ**
   - `Stage::Story`（名前 `story`、LLM のロック）
   - 順は `fetch → extract → digest → score → translate → title → story → tidy`
   - `src/pipeline/mod.rs`（enum、`ALL`、`name`、`lock`、`lock_groups` のテスト）と `src/pipeline/run.rs` の振り分け
   - `status` はソースごとの件数だけを出すので変えない
8. **設定**
   - `llm.story_backend`（省けば `llm.backend`。006 の工程ごとのバックエンドと同じ）
   - `llm.story_model`（既定 `sonnet`）
   - `llm.story_batch_size`（既定 10）。0 は `validate` で拒否する
   - `config::LlmTask::Story` を足し、`RunEnv::stage(LlmTask::Story)` で動かす
9. **一覧（`src/db/read.rs` の `query_items`）**
   - `article_stories` を結合し、`ListItem` に `story_others: Vec<String>`（同じグループの他の記事のソース ID。重複は除く）を足す。すべてのスコープで埋める
   - `ItemScope::List` だけ、グループを畳む
     - 非表示の条件のうち、既読以外（関係なし・低評価・`:min` 未満）を通った記事の中から、`BY_SCORE` の順で代表を 1 件選ぶ（`row_number() OVER (PARTITION BY coalesce(story_id, -id))`）
     - 既読と ★1〜2 はグループ単位で判定する
     - 畳むのは `LIMIT` の前
   - `ItemScope::Explore` は、一覧に出たグループの記事を除く（`src/web/server/pages.rs:177` の除外を story 単位にする）。確認枠の中でも 1 グループ 1 件にする
10. **表示（`src/web/html/`）**
    - `card()`（`list.rs:562`）：`story_others` があれば「他 2 件: WNN, IAEA」と出す。ソース名は `page.source()` を使う
    - 詳細（`detail.rs`）に 2 つの節を足す
      - 「同じ報道」：同じグループの他の記事。見出し・ソース・日付と詳細へのリンク
      - 「関連記事」：グループ内の全員の related 辺の相手から、同じグループの記事を除いたもの
        - 相手がグループに入っていれば、グループごとに 1 件（代表と「他 n 件」）へまとめる
        - 新しい順に並べる
    - `Db::story_members(article_id)` と `Db::related_articles(article_id)` を足す
11. **README と `examples/config.toml`**：ステージの説明と、`story_backend` / `story_model` / `story_batch_size`

`redo` の対象には入れない（今回は必要が無い）。MCP の出力も変えない。

## 作業の進め方

- worktree `../nucrawler-story-grouping`（ブランチ `feat/story-grouping`）で作業し、PR にする（本体は main のまま）
- 振る舞いの変更は TDD にする。RED（失敗するテスト）と GREEN（実装）を別のコミットにする
- 論理的な単位ごとにコミットを分ける。順は次のとおり
  1. マイグレーション
  2. `story.rs`
  3. `prompt::story`
  4. DB
  5. ステージ
  6. 一覧を畳む
  7. 表示
  8. ドキュメント
- 規模が大きいので、PR は 2 つに分けてもよい
  - PR1：判定と保存（1〜8、11 のうちステージと設定の部分）
  - PR2：一覧と表示（9、10）

## テスト

- **マイグレーション**：既存の成果物と検索の索引が残り、`story` を保存でき、記事を消すと links と stories も消える
- **`story.rs`**
  - 正規化で全角と半角が一致する
  - 類似した見出しが候補に入り、±21 日の外と閾値未満は入らない
  - 同じソースからは 3 件まで
  - 候補がグループ単位で数えられる
    - 6 件のグループがあっても、別ソースの枠を 1 単位しか使わない
    - 対象自身のグループは候補に入らない
  - `components`
    - 3 件以上が推移的にまとまり、`story_id` が最小の ID になる
    - `max_size` を超える辺は捨てられる
- **`pending_stories`**
  - 要約済みの記事が対象になり、次のものは対象外
    - 要約待ちの記事（本文があり要約がまだ）
    - story 済み
    - 再試行待ち
    - claim 済み
    - 期間外
  - 見出しだけの記事は、和訳か日本語の原題があれば対象になる
- **`prompt::story`**：スキーマ、閉じタグの無害化、応答の検証（候補外の ID・same と related の重複・欠けた対象）
- **ステージ**
  - 候補が無い記事は LLM を呼ばずに保存する
  - 候補のある記事は 1 回の呼び出しで判定し、links と stories が作られる
  - 1 件の記事が複数の候補と same になると、それらが 1 つのグループにまとまる
  - グループ ID への same が、全員への辺に展開される
  - 既存の 2 つのグループの両方と same なら、1 つにまとまる
  - 失敗を記録する。クォータで止まる
- **一覧**
  - 同じグループの 2 件が 1 件になり、推薦点の高い方が出る
  - 片方を既読にするとグループが消える
  - ★1 でグループが消える
  - `LIMIT` がグループ単位で効く
  - 確認枠に同じグループが出ない
  - 検索ではまとめない
- **Web**
  - カードに「他 n 件」が出る。3 件以上のグループでは、ソース名がすべて並ぶ
  - 詳細に「同じ報道」「関連記事」の節が出る。関連が複数のときは全件出し、同じグループの関連は 1 件にまとめる

## 確かめ方

1. `cargo test`、`cargo clippy`、`nix build`
2. 手元の DB のコピー（scratchpad）で `nucrawler crawl --only story` を実行する。次の組が same にまとまり、
   各社の定期検査や IAEA の連番がまとまらないことを、`article_stories` で確かめる
   - 背景に挙げた組（EIB、浜岡取り下げ、IAEA-米州開銀）
   - eVinci の臨界（WNN / Westinghouse / DOE-NE）
   - 昌江3号機
   - カメコ-GLE
3. 同じコピーで `nucrawler serve` を起動し、一覧で畳まれていることと、詳細に同じ報道・関連記事が出ることを確かめる
