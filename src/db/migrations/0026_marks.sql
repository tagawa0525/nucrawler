-- 既読とブックマークを、行動の記録（events）ではなく利用者が付け外しする印（状態）として持つ。
-- どちらも評価のラベルにはしない（ラベルは ratings だけ）。events には開いた記録だけを残す。

-- 既読：開いたとき・評価したとき・一覧で印を付けたときに付く。read_at は既読になった時刻で、
-- 一覧の「前の訪問より前の欄」は、前の訪問までに既読になった記事を隠す。
CREATE TABLE reads (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    read_at    TEXT NOT NULL,
    PRIMARY KEY (user_id, article_id)
) WITHOUT ROWID;
-- 親の削除時に reads を全件走査しないよう、外部キーの子側に索引を付ける
CREATE INDEX reads_by_article ON reads (article_id);

-- 開いた記録・見送り・評価のうち最初の時刻で既読にする。見送りは「処理済み」の意味でも使われていて、
-- 関心が無いのか似た記事を読んだのか区別できないので、評価ではなく既読に移す。
INSERT INTO reads (user_id, article_id, read_at)
    SELECT user_id, article_id, min(at) FROM (
        SELECT user_id, article_id, created_at AS at FROM events
        WHERE kind IN ('open_detail', 'open_translation', 'dismiss')
        UNION ALL
        SELECT user_id, article_id, rated_at FROM ratings)
    GROUP BY user_id, article_id;

-- ブックマーク：付けた時刻を持ち、付けた行動（events）への参照はやめる。
CREATE TABLE bookmarks_new (
    user_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id    INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    bookmarked_at TEXT NOT NULL,
    PRIMARY KEY (user_id, article_id)
) WITHOUT ROWID;
INSERT INTO bookmarks_new (user_id, article_id, bookmarked_at)
    SELECT b.user_id, b.article_id, e.created_at
    FROM bookmarks AS b JOIN events AS e ON e.id = b.event_id;
DROP TABLE bookmarks;
ALTER TABLE bookmarks_new RENAME TO bookmarks;
CREATE INDEX bookmarks_by_article ON bookmarks (article_id);

-- events は開いた記録だけにする（この時点で events を参照するテーブルは無い）
CREATE TABLE events_new (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('open_detail', 'open_translation')),
    created_at TEXT NOT NULL
);
INSERT INTO events_new (id, user_id, article_id, kind, created_at)
    SELECT id, user_id, article_id, kind, created_at FROM events
    WHERE kind IN ('open_detail', 'open_translation');
DROP TABLE events;
ALTER TABLE events_new RENAME TO events;
CREATE INDEX events_by_user ON events (user_id, created_at);
CREATE INDEX events_by_article ON events (article_id);
