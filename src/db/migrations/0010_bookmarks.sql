-- 一覧での振り分け：ブックマーク（弱い好意）と「見ない」（弱い不要）。
-- どちらも行動として events に記録し、採点の手がかりにする。既存のテーブルの CHECK は
-- 変えられないので、events を作り直して種類を足す（events を参照するテーブルは無い）。
CREATE TABLE events_new (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN (
                   'open_detail', 'open_translation', 'up', 'down', 'bookmark', 'dismiss')),
    created_at TEXT NOT NULL
);
INSERT INTO events_new (id, user_id, article_id, kind, created_at)
    SELECT id, user_id, article_id, kind, created_at FROM events;
DROP TABLE events;
ALTER TABLE events_new RENAME TO events;
CREATE INDEX events_by_user ON events (user_id, created_at);
CREATE INDEX events_by_article ON events (article_id);

-- ブックマークの今の状態。外しても、ブックマークした行動（events）は採点のために残す。
CREATE TABLE bookmarks (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, article_id)
) WITHOUT ROWID;
-- 記事の削除時に bookmarks を全件走査しないよう、外部キーの子側に索引を付ける
CREATE INDEX bookmarks_by_article ON bookmarks (article_id);
