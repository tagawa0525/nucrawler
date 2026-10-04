//! 利用者のアカウント：パスワード・セッション・フィードのトークンと、ログインの失敗の記録（計画 009）。
//! 認証の状態を書き換える操作は、Web サーバーとは別のプロセス（CLI）とも重ならないよう、
//! `BEGIN IMMEDIATE` のトランザクションの中で前提を確かめてから書く。

use super::*;

use rusqlite::OptionalExtension;

use crate::auth;

/// 利用者の一覧の 1 行（`nucrawler user list`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSummary {
    pub login: String,
    pub display_name: String,
    /// 管理者（所有者）
    pub is_admin: bool,
    /// パスワードが設定済みで、ログインできる
    pub has_password: bool,
}

/// セッションから引いた利用者。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewer {
    pub user_id: i64,
    pub is_admin: bool,
}

/// セッションの利用者のパスワードの状態（パスワードの変更の 1 段目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPassword {
    /// 今のパスワードのハッシュ
    pub hash: Option<String>,
    /// ログインの失敗が続いて待ち時間中
    pub locked: bool,
}

/// 本人のパスワードの変更の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordChange {
    /// 変えた。ほかのセッションとフィードのトークンは消え、要求元には新しいセッションを出す
    Changed { token: String },
    /// 今のパスワードが違う（ログインの失敗と同じく数える）
    WrongPassword,
    /// ログインの失敗が続いて待ち時間中
    Locked,
    /// セッションが無いか、照合の間にパスワードが変わった（停止・リセット・別の変更）
    NoSession,
}

/// 書き込みのトランザクションの中で読み直す、利用者の認証の状態。
struct AuthState {
    id: i64,
    password_hash: Option<String>,
    failed_logins: u8,
    locked_until: Option<String>,
}

impl AuthState {
    const COLUMNS: &str = "u.id, u.password_hash, u.failed_logins, u.locked_until";

    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            password_hash: r.get(1)?,
            failed_logins: r.get(2)?,
            locked_until: r.get(3)?,
        })
    }

    /// 待ち時間中か（`now` は `timestamp` の書式。同じ書式なので文字列で比べられる）。
    fn locked(&self, now: &str) -> bool {
        self.locked_until
            .as_deref()
            .is_some_and(|until| until > now)
    }

    /// 照合に使ったハッシュが今も同じか。ハッシュには毎回違うソルトが入るので、設定し直せば必ず変わる。
    fn same_hash(&self, read_hash: Option<&str>) -> bool {
        read_hash.is_some() && self.password_hash.as_deref() == read_hash
    }
}

/// 一意の制約に反したか（ログイン ID の重複）。
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
}

