//! 利用者のアカウント：パスワード・セッション・フィードのトークンと、ログインの失敗の記録（計画 009）。
//! 認証の状態を書き換える操作は、Web サーバーとは別のプロセス（CLI）とも重ならないよう、
//! `BEGIN IMMEDIATE` のトランザクションの中で前提を確かめてから書く。

use super::*;

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
        for n in 0..6 {
            login(&db, Some("hash-1"), n == 0, NOW);
        }
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
        let read = db.session_password_hash(&mine, t(NOW)).unwrap();
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
        let read = db.session_password_hash(&session, t(NOW)).unwrap();
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
