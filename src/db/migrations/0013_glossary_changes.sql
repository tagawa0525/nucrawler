-- 訳語集を画面で編集する。訳語（訳・略語・メモ）を変えた時刻と、原語を加えた時刻を残す。
-- 初期値の語は NULL（以前コードに持っていた訳語集と同じで、どの要約・和訳よりも前からある）。
ALTER TABLE glossary_terms ADD COLUMN changed_at TEXT;
ALTER TABLE glossary_sources ADD COLUMN added_at TEXT;
