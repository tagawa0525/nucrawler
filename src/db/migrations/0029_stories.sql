-- 同じ出来事を報じた記事をまとめられるよう、成果物の種類に同じ報道・関連の判定（story）を足す。
-- payload は {"candidates": [...], "same": [...], "related": [...]}（記事の ID）。CHECK は変えられないので、
-- 0023 と同じく作り直す。
CREATE TABLE artifacts_new (
    id             INTEGER PRIMARY KEY,
    article_id     INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('digest', 'translation', 'judgment', 'title', 'story')),
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

-- 作り直しで消えた全文検索のトリガーを戻す（0023 と同じ）。判定（judgment・story）は検索の対象にしない。
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

-- 判定した組。same は同じ出来事の報道、related は同じ案件の別の出来事（続報など）。similarity は
-- 候補を選んだときの文字の類似度で、グループをつなぐ順に使う。
CREATE TABLE story_links (
    artifact_id INTEGER NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
    other_id    INTEGER NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
    relation    TEXT NOT NULL CHECK (relation IN ('same', 'related')),
    similarity  REAL NOT NULL,
    PRIMARY KEY (artifact_id, other_id)
);
CREATE INDEX story_links_by_other ON story_links (other_id);

-- 一覧で 1 件にまとめるグループ。same の組をつないだもの（2 件以上）で、story_links から作り直す派生データ。
-- story_id はグループの最小の記事 ID。
CREATE TABLE article_stories (
    article_id INTEGER PRIMARY KEY REFERENCES articles (id) ON DELETE CASCADE,
    story_id   INTEGER NOT NULL
);
CREATE INDEX article_stories_by_story ON article_stories (story_id);
