-- 一覧の「前回から」の区切り。閲覧の間隔が短いうちは同じ訪問とみなして区切りを保ち、
-- 再読み込みや詳細からの戻りで新着が「それ以前」に移らないようにする。
ALTER TABLE users ADD COLUMN visit_boundary_at TEXT;
