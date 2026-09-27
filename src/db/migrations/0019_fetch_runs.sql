-- 成功した取得 1 回ごとの件数。一覧の selector が壊れて 0 件になった、絞り込みが外れて新着が
-- 途絶えた、といった「取得は成功したが中身が無い」状態を、件数の推移から見つけるために残す。
-- 失敗した回は source_state に記録するので、ここには入れない。
CREATE TABLE fetch_runs (
    id         INTEGER PRIMARY KEY,
    source_id  TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    total      INTEGER NOT NULL CHECK (total >= 0),              -- 一覧・フィードの件数（絞り込み前）
    matched    INTEGER NOT NULL CHECK (matched BETWEEN 0 AND total),
    new        INTEGER NOT NULL CHECK (new >= 0),
    duplicate  INTEGER NOT NULL CHECK (duplicate >= 0),
    -- URL が不正な候補は登録せずに飛ばすので、new + duplicate が matched に満たないことがある
    CHECK (new + duplicate <= matched)
);
CREATE INDEX fetch_runs_by_source ON fetch_runs (source_id, fetched_at);
