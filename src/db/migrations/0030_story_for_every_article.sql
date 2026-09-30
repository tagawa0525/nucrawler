-- すべての記事を同じ報道のグループに入れる（単独の記事は、自分の ID を story_id とする大きさ 1 のグループ）。
-- 読む側が「グループに入っていない記事」を分けて扱わずに済むようにする。0029 の article_stories は
-- 2 件以上のグループだけを置いていたので、既存の記事の分を埋め、追加された記事はトリガーで入れる。
INSERT INTO article_stories (article_id, story_id)
SELECT id, id FROM articles
WHERE id NOT IN (SELECT article_id FROM article_stories);

CREATE TRIGGER article_stories_on_insert AFTER INSERT ON articles BEGIN
    INSERT INTO article_stories (article_id, story_id) VALUES (NEW.id, NEW.id);
END;

-- 2 件以上のグループの記事（代表以外）を、全件を読まずに引く
CREATE INDEX article_stories_grouped ON article_stories (story_id) WHERE story_id <> article_id;

-- 記事を消してグループの代表（story_id と同じ ID の記事）の行が消えたら、残りの記事を残りのうち最小の
-- ID に付け替える（story_id はいつも、そのグループにいる最小の記事の ID）。1 件だけ残れば自分の ID になる。
-- つながりが切れた分のグループの分かれ方は、次の作り直しで直る。
CREATE TRIGGER article_stories_on_delete AFTER DELETE ON article_stories
WHEN OLD.story_id = OLD.article_id BEGIN
    UPDATE article_stories
    SET story_id = (SELECT min(article_id) FROM article_stories WHERE story_id = OLD.article_id)
    WHERE story_id = OLD.article_id;
END;
