-- 記事への個人的なコメント。1 つの記事に何件でも付けられる。
-- 公開（public）はほかの利用者にも見せ、非公開（private）は書いた本人だけが見る。
CREATE TABLE comments (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    body       TEXT NOT NULL CHECK (trim(body) <> ''),
    visibility TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('public', 'private')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX comments_by_article ON comments (article_id);
CREATE INDEX comments_by_user ON comments (user_id);
