# nucrawler

原子力（軽水炉）関係のサイトを巡回し、記事を日本語で要約・和訳して、関心に合わせて推薦する。
スマホから Tailscale 経由で読むための Web UI を持つ。

- 要約・採点・和訳は Claude Code の headless 実行（`claude -p`）で行い、サブスクリプションの枠内に収める
- 5 時間枠と週次枠の使用率を見て、時間帯ごとの上限を超えないように止まる（`config.toml` の `[quota]`）
- 処理はステージ（取得 → 本文抽出 → 要約 → 採点 → 和訳）ごとに成果物の有無で進み、中断しても次回は続きから再開する
- 要約と和訳はモデルごとに版を残し、`redo` で別のモデルでやり直せる
- 推薦は関心プロファイル（分野と重み）と、閲覧・👍/👎 の行動をもとに LLM が採点する

## 導入（NixOS + home-manager）

flake の入力に加え、home-manager のモジュールを読み込む。

```nix
# flake.nix
inputs.nucrawler.url = "github:tagawa0525/nucrawler";

# home-manager の設定
imports = [ inputs.nucrawler.homeManagerModules.default ];

services.nucrawler = {
  enable = true;
  settings = {
    # スマホから Tailscale 経由で開くなら tailnet の IP で待ち受ける
    web.bind = "100.x.y.z:8080";
  };
  # sourcesFile = ./sources.toml;  # 既定は examples/sources.toml
};
```

これで次の user unit が入る。

| unit                       | 内容                                                 | 既定の時刻        |
| -------------------------- | ---------------------------------------------------- | ----------------- |
| `nucrawler-crawl.timer`    | 取得から要約・採点・和訳まで（`crawl`）              | 03:00 10:00 16:00 |
| `nucrawler-fetch.timer`    | 取得と本文抽出だけ（`crawl --until extract`）        | 22:00             |
| `nucrawler-requests.timer` | Web UI から依頼された和訳（`crawl --requests-only`） | 15 分ごと         |
| `nucrawler-serve.service`  | Web UI（`serve`）                                    | 常駐              |

時刻は `services.nucrawler.schedule` で変えられる（systemd の OnCalendar）。
LLM をどれだけ使うかは時刻ではなく `[quota]` の時間帯ごとの上限で決まる。
timer はマシンのタイムゾーンで動き、`[quota]` の時間帯は `timezone_offset_hours`（既定 JST）で判定する。

### ログインしていなくても動かす（linger）

user unit はログインしている間しか動かない。常時動かすには、NixOS の設定で linger を有効にする。

```nix
users.users.<name>.linger = true;
```

### 初回の準備

1. `claude` にログインしておく（`claude` を一度起動する）。unit は利用者のプロファイルの `claude` を使う。
   別の場所にあるなら `services.nucrawler.extraPackages = [ pkgs.claude-code ];` のように渡す
2. 関心プロファイルを用意して取り込む。パッケージに入っている例をコピーして編集するとよい

   ```sh
   cp "$(dirname "$(readlink -f "$(command -v nucrawler)")")/../share/nucrawler/profile.toml" ~/nucrawler-profile.toml
   $EDITOR ~/nucrawler-profile.toml
   nucrawler profile import ~/nucrawler-profile.toml
   ```

3. ソースから取得できるか確かめる

   ```sh
   nucrawler sources check
   ```

4. timer を待たずに一度動かす

   ```sh
   systemctl --user start nucrawler-crawl.service
   journalctl --user -u nucrawler-crawl -f
   ```

## 使い方

```text
nucrawler crawl [--until STAGE | --only STAGE | --requests-only] [--max-llm-calls N] [--wait-lock]
nucrawler redo digest|translate --model M [--source ID] [--since YYYY-MM-DD] [--min-score N] [--ids 1,2,3]
nucrawler status
nucrawler sources check [ID]
nucrawler serve [--addr IP:PORT]
nucrawler mcp
nucrawler profile import FILE | nucrawler profile export
nucrawler topics import FILE | nucrawler topics export
nucrawler search [--since D] [--until D] [--topic T]... [--source ID]... [--lang en|ja] [--translated] [--liked] [--unread] [--bookmarked] [--min-score N] [--sort newest|score] [--limit N] [語]...
```

