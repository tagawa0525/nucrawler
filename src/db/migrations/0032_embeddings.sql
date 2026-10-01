-- 記事の embedding（計画 010）。

-- ベクトルの空間（モデル）。比べられるのは同じ空間のベクトルどうしだけなので、空間は常に 1 つにする（singleton）。
-- name は設定（URL・モデル・次元・接頭辞）から作った名前、input_version は入力の組み立て方の版、fingerprint は
-- 決まった試験文のベクトル（JSON）。どれかが今の設定・モデルと違えば embed は止まり、`nucrawler embed rebuild` で
-- 行ごと消して作り直す。id は世代で、消した後も同じ値を使い回さない（AUTOINCREMENT）。rebuild の前に始まった処理は、
-- 始めに読んだ世代の行が無くなっていれば何も保存しない。
CREATE TABLE embedding_space (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    singleton     INTEGER NOT NULL DEFAULT 1 UNIQUE CHECK (singleton = 1),
    name          TEXT NOT NULL,
    input_version INTEGER NOT NULL,
    fingerprint   TEXT NOT NULL CHECK (json_valid(fingerprint)),
    created_at    TEXT NOT NULL
);

-- 要約（digest）ごとの embedding。vector は L2 正規化した f32 のリトルエンディアン。
-- 空間を消せば一緒に消える。
CREATE TABLE article_embeddings (
    artifact_id INTEGER PRIMARY KEY REFERENCES artifacts (id) ON DELETE CASCADE,
    space_id    INTEGER NOT NULL REFERENCES embedding_space (id) ON DELETE CASCADE,
    vector      BLOB NOT NULL
);
CREATE INDEX article_embeddings_by_space ON article_embeddings (space_id);
