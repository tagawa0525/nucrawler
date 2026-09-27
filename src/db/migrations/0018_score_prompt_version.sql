-- 採点のプロンプトを変えたら採点し直せるよう、採点にプロンプトの版（prompt_version）を持たせ、
-- 同じ利用者・プロファイル・モデルでも版が違えば別の行として残す。既存の行は版 1 のプロンプトで作った。
-- 一意の制約はテーブルに付いていて変えられないので作り直す。scores を参照しているテーブルは無い。
CREATE TABLE scores_new (
    id             INTEGER PRIMARY KEY,
    user_id        INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    artifact_id    INTEGER NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
    profile_hash   TEXT NOT NULL,
    backend        TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    score          INTEGER NOT NULL CHECK (score BETWEEN 0 AND 100),
    reason         TEXT,                         -- 理由を返さない判定器もある
    created_at     TEXT NOT NULL,
    UNIQUE (user_id, artifact_id, profile_hash, backend, model, prompt_version)
);
INSERT INTO scores_new
    (id, user_id, artifact_id, profile_hash, backend, model, prompt_version, score, reason,
     created_at)
SELECT id, user_id, artifact_id, profile_hash, backend, model, 1, score, reason, created_at
FROM scores;
DROP TABLE scores;
ALTER TABLE scores_new RENAME TO scores;
CREATE INDEX scores_by_artifact ON scores (artifact_id);
