//! DB のほかのファイルが問い合わせに埋め込む SQL の断片（閲覧の制限・最新の要約・和文の見出しなど）と、
//! 値の DB での表現。

use crate::config::Lang;

/// 記事の言語の、DB での値（`articles.lang`）。
pub(super) fn lang_code(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        Lang::Ja => "ja",
    }
}

/// 記事（式 `article`）の、利用者（パラメータ `user`）が閲覧できる最新の要約の列 `column`（要約が無ければ
/// NULL）。記事ごとにまとめて選ぶ `latest_digests` の CTE と同じ規則（同じ時刻なら id の大きい方）。
pub(super) fn latest_digest(column: &str, article: &str, user: &str) -> String {
    format!(
        "(SELECT ld.{column} FROM artifacts AS ld
          WHERE ld.article_id = {article} AND ld.kind = 'digest' AND {viewable}
          ORDER BY ld.created_at DESC, ld.id DESC LIMIT 1)",
        viewable = viewable("ld", user),
    )
}

/// 記事（式 `article`）の最新の見出しの和訳（無ければ NULL）。見出しは公開なので閲覧の制限は掛けない。
pub(super) fn latest_title_translation(article: &str) -> String {
    format!(
        "(SELECT lt.title_ja FROM artifacts AS lt
          WHERE lt.article_id = {article} AND lt.kind = 'title'
          ORDER BY lt.created_at DESC, lt.id DESC LIMIT 1)"
    )
}

/// 和文の見出し：要約の見出し（式 `digest_title`）、空か無ければ記事（式 `article`）の最新の見出しの和訳
/// （本文が取れず要約できない記事）。どちらも無ければ NULL。
pub(super) fn title_ja(digest_title: &str, article: &str) -> String {
    format!(
        "coalesce(nullif(trim({digest_title}), ''), {translation})",
        translation = latest_title_translation(article),
    )
}

/// 一覧が記事の順位に使う点数の列 `column`：要約（式 `digest`）を、利用者 `user` の、hash が `profile` の
/// プロファイルで採点したもの（無ければ NULL）。プロファイルの版の一致率も同じ点数で測る（計画 016）。
pub(super) fn list_score(column: &str, digest: &str, user: &str, profile: &str) -> String {
    format!(
        "(SELECT s.{column} FROM scores AS s
          WHERE s.user_id = {user} AND s.profile_hash = {profile}
            AND s.artifact_id = {digest}
            -- embedding の点数は、採点器を選べるようになるまで（計画 010 の段階 3）使わない
            AND s.backend <> 'embedding'
          -- 採点のプロンプトの最新の版を使い、その版で複数のモデルの採点があれば、
          -- 先回り和訳と同じく最高点を使う
          ORDER BY s.prompt_version DESC, s.score DESC, s.created_at DESC, s.id DESC
          LIMIT 1)"
    )
}

/// 点数（式 `score_id`）で当たった語（`kind` は `interest` か `exclude`）の名前の JSON 配列（名前の順）。
/// 推薦の補正の特徴になるので、一覧・学習・`eval` で同じ値を読む。
pub(super) fn matched_topics(score_id: &str, kind: &str) -> String {
    format!(
        "(SELECT json_group_array(topic) FROM (
           SELECT sm.topic FROM score_matches AS sm
           WHERE sm.score_id = {score_id} AND sm.kind = '{kind}' ORDER BY sm.topic))"
    )
}

/// 別名 `alias` の要約に付いている語の名前（語彙の登録順の JSON 配列）。統合を反映するので、
/// payload の `topics`（LLM が出した名前のまま）ではなくこちらを見せる。
pub(super) fn linked_topics(alias: &str) -> String {
    format!(
        "(SELECT json_group_array(name) FROM (
           SELECT t.name FROM artifact_topics AS at
           JOIN topics AS t ON t.id = at.topic_id
           WHERE at.artifact_id = {alias}.id
           ORDER BY t.id))"
    )
}

/// 利用者（パラメータ `user`）が閲覧できる要約（`viewable`）と、そのうち記事ごとに最新のもの（`latest`）の
/// CTE（`WITH` の後に置く）。採点の対象を選ぶ処理で、LLM と embedding の条件をそろえる。記事ごとに選ぶ
/// `latest_digest` と同じ規則（まとめて選ぶ場面の実行計画を変えないよう、別に持つ）。
pub(super) fn latest_digests(user: &str) -> String {
    format!(
        "viewable AS (
           SELECT r.* FROM artifacts AS r
           WHERE r.kind = 'digest' AND {viewable}
         ),
         latest AS (
           SELECT v.* FROM viewable AS v
           WHERE NOT EXISTS (
             SELECT 1 FROM viewable AS w
             WHERE w.article_id = v.article_id
               AND (w.created_at > v.created_at
                    OR (w.created_at = v.created_at AND w.id > v.id)))
         )",
        viewable = viewable("r", user),
    )
}

/// 別名 `alias` の成果物を、利用者（パラメータ `user`。`:user` や `?1`）が閲覧できる条件。
pub(super) fn viewable(alias: &str, user: &str) -> String {
    format!(
        "NOT EXISTS (
           SELECT 1 FROM artifact_access AS aa
           WHERE aa.artifact_id = {alias}.id
             AND aa.membership_id NOT IN (
               SELECT membership_id FROM user_memberships WHERE user_id = {user}))"
    )
}
