-- 指摘を訳語に限らず受け付ける。種類（kind）を持たせ、訳語の指摘は気になった訳（found）を、
-- ほかの種類は内容（note）を必須にする。訳語以外の対応は「対応済」（done）で、訳語集の状況や訳語は付けない。
-- NOT NULL と CHECK は変えられないので作り直し、名前も reports にする（参照しているテーブルは無い）。
CREATE TABLE reports (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    article_id  INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('term', 'translation', 'digest', 'topic', 'body', 'other')),
    found       TEXT CHECK (trim(found) <> ''),
    wanted      TEXT CHECK (trim(wanted) <> ''),
    source      TEXT CHECK (trim(source) <> ''),
    note        TEXT CHECK (trim(note) <> ''),
    status      TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'added', 'existing', 'done', 'rejected')),
    term_id     INTEGER REFERENCES glossary_terms (id) ON DELETE SET NULL,
    reply       TEXT CHECK (trim(reply) <> ''),
    reported_at TEXT NOT NULL,
    resolved_at TEXT,
    CHECK (CASE kind
             WHEN 'term' THEN found IS NOT NULL AND status <> 'done'
             ELSE found IS NULL AND wanted IS NULL AND source IS NULL AND note IS NOT NULL
                  AND status IN ('pending', 'done', 'rejected') AND term_id IS NULL
           END)
);
INSERT INTO reports (id, user_id, article_id, kind, found, wanted, source, note, status, term_id,
                     reply, reported_at, resolved_at)
    SELECT id, user_id, article_id, 'term', found, wanted, source, note, status, term_id,
           reply, reported_at, resolved_at
    FROM term_reports;
DROP TABLE term_reports;
CREATE INDEX reports_by_article ON reports (article_id);
CREATE INDEX reports_by_term ON reports (term_id);
CREATE INDEX reports_open ON reports (reported_at) WHERE status = 'pending';