impl Db {
    /// 読んでから書き換えるトランザクション（認証の状態・プロファイルの版）。始めた時点で書き込みのロックを取り、
    /// ほかのプロセスの書き込みと重ならない。
    pub(super) fn immediate(&self) -> Result<rusqlite::Transaction<'_>, DbError> {
        Ok(rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?)
    }

    /// 利用者を作る。パスワードのハッシュは呼び出し側が作る。
    pub fn add_user(
        &self,
        login: &str,
        display_name: &str,
        password_hash: &str,
    ) -> Result<i64, DbError> {
        self.conn
            .execute(
                "INSERT INTO users (login, display_name, password_hash) VALUES (?1, ?2, ?3)",
                [login, display_name, password_hash],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    DbError::LoginTaken(login.to_string())
                } else {
                    e.into()
                }
            })?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 利用者の一覧（ログイン ID の順）。
    pub fn users(&self) -> Result<Vec<UserSummary>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT login, display_name, is_owner, password_hash IS NOT NULL FROM users ORDER BY login",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(UserSummary {
                login: r.get(0)?,
                display_name: r.get(1)?,
                is_admin: r.get(2)?,
                has_password: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// ログイン ID を変える（所有者の `owner` をメールアドレスにするときなど）。
    pub fn rename_user(&self, login: &str, new_login: &str) -> Result<(), DbError> {
        let changed = self
            .conn
            .execute(
                "UPDATE users SET login = ?2 WHERE login = ?1",
                [login, new_login],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    DbError::LoginTaken(new_login.to_string())
                } else {
                    e.into()
                }
            })?;
        if changed == 0 {
            return Err(DbError::UnknownUser(login.to_string()));
        }
        Ok(())
    }

    /// 資格をすべて失効させてから、新しいパスワードを設定する（`nucrawler user reset-password`）。
    pub fn reset_password(&self, login: &str, password_hash: &str) -> Result<(), DbError> {
        self.replace_credentials(login, Some(password_hash))
    }

    /// 資格をすべて失効させ、パスワードも無くす（`nucrawler user disable`）。戻すときはリセットする。
    pub fn disable_user(&self, login: &str) -> Result<(), DbError> {
        self.replace_credentials(login, None)
    }

    fn replace_credentials(&self, login: &str, password_hash: Option<&str>) -> Result<(), DbError> {
        let tx = self.immediate()?;
        let id: i64 = self
            .conn
            .query_row("SELECT id FROM users WHERE login = ?1", [login], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| DbError::UnknownUser(login.to_string()))?;
        self.revoke_credentials(id)?;
        self.conn.execute(
            "UPDATE users SET password_hash = ?2 WHERE id = ?1",
            rusqlite::params![id, password_hash],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// パスワード以外の資格（セッション・フィードのトークン）と、ログインの失敗の記録をまとめて消す。
    /// 止めるときにどれかを消し忘れると、そこから読み続けられるので、失効はすべてここを通す。
    fn revoke_credentials(&self, user_id: i64) -> Result<(), DbError> {
        self.conn
            .execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
        self.conn.execute(
            "UPDATE users SET feed_token = NULL, failed_logins = 0, locked_until = NULL WHERE id = ?1",
            [user_id],
        )?;
        Ok(())
    }

    /// ログインの 1 段目：照合の相手（今のパスワードのハッシュ）。ID が無いかパスワードが無ければ `None`
    /// （呼び出し側はダミーのハッシュで照合して時間を揃える）。
    pub fn login_hash(&self, login: &str) -> Result<Option<String>, DbError> {
        Ok(self
            .conn
            .query_row(
                "SELECT password_hash FROM users WHERE login = ?1",
                [login],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// ログインの 3 段目：照合の結果（`verified`）と、1 段目で読んだハッシュ（`read_hash`）から判定して書く。
    /// 成功は、照合が通り、ハッシュが今も同じで、待ち時間中でないときだけで、セッションのトークンを返す。
    /// 待ち時間中の試行は数えない。それ以外の失敗は、ID があれば回数と待ち時間を更新する。
    pub fn finish_login(
        &self,
        login: &str,
        read_hash: Option<&str>,
        verified: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<String>, DbError> {
        let tx = self.immediate()?;
        let state = self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM users AS u WHERE u.login = ?1",
                    AuthState::COLUMNS
                ),
                [login],
                AuthState::from_row,
            )
            .optional()?;
        let token = match state {
            None => None,
            Some(state) if state.locked(&timestamp(now)) => None,
            Some(state) if verified && state.same_hash(read_hash) => {
                self.conn.execute(
                    "UPDATE users SET failed_logins = 0, locked_until = NULL WHERE id = ?1",
                    [state.id],
                )?;
                Some(self.new_session(state.id, now)?)
            }
            Some(state) => {
                self.record_failure(&state, now)?;
                None
            }
        };
        tx.commit()?;
        Ok(token)
    }

    /// ログインの失敗を 1 回数える。待ち時間に入ったら `true`。
    fn record_failure(
        &self,
        state: &AuthState,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        let failures = auth::next_failure_count(state.failed_logins);
        let until = auth::lockout(failures).map(|wait| timestamp(now + wait));
        self.conn.execute(
            "UPDATE users SET failed_logins = ?2, locked_until = ?3 WHERE id = ?1",
            rusqlite::params![state.id, failures, until],
        )?;
        Ok(until.is_some())
    }

    /// セッションを作り、トークンを返す。ついでに全員の期限切れのセッションを消す（捨てられた Cookie の行が残らないよう）。
    fn new_session(
        &self,
        user_id: i64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, DbError> {
        let at = timestamp(now);
        self.conn
            .execute("DELETE FROM sessions WHERE expires_at <= ?1", [&at])?;
        let token = auth::random_token()?;
        self.conn.execute(
            "INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                token,
                user_id,
                at,
                timestamp(now + chrono::Duration::days(auth::SESSION_DAYS))
            ],
        )?;
        Ok(token)
    }

    /// セッションの利用者（期限切れなら `None`）。
    pub fn session_viewer(
        &self,
        token: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Viewer>, DbError> {
        Ok(self
            .conn
            .query_row(
                "SELECT u.id, u.is_owner FROM sessions AS s JOIN users AS u ON u.id = s.user_id
                 WHERE s.token = ?1 AND s.expires_at > ?2",
                [token, &timestamp(now)],
                |r| {
                    Ok(Viewer {
                        user_id: r.get(0)?,
                        is_admin: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn logout(&self, token: &str) -> Result<(), DbError> {
        self.conn
            .execute("DELETE FROM sessions WHERE token = ?1", [token])?;
        Ok(())
    }

    /// パスワードの変更の 1 段目：セッションの利用者の今のパスワードのハッシュと、待ち時間中か。セッションが無ければ `None`。
    /// 待ち時間中なら、呼び出し側は照合も計算もせずに断る（正しいときだけ新しいハッシュを作ると、その時間の差で正しさが分かるため）。
    pub fn session_password(
        &self,
        token: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<SessionPassword>, DbError> {
        let now = timestamp(now);
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM sessions AS s JOIN users AS u ON u.id = s.user_id
                     WHERE s.token = ?1 AND s.expires_at > ?2",
                    AuthState::COLUMNS
                ),
                [token, &now],
                AuthState::from_row,
            )
            .optional()?
            .map(|state| SessionPassword {
                locked: state.locked(&now),
                hash: state.password_hash,
            }))
    }

    /// パスワードの変更の 3 段目：照合の結果から判定して書く。セッションがまだあり、照合に使ったハッシュが今も同じで、
    /// 待ち時間中でないときだけ変える。今のパスワードの誤りはログインの失敗と同じく数え、待ち時間に入ったら
    /// その人のセッションをすべて消す（盗まれたセッションで試している相手を締め出す）。
    pub fn change_password(
        &self,
        token: &str,
        read_hash: Option<&str>,
        verified: bool,
        new_hash: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<PasswordChange, DbError> {
        let tx = self.immediate()?;
        let state = self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM sessions AS s JOIN users AS u ON u.id = s.user_id
                     WHERE s.token = ?1 AND s.expires_at > ?2",
                    AuthState::COLUMNS
                ),
                [token, &timestamp(now)],
                AuthState::from_row,
            )
            .optional()?;
        let outcome = match state {
            Some(state) if state.same_hash(read_hash) => {
                if state.locked(&timestamp(now)) {
                    PasswordChange::Locked
                } else if !verified {
                    if self.record_failure(&state, now)? {
                        self.conn
                            .execute("DELETE FROM sessions WHERE user_id = ?1", [state.id])?;
                    }
                    PasswordChange::WrongPassword
                } else {
                    self.revoke_credentials(state.id)?;
                    self.conn.execute(
                        "UPDATE users SET password_hash = ?2 WHERE id = ?1",
                        rusqlite::params![state.id, new_hash],
                    )?;
                    PasswordChange::Changed {
                        token: self.new_session(state.id, now)?,
                    }
                }
            }
            _ => PasswordChange::NoSession,
        };
        tx.commit()?;
        Ok(outcome)
    }

    /// フィードのトークンを作り直す（古い URL は使えなくなる）。セッションが無ければ何も書かない。
    pub fn rotate_feed_token(
        &self,
        session: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<String>, DbError> {
        let tx = self.immediate()?;
        let Some(viewer) = self.session_viewer(session, now)? else {
            return Ok(None);
        };
        let token = auth::random_token()?;
        self.conn.execute(
            "UPDATE users SET feed_token = ?2 WHERE id = ?1",
            rusqlite::params![viewer.user_id, token],
        )?;
        tx.commit()?;
        Ok(Some(token))
    }

    /// フィードのトークンの利用者。
    pub fn feed_viewer(&self, token: &str) -> Result<Option<i64>, DbError> {
        Ok(self
            .conn
            .query_row("SELECT id FROM users WHERE feed_token = ?1", [token], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// 利用者の今のフィードのトークン（設定画面に購読用の URL を出す）。
    pub fn feed_token(&self, user_id: i64) -> Result<Option<String>, DbError> {
        Ok(self
            .conn
            .query_row(
                "SELECT feed_token FROM users WHERE id = ?1",
                [user_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::t;

    const NOW: &str = "2026-10-01T00:00:00Z";

    fn db_with_user() -> (Db, i64) {
        let db = Db::open_in_memory().unwrap();
        let id = db.add_user("a@example.com", "A", "hash-1").unwrap();
        (db, id)
    }

    /// 照合が通った体でログインを終える（照合の結果は Web 側が渡す）。
    fn login(db: &Db, hash: Option<&str>, verified: bool, at: &str) -> Option<String> {
        db.finish_login("a@example.com", hash, verified, t(at))
            .unwrap()
    }

    #[test]
    fn users_are_listed_with_their_state() {
        let (db, _) = db_with_user();
        assert!(matches!(
            db.add_user("a@example.com", "A2", "h"),
            Err(DbError::LoginTaken(_))
        ));
        let users = db.users().unwrap();
        assert_eq!(
            users,
            [
                UserSummary {
                    login: "a@example.com".into(),
                    display_name: "A".into(),
                    is_admin: false,
                    has_password: true,
                },
                UserSummary {
                    login: "owner".into(),
                    display_name: "owner".into(),
                    is_admin: true,
                    has_password: false,
                },
            ]
        );
    }

    #[test]
    fn login_hash_is_the_current_password_hash() {
        let (db, _) = db_with_user();
        assert_eq!(
            db.login_hash("a@example.com").unwrap().as_deref(),
            Some("hash-1")
        );
        assert_eq!(db.login_hash("nobody").unwrap(), None);
        // パスワードの無い利用者（所有者の初期状態）は照合の相手が無い
        assert_eq!(db.login_hash("owner").unwrap(), None);
    }

    #[test]
    fn a_verified_login_creates_a_session() {
        let (db, id) = db_with_user();
        let token = login(&db, Some("hash-1"), true, NOW).unwrap();
        let viewer = db.session_viewer(&token, t(NOW)).unwrap().unwrap();
        assert_eq!(
            viewer,
            Viewer {
                user_id: id,
                is_admin: false
            }
        );
        // 30 日で切れる
        assert!(
            db.session_viewer(&token, t("2026-10-30T23:59:59Z"))
                .unwrap()
                .is_some()
        );
        assert!(
            db.session_viewer(&token, t("2026-10-31T00:00:00Z"))
                .unwrap()
                .is_none()
        );
        assert!(db.session_viewer("unknown", t(NOW)).unwrap().is_none());
        db.logout(&token).unwrap();
        assert!(db.session_viewer(&token, t(NOW)).unwrap().is_none());
    }

    #[test]
    fn logins_fail_without_verification_or_for_unknown_ids() {
        let (db, _) = db_with_user();
        assert_eq!(login(&db, Some("hash-1"), false, NOW), None);
        assert_eq!(
            db.finish_login("nobody", None, false, t(NOW)).unwrap(),
            None
        );
        // パスワードの無い利用者は、照合の結果によらずログインできない
        assert_eq!(db.finish_login("owner", None, true, t(NOW)).unwrap(), None);
    }

    /// 照合の後・書き込みの前にリセットがあると、古いパスワードで照合が通っていてもログインできない。
    #[test]
    fn a_login_verified_against_a_replaced_hash_fails() {
        let (db, _) = db_with_user();
        let read = db.login_hash("a@example.com").unwrap();
        db.reset_password("a@example.com", "hash-2").unwrap();
        assert_eq!(login(&db, read.as_deref(), true, NOW), None);
        let read = db.login_hash("a@example.com").unwrap();
        db.disable_user("a@example.com").unwrap();
        assert_eq!(login(&db, read.as_deref(), true, NOW), None);
    }

    /// 6 回目の失敗から待たせ、待ち時間中は正しくても通らず、数えもしない。過ぎて成功すると数え直す。
    #[test]
    fn repeated_failures_lock_the_login_for_a_while() {
        let (db, _) = db_with_user();
        let failures = |db: &Db| {
            db.conn()
                .query_row(
                    "SELECT failed_logins, locked_until FROM users WHERE login = 'a@example.com'",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
                )
                .unwrap()
        };
        for _ in 0..5 {
            login(&db, Some("hash-1"), false, NOW);
        }
        assert_eq!(failures(&db), (5, None));
        login(&db, Some("hash-1"), false, NOW);
        assert_eq!(failures(&db), (6, Some("2026-10-01T00:01:00.000Z".into())));
        // 待ち時間中は正しいパスワードでも通らず、回数も期限も変わらない
        assert_eq!(
            login(&db, Some("hash-1"), true, "2026-10-01T00:00:30Z"),
            None
        );
        login(&db, Some("hash-1"), false, "2026-10-01T00:00:40Z");
        assert_eq!(failures(&db), (6, Some("2026-10-01T00:01:00.000Z".into())));
        // 過ぎたら通り、回数と期限が戻る
        assert!(login(&db, Some("hash-1"), true, "2026-10-01T00:01:00Z").is_some());
        assert_eq!(failures(&db), (0, None));
    }

    #[test]
    fn failure_count_stops_at_ten() {
        let (db, _) = db_with_user();
        db.conn()
            .execute(
                "UPDATE users SET failed_logins = 10 WHERE login = 'a@example.com'",
                [],
            )
            .unwrap();
        login(&db, Some("hash-1"), false, NOW);
        let (n, until): (i64, String) = db
            .conn()
            .query_row(
                "SELECT failed_logins, locked_until FROM users WHERE login = 'a@example.com'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((n, until.as_str()), (10, "2026-10-01T00:15:00.000Z"));
    }

    /// ログインでセッションを作るとき、ほかの利用者の分も含めて期限切れの行を消す。
    #[test]
    fn creating_a_session_sweeps_expired_ones() {
        let (db, _) = db_with_user();
        login(&db, Some("hash-1"), true, "2026-08-01T00:00:00Z").unwrap();
        db.conn()
            .execute(
                "INSERT INTO sessions (token, user_id, created_at, expires_at)
                 SELECT 'old', id, '2026-08-01T00:00:00.000Z', '2026-08-31T00:00:00.000Z'
                 FROM users WHERE login = 'owner'",
                [],
            )
            .unwrap();
        login(&db, Some("hash-1"), true, NOW).unwrap();
        let count: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    /// 停止とリセットは、パスワード・セッション・フィードのトークン・失敗の記録をまとめて初期化する。
    #[test]
    fn disabling_and_resetting_revoke_every_credential() {
        let (db, _) = db_with_user();
        // 1 回成功してから 6 回失敗させ、待ち時間に入れる
        for n in 0..7 {
            login(&db, Some("hash-1"), n == 0, NOW);
        }
        assert_eq!(login(&db, Some("hash-1"), true, NOW), None, "locked");
        let session = db
            .conn()
            .query_row("SELECT token FROM sessions", [], |r| r.get::<_, String>(0))
            .unwrap();
        let feed = db.rotate_feed_token(&session, t(NOW)).unwrap().unwrap();
        assert!(db.feed_viewer(&feed).unwrap().is_some());

        db.reset_password("a@example.com", "hash-2").unwrap();
        assert!(db.session_viewer(&session, t(NOW)).unwrap().is_none());
        assert!(db.feed_viewer(&feed).unwrap().is_none());
        // 待ち時間中でも、リセットしたらすぐ新しいパスワードでログインできる
        assert!(login(&db, Some("hash-2"), true, NOW).is_some());

        db.disable_user("a@example.com").unwrap();
        assert_eq!(db.login_hash("a@example.com").unwrap(), None);
        assert_eq!(
            db.conn()
                .query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(matches!(
            db.disable_user("nobody"),
            Err(DbError::UnknownUser(_))
        ));
        assert!(matches!(
            db.reset_password("nobody", "h"),
            Err(DbError::UnknownUser(_))
        ));
    }

    #[test]
    fn users_can_be_renamed() {
        let (db, _) = db_with_user();
        let owner = db.owner_id().unwrap();
        db.rename_user("owner", "me@example.com").unwrap();
        assert_eq!(db.owner_id().unwrap(), owner);
        assert!(
            db.users()
                .unwrap()
                .iter()
                .any(|u| u.login == "me@example.com" && u.is_admin)
        );
        assert!(matches!(
            db.rename_user("me@example.com", "a@example.com"),
            Err(DbError::LoginTaken(_))
        ));
        assert!(matches!(
            db.rename_user("nobody", "x"),
            Err(DbError::UnknownUser(_))
        ));
    }

    /// 本人のパスワードの変更：セッションもフィードのトークンもすべて消し、要求元に新しいセッションを出す。
    #[test]
    fn changing_the_password_rotates_every_session_and_the_feed_token() {
        let (db, id) = db_with_user();
        let mine = login(&db, Some("hash-1"), true, NOW).unwrap();
        let other = login(&db, Some("hash-1"), true, NOW).unwrap();
        let feed = db.rotate_feed_token(&mine, t(NOW)).unwrap().unwrap();
        let read = db
            .session_password(&mine, t(NOW))
            .unwrap()
            .and_then(|p| p.hash);
        assert_eq!(read.as_deref(), Some("hash-1"));
        let outcome = db
            .change_password(&mine, read.as_deref(), true, "hash-2", t(NOW))
            .unwrap();
        let PasswordChange::Changed { token } = outcome else {
            panic!("{outcome:?}")
        };
        for old in [&mine, &other] {
            assert!(db.session_viewer(old, t(NOW)).unwrap().is_none());
        }
        assert_eq!(
            db.session_viewer(&token, t(NOW))
                .unwrap()
                .map(|v| v.user_id),
            Some(id)
        );
        assert!(db.feed_viewer(&feed).unwrap().is_none());
        assert_eq!(
            db.login_hash("a@example.com").unwrap().as_deref(),
            Some("hash-2")
        );
    }

    /// 今のパスワードの誤りはログインと同じ回数に数え、待ち時間に入ったらその人のセッションをすべて消す。
    /// 待ち時間中は、正しくても変えられない。
    #[test]
    fn wrong_current_passwords_count_as_login_failures() {
        let (db, _) = db_with_user();
        let session = login(&db, Some("hash-1"), true, NOW).unwrap();
        for _ in 0..5 {
            let outcome = db
                .change_password(&session, Some("hash-1"), false, "hash-2", t(NOW))
                .unwrap();
            assert_eq!(outcome, PasswordChange::WrongPassword);
        }
        assert!(db.session_viewer(&session, t(NOW)).unwrap().is_some());
        let outcome = db
            .change_password(&session, Some("hash-1"), false, "hash-2", t(NOW))
            .unwrap();
        assert_eq!(outcome, PasswordChange::WrongPassword);
        assert!(db.session_viewer(&session, t(NOW)).unwrap().is_none());

        // 待ち時間中は（別のセッションからでも）正しい今のパスワードで変えられない
        db.conn()
            .execute(
                "INSERT INTO sessions (token, user_id, created_at, expires_at)
                 SELECT 's2', id, ?1, '2026-12-01T00:00:00.000Z' FROM users WHERE login = 'a@example.com'",
                [crate::db::timestamp(t(NOW))],
            )
            .unwrap();
        let outcome = db
            .change_password("s2", Some("hash-1"), true, "hash-2", t(NOW))
            .unwrap();
        assert_eq!(outcome, PasswordChange::Locked);
        assert_eq!(
            db.login_hash("a@example.com").unwrap().as_deref(),
            Some("hash-1")
        );
    }

    /// 照合の後・書き込みの前に停止されたら、パスワードの変更もフィードのトークンの作り直しも書かない。
    #[test]
    fn credential_changes_after_revocation_write_nothing() {
        let (db, _) = db_with_user();
        let session = login(&db, Some("hash-1"), true, NOW).unwrap();
        let read = db
            .session_password(&session, t(NOW))
            .unwrap()
            .and_then(|p| p.hash);
        db.disable_user("a@example.com").unwrap();
        let outcome = db
            .change_password(&session, read.as_deref(), true, "hash-2", t(NOW))
            .unwrap();
        assert_eq!(outcome, PasswordChange::NoSession);
        assert_eq!(db.login_hash("a@example.com").unwrap(), None);
        assert_eq!(db.rotate_feed_token(&session, t(NOW)).unwrap(), None);
        let feeds: i64 = db
            .conn()
            .query_row("SELECT count(feed_token) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(feeds, 0);
    }

    /// パスワードの変更の 1 段目は、ハッシュと一緒に待ち時間中かを返す（待ち時間中は照合も計算もせずに断るため）。
    /// ログインの失敗ではセッションは消えないので、待ち時間中でもセッションはある。
    #[test]
    fn session_password_reports_the_lockout() {
        let (db, _) = db_with_user();
        let session = login(&db, Some("hash-1"), true, NOW).unwrap();
        assert_eq!(
            db.session_password(&session, t(NOW)).unwrap(),
            Some(SessionPassword {
                hash: Some("hash-1".into()),
                locked: false
            })
        );
        for _ in 0..6 {
            login(&db, Some("hash-1"), false, NOW);
        }
        assert_eq!(
            db.session_password(&session, t(NOW)).unwrap(),
            Some(SessionPassword {
                hash: Some("hash-1".into()),
                locked: true
            })
        );
        assert_eq!(db.session_password("unknown", t(NOW)).unwrap(), None);
    }

    #[test]
    fn feed_tokens_identify_their_user_until_rotated() {
        let (db, id) = db_with_user();
        let session = login(&db, Some("hash-1"), true, NOW).unwrap();
        let first = db.rotate_feed_token(&session, t(NOW)).unwrap().unwrap();
        assert_eq!(db.feed_viewer(&first).unwrap(), Some(id));
        let second = db.rotate_feed_token(&session, t(NOW)).unwrap().unwrap();
        assert_ne!(first, second);
        assert_eq!(db.feed_viewer(&first).unwrap(), None);
        assert_eq!(db.feed_viewer(&second).unwrap(), Some(id));
        assert_eq!(db.feed_token(id).unwrap().as_deref(), Some(second.as_str()));
    }
}