- `crawl` は途中で Ctrl-C（または SIGTERM）で止めても、次回は続きから処理する。2 回目のシグナルで即座に終了する
- 別の `crawl` が実行中なら終了コード 75（EX_TEMPFAIL）で終わる。`--wait-lock` を付けると終わるのを待ってから始める（unit はこちらを使う）
- `status` はソースごとの記事数と取得状況を表示する
- `search` は Web の検索画面（下記）と同じ条件で記事を探し、1 行 1 件（公開日時・点数・見出し・URL）で出す。
  閲覧としては記録しない
- `topics` は要約に付けるトピックの語彙を扱う。要約では語彙から選び、当てはまる語が無いときだけ
  LLM が新しい語を 1 個まで提案して語彙に加える。初期の語彙は DB を作るときに入るので、
  手で変えるときは `topics export > topics.toml` で書き出して編集し、`topics import topics.toml` で取り込む。
  軸（facet）は `分野`・`炉型`・`地域`・`組織` のいずれか。発電所名などの固有名は語彙に入れず全文検索で探す。
  要約に付いている語は削除できない（取り込みがエラーになり、語彙は変わらない）。
  LLM が足した語の表記揺れは、crawl の最後の `tidy` ステージで週 1 回（`llm.tidy_interval_days`）LLM に見直させ、
  既存の語へ自動で統合する（統合元は LLM が足した語だけで、軸の違う語へは統合しない）。すぐ整理するなら `crawl --only tidy`。
  書き出した語彙では LLM が足した語に `added_at` が付く。その行を消して取り込めば、人が決めた語になり統合されなくなる。
  統合した語は別名として残り（`topic_aliases` に統合した時刻と LLM を記録）、LLM が同じ名前を付けても統合先に付く。
  誤った統合は、`topics export` した語彙に統合元を足して `topics import` すれば語に戻る
- 訳語集は要約と和訳で訳語と略語を揃えるための一覧で、DB が正本（初期値は DB を作るときに入る）。
  1 つの訳語（略語を添えられる）に原語を複数結び付け、表記の揺れや略語をまとめて同じ訳にする。
  LLM には、渡す記事（切り詰めた後の見出しと本文）に原語が出てくる語だけを載せる。
  原語は語の単位で当て（複数形の s / es は当てる）、大文字だけの略語は大文字のときだけ、ほかは大文字小文字を問わない

## Web UI

- 一覧の上部は絵文字のボタンだけを並べる。🔍 は検索、👍 は 👍 した記事（`/search?liked=1`）、🔖 はブックマークへ。
  切り替え（⭐・👁）は ON が緑、OFF が赤
- 記事のカードでは、ソース・日付の横に 👍 した記事は 👍、ブックマークした記事は 🔖 が付く
- 一覧は「前回の訪問の後に届いた記事」と「それより前の未読」に分かれ、点数の高い順に並ぶ。
  👎・見ない・閾値（`web.min_score`）未満・未採点・軽水炉と無関係の記事は⭐（おすすめだけ表示）を OFF にしたとき（`?all=1`）だけ出る。
  前回より前の既読の記事は👁（過去の既読も表示、`?read=1`）でだけ出る（前回からの欄は既読も出す）
- 一覧の記事は、詳細を開かずに左右のスワイプで振り分けられる。右でブックマーク、左で見ない。
  キーボードでは j / k（↓ / ↑）で記事を選び、l（→）でブックマーク、h（←）で見ない、u で取り消す。
  振り分けた記事は一覧から外れ、しばらく「元に戻す」が出る（戻すと振り分けは無かったことになる）。
  ブックマークした記事は一覧上部の 🔖（`/search?bookmarked=1`）で読め、詳細の 🔖 で外せる
- 詳細では要約の版を切り替えられる。英語の記事は全文和訳を読むか、まだ無ければ依頼できる
- 詳細の末尾の「訳語の指摘」を開くと、気になった訳を送れる（希望する訳・原語・メモは任意）。
  送った指摘は受付箱（`term_reports`）に溜まり、あとで訳語集に反映する
- 詳細や和訳を開いたこと、👍/👎、ブックマーク、見ないは、次回からの採点に反映される。
  ブックマークと見ないは 👍/👎 より弱い反応として扱う（ブックマークを外しても、した反応は残る）
