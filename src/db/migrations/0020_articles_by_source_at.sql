-- ソースごとの記事を、一覧などと同じ日時（公開日時、無ければ取得日時）で引く索引。
-- 新着の途絶えの警告は画面を開くたびに調べるので、ソースごとに最新の記事と直近の期間だけを
-- 索引で読めるようにする（記事の全件を読まない）。
CREATE INDEX articles_by_source_at ON articles (source_id, coalesce(published_at, fetched_at));
