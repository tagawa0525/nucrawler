-- プロファイルの更新案（計画 016）。評価を根拠に LLM が作った案と、今のプロファイルとの比較を残す。
-- base_version_id は案を作ったときの今の版。evidence は根拠にした記事の id（JSON 配列。採用した版の一致率から除く）。
-- current_*・candidate_* は、評価した記事での今のプロファイルと案の一致率（一覧と同じ規則で選んだ点数。
-- 評価の違う組が無ければ concordance は NULL）。
-- status：pending（採用か見送りを待つ）・applied（版にした）・dismissed（見送った）・superseded（新しい案に
-- 置き換わった）・unchanged（今と同じ中身だった）。decided_at は pending でなくなった時刻（unchanged は NULL）。
CREATE TABLE profile_suggestions (
    id                    INTEGER PRIMARY KEY,
    user_id               INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    base_version_id       INTEGER NOT NULL REFERENCES profile_versions (id) ON DELETE CASCADE,
    interests             TEXT NOT NULL CHECK (json_valid(interests)),
    excludes              TEXT NOT NULL CHECK (json_valid(excludes)),
    reasons               TEXT NOT NULL CHECK (json_valid(reasons)),
    evidence              TEXT NOT NULL CHECK (json_valid(evidence)),
    current_rated         INTEGER NOT NULL CHECK (current_rated >= 0),
    current_concordance   REAL CHECK (current_concordance BETWEEN 0 AND 1),
    candidate_rated       INTEGER NOT NULL CHECK (candidate_rated >= 0),
    candidate_concordance REAL CHECK (candidate_concordance BETWEEN 0 AND 1),
    trigger               TEXT NOT NULL CHECK (trigger IN ('auto', 'manual')),
    status                TEXT NOT NULL
        CHECK (status IN ('pending', 'applied', 'dismissed', 'superseded', 'unchanged')),
    created_at            TEXT NOT NULL,
    decided_at            TEXT
);
CREATE INDEX profile_suggestions_by_user ON profile_suggestions (user_id, created_at);

-- 案を自動で当てるか（既定は当てる。当てても履歴から戻せる）
ALTER TABLE users ADD COLUMN auto_apply_profile INTEGER NOT NULL DEFAULT 1
    CHECK (auto_apply_profile IN (0, 1));
