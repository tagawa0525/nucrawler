-- 評価：記事を自分に推薦すべきだったかを 1〜5 で付ける（5 必読、4 読んでよかった、3 どちらでもない、
-- 2 不要、1 二度と出さないでほしい）。評価のラベルはこれだけで、1 記事 1 行の今の状態として持つ。
-- 評価なしは行が無いことで表す（3 とは別物）。
CREATE TABLE ratings (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    value      INTEGER NOT NULL CHECK (value BETWEEN 1 AND 5),
    rated_at   TEXT NOT NULL,
    PRIMARY KEY (user_id, article_id)
) WITHOUT ROWID;
-- 親の削除時に ratings を全件走査しないよう、外部キーの子側に索引を付ける
CREATE INDEX ratings_by_article ON ratings (article_id);

-- 👍/👎 は、記事ごとに最後のもの（同じ時刻なら後に記録したもの）を 👍 → 4、👎 → 2 で移す
INSERT INTO ratings (user_id, article_id, value, rated_at)
    SELECT e.user_id, e.article_id, CASE e.kind WHEN 'up' THEN 4 ELSE 2 END, e.created_at
    FROM events AS e
    WHERE e.kind IN ('up', 'down')
      AND NOT EXISTS (
        SELECT 1 FROM events AS f
        WHERE f.user_id = e.user_id AND f.article_id = e.article_id AND f.kind IN ('up', 'down')
          AND (f.created_at > e.created_at OR (f.created_at = e.created_at AND f.id > e.id)));
DELETE FROM events WHERE kind IN ('up', 'down');
