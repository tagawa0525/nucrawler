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
nucrawler profile import FILE | nucrawler profile export
```

- `crawl` は途中で Ctrl-C（または SIGTERM）で止めても、次回は続きから処理する。2 回目のシグナルで即座に終了する
- 別の `crawl` が実行中なら終了コード 75（EX_TEMPFAIL）で終わる。`--wait-lock` を付けると終わるのを待ってから始める（unit はこちらを使う）
- `status` はソースごとの記事数と取得状況を表示する

## Web UI

- 一覧は「前回の訪問の後に届いた記事」と「それより前の未読」に分かれ、点数の高い順に並ぶ。
  👎・閾値（`web.min_score`）未満・未採点・軽水炉と無関係の記事は「すべて表示」でだけ出る
- 詳細では要約の版を切り替えられる。英語の記事は全文和訳を読むか、まだ無ければ依頼できる
- 詳細や和訳を開いたこと、👍/👎 は、次回からの採点に反映される
- 取得に失敗しているソースや、LLM の失敗（認証切れなど）は画面の上部に出る
- 認証は無いので、Tailscale など信頼できるネットワークのアドレスで待ち受ける

## 設定

設定は `$XDG_CONFIG_HOME/nucrawler/`、DB は `$XDG_DATA_HOME/nucrawler/nucrawler.db` に置く
（`--config-dir` と `--data-dir` で変えられる）。

- `config.toml`：HTTP、処理の範囲、LLM のモデルとバッチ、クォータ、Web UI。項目と既定値は [examples/config.toml](examples/config.toml)
- `sources.toml`：巡回するソース。[examples/sources.toml](examples/sources.toml)
- 関心プロファイル：[examples/profile.toml](examples/profile.toml)（`profile import` で DB に取り込む）

home-manager のモジュールを使うときは `services.nucrawler.settings` と `sourcesFile` から生成される。

## 開発

```sh
nix develop   # rustup と sqlite
cargo test
nix build     # パッケージ（テストも実行する）
```

設計と今後の予定は [docs/plans/soft-squishing-fern.md](docs/plans/soft-squishing-fern.md) にある。
