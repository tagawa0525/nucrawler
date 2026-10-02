# MCP

`nucrawler mcp` は MCP の stdio サーバーで、Claude Code などから記事を検索・参照できる。
stdio で起動できるのはこのマシンの利用者だけなので、オーナーとして閲覧判定する。
ツールは読み取り専用で、LLM を呼んだり DB に書いたり（和訳の依頼、評価、既読の記録）はしない。

## ツール

- `search_articles`：記事の検索。引数はどれも省略できる。一覧と違い、既読の記事も返し、同じ報道の記事をまとめない
  - `keyword`：原題・本文・要約・和訳に含む語（全文検索）。空白で区切ると、すべてを含む記事に絞る
  - `since` / `until`：日本時間の日付・月・年（`YYYY-MM-DD` / `YYYY-MM` / `YYYY`、`until` はその日・月・年を含む）。既定は Web UI と同じ直近 `web.list_days` 日
  - `source`：ソースの ID
  - `min_score`：最低点（0〜100。既定は Web UI の一覧と同じく `web.min_score` で、プロファイルが無ければ点数で絞らない）。点数は推薦点で、
    結果には補正の前の `llm_score` も付く
  - `include_hidden`：Web UI の「すべて表示」と同じく、評価 1〜2・閾値未満・未採点・軽水炉と無関係の記事も含める
  - `limit`：最大件数（既定は `web.list_limit`）
- `get_article`：記事 1 件（`id`）の元記事の URL、最新の要約、全文和訳があればその本文

## 登録

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
