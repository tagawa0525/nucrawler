# 導入と設定

## home-manager のモジュール

README の設定例で入る user unit：

| unit                       | 内容                                                         | 既定の時刻        |
| -------------------------- | ------------------------------------------------------------ | ----------------- |
| `nucrawler-crawl.timer`    | 取得から要約・採点（embedding）・和訳まで（`crawl`）         | 03:00 10:00 16:00 |
| `nucrawler-fetch.timer`    | 取得と本文抽出だけ（`crawl --until extract`）                | 22:00             |
| `nucrawler-requests.timer` | Web UI から依頼された和訳と見直し（`crawl --requests-only`） | 15 分ごと         |
| `nucrawler-serve.service`  | Web UI（`serve`）                                            | 常駐              |

- 時刻は `services.nucrawler.schedule` で変えられる（systemd の OnCalendar）。
  LLM をどれだけ使うかは時刻ではなく `[quota]` の時間帯ごとの上限で決まる。
  timer はマシンのタイムゾーンで動き、`[quota]` の時間帯は `timezone_offset_hours`（既定 JST）で判定する
- `services.nucrawler.embeddingServer.enable = true;` にすると、embedding のサーバー（text-embeddings-inference で
  `cl-nagoya/ruri-v3-310m` を動かす。Podman を使う）を `nucrawler-embedding.service` にし、`[embedding]` も
  それを呼ぶよう設定する。常駐はしない：使う unit が全部終わると止まる（計画 018）
  - 全体の `nucrawler-crawl` は、起動して待つ
  - `nucrawler-requests` は、プロファイルの見直しを頼まれているときだけ起動する
  - `eval --profile` や `crawl --only embed` など、手で使うときは `systemctl --user start nucrawler-embedding-hold`
    （応答するまで待つ）で起動し、終わったら `systemctl --user stop nucrawler-embedding-hold` で止める。
    `nucrawler-embedding` を単独で start しても、使う unit が無いのですぐ止まる
  - クラウドの API を使うなら、`embeddingServer.enable` を外し、`settings.embedding` に url・model・認証を書く
- unit は利用者のプロファイルの `claude` を使う。別の場所にあるなら
  `services.nucrawler.extraPackages = [ pkgs.claude-code ];` のように渡す
- 設定ファイルは `services.nucrawler.settings` と `sourcesFile` から生成される

### ログインしていなくても動かす（linger）

user unit はログインしている間しか動かない。常時動かすには、NixOS の設定で linger を有効にする。

```nix
users.users.<name>.linger = true;
```

## 初回の準備

1. `claude` にログインしておく（`claude` を一度起動する）。Copilot を使う工程があれば、`copilot login` でもログインしておく
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

4. Web UI にログインできるようにする。所有者の初期のログイン ID は `owner` でパスワードが無いので、
   ログイン ID を変えてパスワードを発行する（表示されたパスワードでログインする）

   ```sh
   nucrawler user rename owner you@example.com
   nucrawler user reset-password you@example.com
   ```

5. timer を待たずに一度動かす

   ```sh
   systemctl --user start nucrawler-crawl.service
   journalctl --user -u nucrawler-crawl -f
   ```

## 設定ファイル

設定は `$XDG_CONFIG_HOME/nucrawler/`、DB は `$XDG_DATA_HOME/nucrawler/nucrawler.db` に置く
（`--config-dir` と `--data-dir` で変えられる）。

- `config.toml`：HTTP、処理の範囲、LLM のモデルとバッチ、クォータ、Web UI。項目と既定値は [examples/config.toml](../examples/config.toml)
- `sources.toml`：巡回するソース。[examples/sources.toml](../examples/sources.toml)
- 関心プロファイル：[examples/profile.toml](../examples/profile.toml)（`profile import` で DB に取り込む）
- トピックの語彙：DB に初期値が入る（`topics export` / `topics import` で編集する）
- 訳語集：DB に初期値が入る（Web UI の ⚙️ → 訳語集で編集する）
