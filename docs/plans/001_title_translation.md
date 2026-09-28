# 本文の無い英語記事の見出しを和訳する

## 背景

一覧・詳細・受付箱の見出しは、記事の最新の要約（`artifacts.kind = 'digest'` の `title_ja`）から取り、
要約が無ければ原題を出す。要約には本文が要るので、本文が取れない英語記事はいつまでも英語の見出しのまま残る。

- IAEA は記事ページが Cloudflare のチャレンジで 403 になり、フィード（`feeds/news`）にも本文が無い。
  2026-09-29 に全件のフィードへ切り替えたので、見出しだけの記事が最大 150 件ほど増える
- ほかのソースでも、抽出に失敗した記事や、抽出の再試行を待っている記事は、それまで英語のまま

一度だけ訳すのではなく、今後入ってくる記事も crawl が自動で訳すステージとして組み込む。

## 方針

- LLM の出力は、ほかと同じく `artifacts` に残す（backend・model・prompt_version 付き）。新しい種類 `title` を足し、
  payload は `{"title_ja": "..."}`。生成列の `title_ja` がそのまま使える
- 表示の見出しは「最新の要約の title_ja → 最新の title の title_ja → 原題」の順に使う。本文が後から取れて
  要約ができれば、要約の見出しが優先される
- 対象：英語の記事で、要約も title も無く、公開の本文（body/fulltext）が無いもの。本文がある記事は同じ crawl の
  要約で見出しが付くので対象にしない。抽出の再試行待ちの記事も、見出しは先に訳しておく（安いので）
- `backlog_days` で絞らない。見出しだけの記事は数が限られ、1 回の呼び出しで数十件を訳せる。絞ると期間より古い記事が
  英語のまま残り続ける
- 失敗は要約と同じく `stage_errors`（stage `title`）に記録して、間隔を置いて再試行し、上限で断念する
- 会員限定の記事も、見出しは公開なので訳す（title の成果物には本文を入力にしないので、閲覧の制限も掛からない）

## 変更

1. マイグレーション `0023_title_artifacts.sql`：`artifacts.kind` の CHECK に `title` を足す（0017 と同じく
   テーブルを作り直し、索引と全文検索のトリガーを戻す）。トリガーは title も索引に入れる（本文は title_ja）
2. `ArtifactKind::Title`
3. `Db::pending_titles(now, backend, model, limit)`：上の対象を新しい順に返す
4. `prompt::title`：system prompt・JSON Schema・プロンプト（`<article id>` で見出しを包む）・応答の検証
5. `pipeline::title`：`title_batch_size` 件ずつ訳して保存する。クォータ・中断・失敗の扱いは要約と同じ
6. `Stage::Title`（名前 `title`、LLM のロック）。順は `fetch → extract → digest → score → translate → title → tidy`
   （推薦に効く要約・採点・和訳を先にする）
7. 設定 `llm.title_model`（既定 `sonnet`）と `llm.title_batch_size`（既定 30）
8. 表示：`db::read` の一覧・詳細と `db::notes` の受付箱で、要約の見出しが無ければ title の見出しを使う
9. README と `examples/config.toml`

## テスト

- マイグレーション：既存の成果物・検索の索引が残り、`title` を保存でき、title_ja が検索に当たる
- `pending_titles`：本文のある記事・要約済み・訳済み・日本語の記事・再試行待ちを除く
- `prompt::title`：スキーマ、見出しの閉じタグの無害化、応答の検証（欠けた記事・空の見出し・余分な項目）
- ステージ：バッチに分けて訳し、保存し、失敗を記録する。クォータで止まる
- 表示：要約が無ければ title の見出し、要約があれば要約の見出し
