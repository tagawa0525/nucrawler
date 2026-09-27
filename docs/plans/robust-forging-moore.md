# 肥大化ファイルの分割計画

## Context

ソースが大きくなり、一部のファイルが読みにくい。報告（reports）・コメント（comments）・redo --glossary の追加で、前回の計測から上位3ファイルがさらに膨らんだ（db +1163、html +671、server +553 行）。
行数を測り、分割すべきファイルとその切り口を決める。分割はファイルごとに別 PR で行う。

## 計測結果（main @ 50ffaf0、`#[cfg(test)]` の前後で本体とテストに分けた）

| ファイル                            | 合計 | 本体 | テスト | 判定                                 |
| ----------------------------------- | ---: | ---: | -----: | ------------------------------------ |
| src/db/mod.rs                       | 7256 | 2949 |   4307 | **分割必須**                         |
| src/web/html.rs                     | 2282 | 1198 |   1084 | **分割必須**                         |
| src/web/server.rs                   | 2048 |  906 |   1142 | **分割必須**                         |
| src/main.rs                         |  669 |  653 |     16 | 分割推奨（本体が大きくテストが無い） |
| src/config.rs                       |  854 |  444 |    410 | 様子見                               |
| src/cli.rs                          |  775 |  380 |    395 | 不要（clap の定義が主）              |
| src/mcp.rs                          |  810 |  332 |    478 | 不要                                 |
| src/pipeline/digest.rs              |  904 |  216 |    688 | 不要（テストが厚いだけ）             |
| src/pipeline/translate.rs           |  655 |  211 |    444 | 不要                                 |
| digest.rs / check.rs / http.rs ほか | ≤664 | ≤324 |      — | 不要                                 |

判定基準: 本体が 〜500 行を超え、かつ **独立した関心事が複数入っている** もの。テストが長いだけで本体が単一の関心事のファイルは分けない。

## 1. src/db/mod.rs（最優先）

`impl Db` に全関心事が集まり、型定義だけで 〜800 行。Rust は `impl Db` を複数ファイルに書けるので、`src/db/` 以下に関心事ごとのサブモジュールを作り、型・`impl Db` ブロック・テストをまとめて移す。公開 API（`crate::db::Xxx`）は `mod.rs` の `pub use` で維持し、呼び出し側は変更しない。

| 新ファイル        | 移すもの                                                                                                                                                                                                                                                                                                                                                                                     |
| ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `db/mod.rs`       | `DbError`, `Db`, `open`/`open_in_memory`/`schema_version`/`owner_id`, `MIGRATIONS`, `NOW`, `BUSY_TIMEOUT`, `timestamp`, `enable_wal`/`migrate*`/`apply_migrations`, `pub use`                                                                                                                                                                                                                |
| `db/articles.rs`  | `NewArticle`, `ContentKind/Origin`, `insert_article*`, `insert_content`, `normalize_url`, `is_tracking_param`                                                                                                                                                                                                                                                                                |
| `db/sources.rs`   | `SourceOverview`, `SourceState`, `PendingPage`, `record_source_*`, `source_state`, `source_overview`                                                                                                                                                                                                                                                                                         |
| `db/stages.rs`    | `StageKey`, `backoff`, `LlmCall`, `record/clear_stage_failure`, `pending_extract`, `record_llm_call`, `latest_rate_limit`, `llm_succeeded_since`                                                                                                                                                                                                                                             |
| `db/artifacts.rs` | `ArtifactKind`, `NewArtifact`, `DigestInput`, `InputContent`, `insert_artifact`, `pending_digest`, `write_artifact`, `link_digest_topics`                                                                                                                                                                                                                                                    |
| `db/score.rs`     | `ScoreKey`, `score_stage`, `ScoreInput`, `pending_score`, `insert_score`                                                                                                                                                                                                                                                                                                                     |
| `db/feedback.rs`  | `SignalKind`, `Signal`, `Feedback`, `record_event`, `unbookmark`, `undo_event`, `recent_signals`                                                                                                                                                                                                                                                                                             |
| `db/translate.rs` | `TranslateQuery/Input`, `insert_translation`, `request_translation`, `pending_translate`                                                                                                                                                                                                                                                                                                     |
| `db/redo.rs`      | `RedoFilter/Key`, `redo_digest*`, `redo_translate*`, `REDO_*`, `redo_params`                                                                                                                                                                                                                                                                                                                 |
| `db/vocab.rs`     | topics・glossary・profile: `TopicUsage`, `TopicMerge`, `topics`, `glossary_entries`, `add/update/delete_glossary_term`, `check_glossary_conflicts`, `replace_glossary_sources`, `vocabulary`, `replace_topics`, `topic_usage`, `merge_topics`, `save/load_profile`                                                                                                                           |
| `db/notes.rs`     | 報告とコメント: `ReportKind`, `NewReport`, `ReportStatus`, `Report`, `ReportFilter`, `Visibility`, `Comment`, `comments`, `add/update/delete_comment`, `add_report`, `reports`, `report_kind`, `report_counts`, `resolve_report`                                                                                                                                                             |
| `db/read.rs`      | 画面向けの読み出し: `ListItem/Query`, `SearchQuery/Order`, 検索用の補助（`lang_code`, `search_term_filter`, `TRIGRAM_MIN_CHARS`, `is_indexable`, `like_pattern`, `fts_phrase`, `linked_topics`, `viewable`, `SearchFilters`, `ItemScope`, `ContentSet`）, `ArtifactVersion`, `ArticleDetail`, `begin_visit`, `list_articles`, `search_articles`, `article_detail`, `versions`, `query_items` |
| `db/warnings.rs`  | `Warning`, `warnings` と補助関数                                                                                                                                                                                                                                                                                                                                                             |

