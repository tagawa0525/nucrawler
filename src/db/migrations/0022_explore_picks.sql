-- 確認枠：閾値未満の記事から日ごとに無作為に選んで一覧に出した記事。表示されない記事の反応（見逃し）を
-- 集めるために出す。同じ日は同じ記事を出し、1 つの記事は 1 回しか選ばないので、利用者と記事で一意にする。
-- picked_on は選んだ日（日本時間の日付 YYYY-MM-DD）。
CREATE TABLE explore_picks (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    picked_on  TEXT NOT NULL,
    PRIMARY KEY (user_id, article_id)
) WITHOUT ROWID;
CREATE INDEX explore_picks_by_day ON explore_picks (user_id, picked_on);
CREATE INDEX explore_picks_by_article ON explore_picks (article_id);
