-- プロファイルの版（計画 016）。保存するたびに追記し、消さない。今のプロファイル（profiles）は、退いていない版
-- （retired_at が NULL。利用者ごとに 1 つ）と同じ中身。
-- evidence は、案から作った版の根拠にした記事の id（JSON 配列）。その版の一致率から除く。
-- rated・concordance は、版が退いた時点で固めた一致率（評価の件数と値）。NULL ならその場で集計する
-- （今の版と、この移行で入れた版）。
CREATE TABLE profile_versions (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    interests   TEXT NOT NULL CHECK (json_valid(interests)),
    excludes    TEXT NOT NULL CHECK (json_valid(excludes)),
    hash        TEXT NOT NULL,
    origin      TEXT NOT NULL CHECK (origin IN ('import', 'suggest', 'auto', 'revert')),
    evidence    TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(evidence)),
    created_at  TEXT NOT NULL,
    retired_at  TEXT,
    rated       INTEGER CHECK (rated >= 0),
    concordance REAL CHECK (concordance BETWEEN 0 AND 1)
);
CREATE INDEX profile_versions_by_user ON profile_versions (user_id, created_at);
CREATE UNIQUE INDEX profile_versions_current ON profile_versions (user_id) WHERE retired_at IS NULL;

-- 今のプロファイルを最初の版にする（出どころは取り込み。今は CLI の取り込みでしか保存できない）
INSERT INTO profile_versions (user_id, interests, excludes, hash, origin, created_at)
SELECT user_id, interests, excludes, hash, 'import', updated_at FROM profiles;
