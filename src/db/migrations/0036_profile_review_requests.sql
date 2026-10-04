-- 利用者が Web から頼んだプロファイルの見直し（計画 016）。評価の件数によらず、次の `crawl --requests-only` か
-- `crawl` で案を作り、手動の案を保存したら消す（評価が無くて案を作れないときも消す）。
CREATE TABLE profile_review_requests (
    user_id      INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL
);
