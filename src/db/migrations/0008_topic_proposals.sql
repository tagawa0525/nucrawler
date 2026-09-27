-- 要約が提案した新しい語を語彙に加えた時刻。初期語彙と `topics import` で入れた語は NULL。
-- 週 1 回の語彙の整理で、新しく増えた語を見分けるのに使う。
ALTER TABLE topics ADD COLUMN added_at TEXT;

-- 0007 の後、要約の保存が付与を書くようになるまでの間に作られた要約も、
-- 語彙と同じ名前のトピックを付与として移す（0007 と同じ処理。既にある付与は無視する）。
INSERT OR IGNORE INTO artifact_topics (artifact_id, topic_id)
SELECT a.id, t.id
FROM artifacts AS a, json_each(a.payload, '$.topics') AS j
JOIN topics AS t ON t.name = j.value
WHERE a.kind = 'digest';
