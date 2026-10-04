# CLI

```text
nucrawler crawl [--until STAGE | --only STAGE | --requests-only] [--max-llm-calls N] [--wait-lock]
nucrawler redo digest|translate --model M [--source ID] [--since YYYY-MM-DD] [--min-score N] [--ids 1,2,3] [--glossary] [--max-llm-calls N]
nucrawler status
nucrawler sources check [ID]
nucrawler serve [--addr IP:PORT]
nucrawler mcp
nucrawler profile import FILE | nucrawler profile export | nucrawler profile suggest --out FILE [--max-llm-calls N] | nucrawler profile history | nucrawler profile revert VERSION
nucrawler topics import FILE | nucrawler topics export
nucrawler search [--since D] [--until D] [--topic T]... [--source ID]... [--lang en|ja] [--translated] [--min-rating 1-5] [--read | --unread] [--bookmarked | --unbookmarked] [--unrated] [--min-score N] [--sort newest|score] [--limit N] [語]...
nucrawler eval [--all] [--profile FILE]
nucrawler embed rebuild
nucrawler user add LOGIN NAME | nucrawler user reset-password LOGIN | nucrawler user disable LOGIN | nucrawler user rename LOGIN NEW_LOGIN | nucrawler user list
```

設定や DB の場所は `--config-dir DIR` と `--data-dir DIR` で変えられる。

## crawl

ステージを順に進める。中断と再開、並行、ロック（`--wait-lock`）は [処理の流れ](pipeline.md) を参照。

## redo

指定したモデル・プロンプト版の成果物がまだ無い記事を作り直す。同じコマンドを再実行すれば続きから処理する。

`--glossary` を付けると、そのモデルの最新の版が、記事に当たる訳語の変更（訳・略語・メモの変更と、
記事に出てくる原語の追加）より前に作られた記事だけを、同じモデルで新しい版として作り直す
（例：訳語集を直した後に `redo translate --model sonnet --glossary`）。当たる訳語は、LLM に渡すのと同じ
切り詰めた本文で判定する。

## status

ソースごとの記事数と取得状況、ステージ（とバックエンド・モデル）ごとの失敗の記録が残っている記事の数を表示する。
再試行と諦めた記事の扱いは [処理の流れ](pipeline.md#失敗と再試行)。

## eval

採点が利用者の評価とどれだけ合っているかを表示する。記事に付けた評価（★1〜5）を正解とし
（ブックマーク・既読の印・開いただけの記事は使わない）、採点のキー（プロファイル・モデル・式の版）ごとに次を出す。

- 採点済みの割合
- 一致率：評価の違う記事の組のうち、評価の高い方が高い点になっている割合。同点は半分と数え、
  0.5 なら当て推量と同じ。評価が 2 通りだけなら AUC と同じ
- 点数帯ごとの評価別の件数
- `adjusted`：推薦点（[処理の流れ](pipeline.md#推薦点)）の一致率。学習と評価に同じ評価を使うと良く出すぎるので、
  評価を 1 件ずつ外して学習し、外した 1 件を予測して測る（leave-one-out）
- 確認枠（[Web UI](web-ui.md#確認枠)）に出した記事の評価の内訳。評価した記事のうち関心（★4〜5）の割合を、
  閾値未満での見逃し率の見積もりとして示す

採点は embedding の点数（`embedding/<モデル>`。`embed` ステージが採点した時点の基準で保存したもの）で、既定は今の
プロファイルと式の版のキーだけを並べる。`--all` で過去のキーと、計画 017 の前に保存した LLM の点数（`claude-cli/<モデル>`）も並べる。

`[embedding]` があれば、さらに式の候補（`embedding-trial/<モデル> now`・`λ=0.5`・`λ=0`・`mean`・`top3`）を、保存せずに今の時点・
同じ基準でその場で計算して `(trial)` として並べる。式どうしは `now` と比べる（保存した点数とは基準の時点が違う）。

### eval --profile FILE

候補のプロファイル（`profile import` と同じ形式）で、評価した記事を embedding で今の式でその場で採点し、今のプロファイルと
並べて表示する（`[embedding]` が要る）。候補は取り込まず、点数も保存しない。LLM は呼ばない
（候補の好みの文のベクトルは作って残すが、次の `embed` で使われなければ消える）。

## profile suggest

`profile suggest --out FILE` は、記事に付けた評価（★1〜5）を根拠に、LLM に関心プロファイルの更新案を作らせる。
今のプロファイルとの差分（Rust で計算したもの）と、変更ごとの根拠を表示し、案を FILE に書く（既にあるファイルは
上書きしない）。案は取り込まないので、`eval --profile FILE` で今のプロファイルと比べてから `profile import FILE` する。
評価が無いことは関心が無い根拠にしない（表示されなかった記事には評価できないため）。LLM の呼び出しは 1 回。

## profile history・profile revert

プロファイルは保存するたびに版として残る（取り込み・案の採用・前の版に戻すなど）。`profile history` は版を新しい順に
並べ、版ごとに、出どころ・その版が今のプロファイルだった間に付けた評価での一致率（一覧と同じ規則で選んだ点数で測る。
案の根拠にした記事は除く）・1 つ前の版からの変更を出す。一致率は版が今でなくなった時点の値で固まる。

`profile revert VERSION` は、その版の中身を新しい版として保存し、今のプロファイルにする。点数は次の crawl で、
戻した版で全記事を採点し直す（embedding なので数秒）。

## search

Web の検索画面（[Web UI](web-ui.md#検索)）と同じ条件で記事を探し、1 行 1 件（公開日時・点数・見出し・URL）で出す。
閲覧としては記録しない。

## topics

要約に付けるトピックの語彙を扱う。初期の語彙は DB を作るときに入るので、
手で変えるときは `topics export > topics.toml` で書き出して編集し、`topics import topics.toml` で取り込む。

- 軸（facet）は `分野`・`炉型`・`地域`・`組織` のいずれか。発電所名などの固有名は語彙に入れず全文検索で探す
- 要約に付いている語は削除できない（取り込みがエラーになり、語彙は変わらない）
- LLM が足した語は `tidy` ステージで既存の語へ統合される（統合元は LLM が足した語だけで、軸の違う語へは統合しない）。
  すぐ整理するなら `crawl --only tidy`
- 書き出した語彙では LLM が足した語に `added_at` が付く。その行を消して取り込めば、人が決めた語になり統合されなくなる
- 統合した語は別名として残り（`topic_aliases` に統合した時刻と LLM を記録）、LLM が同じ名前を付けても統合先に付く。
  誤った統合は、`topics export` した語彙に統合元を足して `topics import` すれば語に戻る

## user

Web UI の利用者（ログイン ID はメールアドレスなど 254 バイト以下）を管理する。管理者は所有者で、
CLI を使えるのは稼働ホストに入れる人だけなので、CLI の操作が管理者の確認を兼ねる。

- パスワードは引数で受け取らず（シェルの履歴に残さないため）、`add` と `reset-password` が紛らわしい文字を除いた
  英数字 20 文字を作って 1 回だけ表示する。保存するのは argon2id のハッシュだけ
- `reset-password` と `disable` は、パスワード・ログイン中のセッション・フィードの URL・ログインの失敗の記録を
  まとめて失効させる（`disable` はパスワードも無くし、戻すときは `reset-password`）
- 所有者の初期のログイン ID は `owner` なので、`rename owner you@example.com` と `reset-password` で使えるようにする
- 利用者の削除は無い（評価や既読の記録ごと消えるため）
