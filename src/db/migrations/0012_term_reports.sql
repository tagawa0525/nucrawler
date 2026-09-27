-- 訳語の指摘の受付箱。読んでいて気になった訳を記事ごとに溜め、あとで訳語集に反映して閉じる。
-- 原語や希望する訳は分からなければ空（NULL）でよい。
CREATE TABLE term_reports (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id  INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    found       TEXT NOT NULL CHECK (trim(found) <> ''),
    wanted      TEXT CHECK (trim(wanted) <> ''),
    source      TEXT CHECK (trim(source) <> ''),
    note        TEXT CHECK (trim(note) <> ''),
    reported_at TEXT NOT NULL,
    resolved_at TEXT
);
CREATE INDEX term_reports_by_article ON term_reports (article_id);
CREATE INDEX term_reports_open ON term_reports (reported_at) WHERE resolved_at IS NULL;
