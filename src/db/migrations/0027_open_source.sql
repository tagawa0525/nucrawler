-- 原文を開いたこと（詳細の「原文」のリンクから）を、開いた記録として残す。
-- 後で推薦の点数を補正するときの材料にするだけで、既読の判定には使わない（詳細を開かないと原文へは行けない）。
CREATE TABLE events_new (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('open_detail', 'open_translation', 'open_source')),
    created_at TEXT NOT NULL
);
INSERT INTO events_new (id, user_id, article_id, kind, created_at)
    SELECT id, user_id, article_id, kind, created_at FROM events;
DROP TABLE events;
ALTER TABLE events_new RENAME TO events;
CREATE INDEX events_by_user ON events (user_id, created_at);
CREATE INDEX events_by_article ON events (article_id);
