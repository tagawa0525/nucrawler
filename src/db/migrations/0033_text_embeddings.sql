-- 好み（関心分野・推薦しない話題）の文の embedding（計画 010）。キーは接頭辞を付けた入力の文そのもので、
-- 利用者やプロファイルには結び付けない（同じ文なら誰がいつ作っても同じベクトルなので、プロファイルの差し替えと
-- ベクトルの作成が重なっても整合の仕組みが要らない）。空間を消せば一緒に消える。
CREATE TABLE text_embeddings (
    text     TEXT PRIMARY KEY,
    space_id INTEGER NOT NULL REFERENCES embedding_space (id) ON DELETE CASCADE,
    vector   BLOB NOT NULL
) WITHOUT ROWID;
CREATE INDEX text_embeddings_by_space ON text_embeddings (space_id);
