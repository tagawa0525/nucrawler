-- 訳語の指摘の対応状況。受付中（pending）から追加済・登録済・却下に変えた時刻を resolved_at（対応日時）に残す。
-- term_id は反映した・既にあった訳語で、訳語を消しても指摘は残す。reply は対応のひとこと。
ALTER TABLE term_reports ADD COLUMN status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'added', 'existing', 'rejected'));
ALTER TABLE term_reports ADD COLUMN term_id INTEGER
    REFERENCES glossary_terms (id) ON DELETE SET NULL;
ALTER TABLE term_reports ADD COLUMN reply TEXT CHECK (trim(reply) <> '');
CREATE INDEX term_reports_by_term ON term_reports (term_id);