`db/read.rs` は `query_items` が大きく 〜500 行になる見込み。これは1つの関心事なのでさらには分けない。

テストは各サブモジュールの `mod tests` へ、テスト名が指す関数に合わせて移す。複数モジュールで使う db 固有のテスト補助は `db/mod.rs` の `#[cfg(test)] pub(crate) mod test_support` に集める（既存の `src/testutil.rs` にあるものは再利用）。

## 2. src/web/server.rs

ハンドラ 25 個が1ファイル。`AppState`・`router`・`run`・`AppError`・`with_db`・`viewer`・`find_article`・`check_same_origin` を `web/server/mod.rs` に残し、ハンドラを画面単位で分ける。

- `server/pages.rs`: `list`, `search`, `detail`, `feed`, `settings`（`ListParams`, `DetailParams`, `list_items`, `warnings`）
- `server/json.rs`: `api_list`, `api_search`, `api_detail`, `json`（既存の `web/api.rs` は JSON 変換なので別名にする）
- `server/feedback.rs`: `feedback`, `undo_feedback`, `translation_request`
- `server/notes.rs`: 報告とコメント: `add_report`, `reports`, `resolve_report`, `report_filter`, `add/update/delete_comment`, `back_to_comments` と各フォーム型
- `server/glossary.rs`: `glossary`, `add/update/delete_glossary_term`, `GlossaryForm`, `glossary_error`

## 3. src/web/html.rs

本体 1198 行のうち `STYLE`（〜70 行）・`CALENDAR_SCRIPT`・`SWIPE_SCRIPT`（〜150 行）が埋め込み文字列。

- `web/html/assets/{style.css,calendar.js,swipe.js}` を `include_str!` で読む。エディタで CSS/JS として扱えるようになる
- `web/html/mod.rs`: `escape`, `split_sections`, `Page`, `layout`, `warning_banner`, `button`
- `web/html/list.rs`: `ListView`, `list_page`, `card`
- `web/html/search.rs`: `search_page`, `search_form`, `calendar`
- `web/html/detail.rs`: `Notes`, `DetailView`, `detail_page`, `comment_section`, `report_section`, `report_summary`, `feedback_forms`, `translation_section`
- `web/html/reports.rs`: `reports_page`, `kind_label`, `status_label`
- `web/html/settings.rs`: `settings_page`, `glossary_page`, `glossary_fields`

`include_str!` 化で出力 HTML が1バイトも変わらないよう、ファイル末尾の改行の扱いに注意する（テストで検出できる）。

## 4. src/main.rs

`crawl`（〜200 行）と `redo`（〜90 行）がバイナリ側にあり、テストが 16 行しかない。まずはサブコマンドごとに `src/cmd/{crawl,redo,...}.rs` へ移すだけの分割にする。オーケストレーションを lib 側へ寄せてテスト可能にするのは振る舞いに関わる別件として、この計画の対象外。

## 様子見

- `src/config.rs`（本体 444）: 設定型と読み込み・検証が同居。次の機能追加で膨らんだら `config/sources.rs` 等に分ける
- その他は本体 ≤ 400 行で単一の関心事のため現状維持

## 進め方

- 1ファイル = 1PR。各 PR は移動のみで、振る舞い変更を混ぜない（振る舞いが変わらないので TDD サイクルの対象外）
- PR 内は「サブモジュールを1つ切り出す」単位でコミットを分ける（例: `refactor: move report and comment queries to db::notes`）
- 順序: db → web/server → web/html → main（db は他の3つの依存先で、並行作業と最も競合しやすい）
- 他の機能ブランチが db/web を触っている間は着手を待ち、マージ直後に一気に行う

## 検証

- 各コミットで `cargo build`、`cargo clippy --all-targets -- -D warnings`、`cargo test` が通る
- 分割前後で `cargo test 2>&1 | grep 'test result'` のテスト件数が一致する
- `git diff -M --stat` と `git diff --color-moved` で、差分がほぼ移動だけであることを確認
- 分割後に再計測し、本体 500 行超のファイルが `db/read.rs` 以外に残らないこと
