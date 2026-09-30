# すべての記事を同じ報道のグループに入れる

## 背景

007（同じ報道をまとめる、#134・#135）では、グループ（`article_stories`）に 2 件以上のグループの記事だけを入れた。
そのため、どのグループにも入っていない記事の扱いが、読む側のあちこちに散っている。

- 一覧の SQL：`PARTITION BY coalesce(rows.story_id, -rows.id)`
- `ListItem.story_id: Option<i64>` と、確認枠の除外の `is_none_or`
- 候補選び（`story.rs`）：`stories.get(&id).unwrap_or(article_id)` と、`members()` の単独記事の分岐
- プロンプトに渡す `story: bool`
- 関連記事のまとめ：`unwrap_or(-a.article_id)`

「単独の記事は大きさ 1 のグループ」とすれば、これらの分岐はすべて消える。

## 方針

- `article_stories` には、すべての記事の行を置く。単独の記事の `story_id` は自分の ID
  （グループの ID は最小の記事 ID なので、今の決め方と同じになる）
- ビュー（`articles LEFT JOIN article_stories` と `coalesce`）にはしない
  - グループのほかの記事を読むときは `story_id` で絞り込むが、計算した列には索引が効かない
  - 一覧の 1 行ごとに全記事を走査することになり、一覧全体では O(N²) になる
  - 表なら `article_stories_by_story` の索引がいつも効く
- 行を欠かさない
  - 記事の追加はトリガー（`AFTER INSERT ON articles`）で、自分を ID とするグループの行を入れる
  - 記事を消すと行も消える（外部キーの `ON DELETE CASCADE`）
  - 既存の記事の分は、マイグレーションで埋める
- 作り直し（`rebuild_stories`）は差分だけを書く
  - same の組のつながりから、2 件以上のグループを求める（`story::components`）
  - 今の行のうちグループに入っている行（`story_id <> article_id`）と比べ、ID が変わる行だけを更新する
  - グループから外れた記事は、自分の ID に戻す
  - 書くのは変わった行だけなので、記事が増えても作り直しは重くならない
- Rust 側：記事からグループの ID への対応を `story::Stories` にまとめる
  - 対応に無い記事は自分のグループにいる、という決まりを 1 か所に置く
  - DB からは 2 件以上のグループの行だけを読む（全記事を読まない）
  - 候補選び・関連記事のまとめ・プロンプトは、この型を使う
- `ListItem.story_id` は `i64` にする。グループかどうかは、ほかの記事がいるか（`story_others` が空でないか）で分かる

## 変更

1. マイグレーション `0030_story_for_every_article.sql`：既存の記事の行を埋め、記事の追加のトリガーを置く
2. `Db::rebuild_stories`：差分だけを書く
3. `story::Stories`（`story_of`・`members`）と、候補選び・ステージ・関連記事での使用
4. 一覧の SQL（`PARTITION BY rows.story_id`）、`ListItem.story_id: i64`、確認枠の除外
5. プロンプトの `story: bool` をやめ、ほかの記事がいるかで決める

振る舞い（一覧・確認枠・詳細・判定）は変えない。

## テスト

- 記事を追加すると自分の ID のグループに入り、マイグレーションは既存の記事を埋める
- 作り直しで外れた記事は自分の ID に戻り、変わらない行は書き直さない
- 既存のテスト（一覧・確認枠・詳細・ステージ）がそのまま通る
