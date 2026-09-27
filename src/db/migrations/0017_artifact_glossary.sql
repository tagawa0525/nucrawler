-- 訳語集が変わった後に要約・和訳を作り直せるよう、成果物に使った訳語集の時点（glossary_at）を持たせ、
-- 同じモデル・プロンプト版でも時点が違えば別の版として残す。glossary_at は記事に当たった訳語のうち
-- 最も新しく変えた時刻で、当たる語が無いか、記録を始める前の成果物は NULL。
-- 一意の制約はテーブルに付いていて変えられないので作り直す。参照している側（artifact_inputs・
-- artifact_topics・scores）が消えないよう、マイグレーションの間は外部キーを止めている。
CREATE TABLE artifacts_new (
    id             INTEGER PRIMARY KEY,
    article_id     INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('digest', 'translation', 'judgment')),
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
    (id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at)
SELECT id, article_id, kind, backend, model, prompt_version, input_scope, payload, created_at
FROM artifacts;
DROP TABLE artifacts;
ALTER TABLE artifacts_new RENAME TO artifacts;
CREATE UNIQUE INDEX artifacts_identity ON artifacts
    (article_id, kind, backend, model, prompt_version, input_scope, coalesce(glossary_at, ''));

-- 作り直しで消えた全文検索のトリガーを戻す（0006 と同じ）。判定（judgment）は検索の対象にしない。
CREATE TRIGGER search_docs_artifact_insert AFTER INSERT ON artifacts
WHEN NEW.kind IN ('digest', 'translation') BEGIN
    INSERT INTO search_docs (article_id, artifact_id, text)
    VALUES (NEW.article_id, NEW.id, CASE NEW.kind
        WHEN 'digest' THEN concat_ws(char(10),
            json_extract(NEW.payload, '$.title_ja'), json_extract(NEW.payload, '$.summary_ja'))
        ELSE coalesce(json_extract(NEW.payload, '$.body_ja'), '')
    END);
END;
CREATE TRIGGER search_docs_artifact_delete AFTER DELETE ON artifacts BEGIN
    DELETE FROM search_docs WHERE artifact_id = OLD.id;
END;
