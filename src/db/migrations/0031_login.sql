-- ログイン ID とパスワードでの認証（計画 009）。login は利用者が入れるログイン ID（メールアドレス）。

-- password_hash は argon2id の PHC 文字列。NULL ならログインできない（所有者の初期状態と、停止した利用者）。
ALTER TABLE users ADD COLUMN password_hash TEXT;
-- 最後に成功してから続けて失敗した回数（10 で止める）と、次に試せる時刻
ALTER TABLE users ADD COLUMN failed_logins INTEGER NOT NULL DEFAULT 0
    CHECK (failed_logins BETWEEN 0 AND 10);
ALTER TABLE users ADD COLUMN locked_until TEXT;
-- フィードを読むためのトークン（URL に入れる）。ALTER TABLE ... ADD COLUMN は UNIQUE の列を足せないので、一意性は索引で付ける
ALTER TABLE users ADD COLUMN feed_token TEXT;
CREATE UNIQUE INDEX users_by_feed_token ON users (feed_token);

-- ログインのセッション。token は Cookie に入れる乱数（DB を読めるのは稼働ホストの管理者だけなので平文で置く）
CREATE TABLE sessions (
    token      TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
) WITHOUT ROWID;
CREATE INDEX sessions_by_user ON sessions (user_id);
