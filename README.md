# nucrawler

原子力（軽水炉）関係のサイトを巡回し、記事を日本語で要約・和訳して、関心に合わせて推薦する。
スマホから Tailscale 経由で読むための Web UI を持つ。

- 要約・採点・和訳は Claude Code の headless 実行（`claude -p`）で行い、サブスクリプションの枠内に収める
  （工程ごとに GitHub Copilot CLI も選べる）。枠の使用率を見て、時間帯ごとの上限を超えないように止まる
- 処理はステージ（取得 → 本文抽出 → 要約 → embedding → 採点 → 和訳 → 見出しの和訳 → 同じ報道の判定）ごとに進み、
  中断しても次回は続きから再開する
- 同じ出来事を別のソースが報じた記事は、1 つにまとめて出す
- 推薦は、関心プロファイルとの embedding の近さの点数に、記事に付けた評価（★1〜5）から学んだ補正を足した点数で並べる

## 導入（NixOS + home-manager）

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

巡回の timer と Web UI の service が入る。初回は `claude` へのログイン、関心プロファイルの取り込み
（`nucrawler profile import`）、Web UI の利用者の発行（`nucrawler user`）が要る。手順は [docs/setup.md](docs/setup.md)。

## ドキュメント

- [導入と設定](docs/setup.md)：unit と時刻、linger、初回の準備、設定ファイル
- [処理の流れ](docs/pipeline.md)：ステージ、LLM とクォータ、並行とロック、同じ報道のまとめ方、推薦点
- [CLI](docs/cli.md)：`crawl`・`redo`・`eval`・`profile`・`topics`・`user` など
- [Web UI](docs/web-ui.md)：一覧と絞り込み、評価、検索、管理者の画面、フィードと JSON
- [MCP](docs/mcp.md)：Claude Code などから記事を検索・参照する
- [計画](docs/plans/)：設計の判断の記録。全体の設計は [soft-squishing-fern.md](docs/plans/soft-squishing-fern.md)

## 開発

```sh
nix develop   # rustup と sqlite
cargo test
nix build     # パッケージ（テストも実行する）
```
