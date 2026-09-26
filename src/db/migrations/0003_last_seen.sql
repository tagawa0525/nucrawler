-- 一覧で「前回見てから届いた記事」を区切るための、利用者ごとの最後に一覧を見た時刻。
ALTER TABLE users ADD COLUMN last_seen_at TEXT;
