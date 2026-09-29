-- 呼び出しで消費した AI Credits（copilot-cli、10^-9 クレジット単位）。使用率（rate_limit）が分からない
-- バックエンドのクォータを、月の消費の合計で判定するために残す。
ALTER TABLE llm_calls ADD COLUMN credits_nano INTEGER CHECK (credits_nano IS NULL OR credits_nano >= 0);