- 取得に失敗しているソースや、LLM の失敗（認証切れなど）は画面の上部に出る
- 検索（`/search`、一覧上部の 🔍）では、一覧で隠す記事や期間外の記事も探せる。条件はすべて AND で、
  既定は新しい順（点数順も選べる）。件数の上限は `web.list_limit`
  - `q`：原題・本文・要約・和訳に含む語（全文検索）。空白で区切るとすべてを含む記事に絞る。
    3 文字以上の語は索引で、2 文字以下の語は全文を順に調べて探す
  - `since` / `until`：日本時間の年・年月・年月日（`2026` / `2026-09` / `2026-09-20`、`until` はその日・月・年を含む）。
    画面では文字で入れるほか、横の 📅 のカレンダーで日付を選べる
  - `topic`：最新の要約に付いているトピック（繰り返すとすべてが付いている記事）。統合した語の別名でもよい
  - `source`：ソース（繰り返すとどれかのソース）
  - `lang`（`en` / `ja`）、`translated=1`（和訳あり）、`liked=1`（👍）、`unread=1`（未読）、`bookmarked=1`（ブックマーク中）、`min_score`（最低点。未採点は除く）
  - `sort`：`newest`（既定）か `score`
- 認証は無いので、Tailscale など信頼できるネットワークのアドレスで待ち受ける
- 同じサーバーで、既定の一覧と同じ記事をフィードと JSON でも出す。
  これらを読んでも、開いたことや訪問としては記録しない
  - `/feed.xml`：Atom フィード（新しい順。和訳タイトル・要約・詳細ページと原文へのリンク）。
    エントリの ID は元記事の URL から作り、アクセスしたアドレスによらないので、
    別の名前（Tailscale の IP と MagicDNS 名など）で購読しても既読の記事は新着にならない
  - `/api/articles`：記事の一覧（`?all=1` で「すべて表示」と同じ記事）
  - `/api/articles/{id}`：記事 1 件（最新の要約と、あれば最新の全文和訳）
  - `/api/search`：検索画面と同じ条件の検索（一覧と同じ形）。条件の誤りは 400

## MCP

`nucrawler mcp` は MCP の stdio サーバーで、Claude Code などから記事を検索・参照できる。
stdio で起動できるのはこのマシンの利用者だけなので、オーナーとして閲覧判定する。
ツールは読み取り専用で、LLM を呼んだり DB に書いたり（和訳の依頼、👍/👎、既読の記録）はしない。

- `search_articles`：記事の検索。引数はどれも省略できる
  - `keyword`：原題・本文・要約・和訳に含む語（全文検索）。空白で区切ると、すべてを含む記事に絞る
  - `since` / `until`：日本時間の日付・月・年（`YYYY-MM-DD` / `YYYY-MM` / `YYYY`、`until` はその日・月・年を含む）。既定は Web UI と同じ直近 `web.list_days` 日
  - `source`：ソースの ID
  - `min_score`：最低点（既定は `web.min_score`）
  - `include_hidden`：Web UI の「すべて表示」と同じく、👎・見ない・閾値未満・未採点・軽水炉と無関係の記事も含める
  - `limit`：最大件数（既定は `web.list_limit`）
- `get_article`：記事 1 件（`id`）の元記事の URL、最新の要約、全文和訳があればその本文

Claude Code に登録する例（ユーザー全体で使う）：

```sh
claude mcp add --scope user nucrawler -- nucrawler mcp
```

プロジェクトの `.mcp.json` に書く場合：

```json
{
  "mcpServers": {
    "nucrawler": {
      "command": "nucrawler",
      "args": ["mcp"]
    }
  }
}
```

設定や DB の場所を変えているときは、`args` の `mcp` の前に `--config-dir DIR` や `--data-dir DIR` を置く。

## 設定

設定は `$XDG_CONFIG_HOME/nucrawler/`、DB は `$XDG_DATA_HOME/nucrawler/nucrawler.db` に置く
（`--config-dir` と `--data-dir` で変えられる）。

- `config.toml`：HTTP、処理の範囲、LLM のモデルとバッチ、クォータ、Web UI。項目と既定値は [examples/config.toml](examples/config.toml)
- `sources.toml`：巡回するソース。[examples/sources.toml](examples/sources.toml)
- 関心プロファイル：[examples/profile.toml](examples/profile.toml)（`profile import` で DB に取り込む）
- トピックの語彙：DB に初期値が入る（`topics export` / `topics import` で編集する）
- 訳語集：DB に初期値が入る（`glossary_terms` と `glossary_sources`）

home-manager のモジュールを使うときは `services.nucrawler.settings` と `sourcesFile` から生成される。

## 開発

```sh
nix develop   # rustup と sqlite
cargo test
nix build     # パッケージ（テストも実行する）
```

設計と今後の予定は [docs/plans/soft-squishing-fern.md](docs/plans/soft-squishing-fern.md) にある。
