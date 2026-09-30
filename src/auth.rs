//! 認証の部品：パスワードのハッシュと照合、トークンと初期パスワードの生成、ログインの失敗の待ち時間。
//! 乱数は OS から取る。DB と Web には依存しない（計画 009）。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_passwords_verify_only_the_same_password() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(verify_password(&hash, "correct horse battery"));
        assert!(!verify_password(&hash, "correct horse battery "));
        // 同じパスワードでも、ソルトが毎回違うのでハッシュは変わる
        assert_ne!(hash, hash_password("correct horse battery").unwrap());
        // 壊れたハッシュは照合に通らない（panic しない）
        assert!(!verify_password("not a hash", "correct horse battery"));
    }

    /// ダミーのハッシュは、どのパスワードでも照合に通らない（ID が無いときも照合して時間を揃えるため）。
    #[test]
    fn dummy_hash_never_verifies() {
        assert!(dummy_hash().starts_with("$argon2id$"));
        assert!(!verify_password(dummy_hash(), ""));
        assert!(!verify_password(dummy_hash(), "correct horse battery"));
    }

    #[test]
    fn new_passwords_need_12_chars_and_at_most_1024_bytes() {
        assert_eq!(
            check_password("a".repeat(11).as_str()),
            Err(PasswordRule::TooShort)
        );
        assert_eq!(check_password("a".repeat(12).as_str()), Ok(()));
        // 文字数で数える（日本語の 12 文字は 36 バイト）
        assert_eq!(check_password("あ".repeat(12).as_str()), Ok(()));
        assert_eq!(check_password("a".repeat(1024).as_str()), Ok(()));
        assert_eq!(
            check_password("a".repeat(1025).as_str()),
            Err(PasswordRule::TooLong)
        );
        // 計算の前に確かめる上限は、ログインでも同じ
        assert!(within_password_limit(&"a".repeat(1024)));
        assert!(!within_password_limit(&"a".repeat(1025)));
    }

    #[test]
    fn login_ids_are_non_empty_and_at_most_254_bytes() {
        assert!(valid_login("someone@example.com"));
        assert!(valid_login(&"a".repeat(254)));
        assert!(!valid_login(&"a".repeat(255)));
        assert!(!valid_login(""));
        assert!(!valid_login("  "));
    }

    /// 初期パスワードは紛らわしい文字を除いた英数字 20 文字で、パスワードの条件を満たす。
    #[test]
    fn initial_passwords_avoid_confusable_characters() {
        let a = initial_password().unwrap();
        let b = initial_password().unwrap();
        assert_ne!(a, b);
        for p in [&a, &b] {
            assert_eq!(p.chars().count(), 20, "{p}");
            assert!(p.chars().all(|c| c.is_ascii_alphanumeric()), "{p}");
            assert!(!p.contains(['0', 'O', '1', 'l', 'I']), "{p}");
            assert_eq!(check_password(p), Ok(()));
        }
    }

    /// トークンは 256 bit を 16 進の 64 文字で表す（URL にそのまま入る）。
    #[test]
    fn tokens_are_256_bit_hex() {
        let a = random_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(
            a.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_ne!(a, random_token().unwrap());
    }

    /// ID ごとの待ち時間：5 回目までは待たず、6〜9 回目は 1・2・4・8 分、10 回目以降は 15 分。
    #[test]
    fn lockout_doubles_from_the_sixth_failure_up_to_15_minutes() {
        let minutes = |n| lockout(n).map(|d| d.num_minutes());
        assert_eq!(minutes(5), None);
        assert_eq!(minutes(6), Some(1));
        assert_eq!(minutes(7), Some(2));
        assert_eq!(minutes(8), Some(4));
        assert_eq!(minutes(9), Some(8));
        assert_eq!(minutes(10), Some(15));
        // 回数は 10 で止めるが、それより大きくても計算はあふれない
        assert_eq!(minutes(u8::MAX), Some(15));
        assert_eq!(next_failure_count(9), 10);
        assert_eq!(next_failure_count(10), 10);
    }
}
