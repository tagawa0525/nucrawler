-- 本文が取れず要約できない英語記事の見出しを和訳して残せるよう、成果物の種類に見出しの和訳（title）を足す。
-- payload は {"title_ja": ...} で、生成列の title_ja がそのまま使える。
-- CHECK はテーブルに付いていて変えられないので、0017 と同じく作り直す。参照している側
-- （artifact_inputs・artifact_topics・scores）が消えないよう、マイグレーションの間は外部キーを止めている。
CREATE TABLE artifacts_new (
    id             INTEGER PRIMARY KEY,
    article_id     INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('digest', 'translation', 'judgment', 'title')),
    backend        TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    input_scope    TEXT NOT NULL,
    payload        TEXT NOT NULL CHECK (json_valid(payload)),
    created_at     TEXT NOT NULL,
    glossary_at    TEXT,
    title_ja       TEXT GENERATED ALWAYS AS (json_extract(payload, '$.title_ja')) VIRTUAL,
    summary_ja     TEXT GENERATED ALWAYS AS (json_extract(payload, '$.summary_ja')) VIRTUAL,
    UNIQUE (id, article_id)                      -- artifact_inputs の複合外部キー用
);
INSERT INTO artifacts_new
    (id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at,
     glossary_at)
SELECT id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at,
       glossary_at
FROM artifacts;
DROP TABLE artifacts;
ALTER TABLE artifacts_new RENAME TO artifacts;
CREATE UNIQUE INDEX artifacts_identity ON artifacts
    (article_id, kind, backend, model, prompt_version, input_scope, coalesce(glossary_at, ''));

-- 作り直しで消えた全文検索のトリガーを戻す（0017 と同じ）。見出しの和訳も検索に入れる。
-- 判定（judgment）は検索の対象にしない。
CREATE TRIGGER search_docs_artifact_insert AFTER INSERT ON artifacts
WHEN NEW.kind IN ('digest', 'translation', 'title') BEGIN
    INSERT INTO search_docs (article_id, artifact_id, text)
    VALUES (NEW.article_id, NEW.id, CASE NEW.kind
        WHEN 'digest' THEN concat_ws(char(10),
            json_extract(NEW.payload, '$.title_ja'), json_extract(NEW.payload, '$.summary_ja'))
        WHEN 'title' THEN coalesce(json_extract(NEW.payload, '$.title_ja'), '')
        ELSE coalesce(json_extract(NEW.payload, '$.body_ja'), '')
    END);
END;
CREATE TRIGGER search_docs_artifact_delete AFTER DELETE ON artifacts BEGIN
    DELETE FROM search_docs WHERE artifact_id = OLD.id;
END;
