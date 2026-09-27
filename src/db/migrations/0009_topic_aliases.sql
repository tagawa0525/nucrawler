-- 統合した語の別名。LLM が統合元の名前をまた付けたり提案したりしても、統合先に付ける
-- （記録が無いと、統合した揺れが毎週また生まれる）。統合を決めた LLM と時刻を履歴として残す。
CREATE TABLE topic_aliases (
    alias     TEXT PRIMARY KEY,
    topic_id  INTEGER NOT NULL REFERENCES topics (id) ON DELETE CASCADE,
    merged_at TEXT NOT NULL,
    backend   TEXT NOT NULL,
    model     TEXT NOT NULL
) WITHOUT ROWID;
CREATE INDEX topic_aliases_by_topic ON topic_aliases (topic_id);
