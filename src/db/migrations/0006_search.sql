-- 全文検索の索引。1 行が 1 つの文書（記事の原題、本文の部分、要約の版、和訳の版）。
-- 閲覧できるかは由来の行で判定するので、由来の id を持たせる（原題は両方とも NULL）。
-- trigram は形態素解析なしで日本語も部分一致で引けるが、3 文字未満の語は索引を使えない。
CREATE VIRTUAL TABLE search_docs USING fts5(
    article_id UNINDEXED,
    content_id UNINDEXED,
    artifact_id UNINDEXED,
    text,
    tokenize = 'trigram'
);

-- 同期はトリガで行う。記事・本文・成果物は追記と削除だけで、更新しない。
-- 外部キーの CASCADE で消えるときもトリガは動く。

CREATE TRIGGER search_docs_article_insert AFTER INSERT ON articles BEGIN
    INSERT INTO search_docs (article_id, text) VALUES (NEW.id, NEW.title);
END;
CREATE TRIGGER search_docs_article_delete AFTER DELETE ON articles BEGIN
    DELETE FROM search_docs
    WHERE article_id = OLD.id AND content_id IS NULL AND artifact_id IS NULL;
END;

CREATE TRIGGER search_docs_content_insert AFTER INSERT ON contents BEGIN
    INSERT INTO search_docs (article_id, content_id, text)
    VALUES (NEW.article_id, NEW.id, NEW.text);
END;
CREATE TRIGGER search_docs_content_delete AFTER DELETE ON contents BEGIN
    DELETE FROM search_docs WHERE content_id = OLD.id;
END;

-- 判定（judgment）は検索の対象にしない。
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

-- 既存の行を入れる。
INSERT INTO search_docs (article_id, text) SELECT id, title FROM articles;
INSERT INTO search_docs (article_id, content_id, text) SELECT article_id, id, text FROM contents;
INSERT INTO search_docs (article_id, artifact_id, text)
SELECT article_id, id, CASE kind
    WHEN 'digest' THEN concat_ws(char(10),
        json_extract(payload, '$.title_ja'), json_extract(payload, '$.summary_ja'))
    ELSE coalesce(json_extract(payload, '$.body_ja'), '')
END
FROM artifacts
WHERE kind IN ('digest', 'translation');
