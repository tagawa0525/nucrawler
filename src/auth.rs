//! 認証の部品：パスワードのハッシュと照合、トークンと初期パスワードの生成、ログインの失敗の待ち時間。
//! 乱数は OS から取る。DB と Web には依存しない（計画 009）。

use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("failed to read random bytes from the OS")]
    Random(#[from] getrandom::Error),
    #[error("failed to hash a password: {0}")]
    Hash(argon2::password_hash::Error),
}

/// 本人が決めるパスワードの最短の文字数。
pub const MIN_PASSWORD_CHARS: usize = 12;
/// パスワードの最長のバイト数。極端に長い入力でハッシュの計算を重くされないよう、計算の前に確かめる。
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// ログイン ID の最長のバイト数（メールアドレスの上限）。
pub const MAX_LOGIN_BYTES: usize = 254;
/// セッションの有効期間（ログインから）。
pub const SESSION_DAYS: i64 = 30;
/// ID ごとの失敗の回数の上限。待ち時間はここで上限に達するので、これより先は数えない。
pub const MAX_FAILURES: u8 = 10;

/// 新しいパスワードが条件を満たさない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordRule {
    TooShort,
    TooLong,
}

impl std::fmt::Display for PasswordRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasswordRule::TooShort => {
                write!(
                    f,
                    "パスワードは {MIN_PASSWORD_CHARS} 文字以上にしてください"
                )
            }
            PasswordRule::TooLong => {
                write!(
                    f,
                    "パスワードは {MAX_PASSWORD_BYTES} バイト以下にしてください"
                )
            }
        }
    }
}

/// 本人が決めるパスワードの条件。文字の種類は問わない（長さのほうが強さに効くため）。
pub fn check_password(password: &str) -> Result<(), PasswordRule> {
    if !within_password_limit(password) {
        Err(PasswordRule::TooLong)
    } else if password.chars().count() < MIN_PASSWORD_CHARS {
        Err(PasswordRule::TooShort)
    } else {
        Ok(())
    }
}

/// ハッシュを計算してよい長さか（ログインの入力にも掛ける上限）。
pub fn within_password_limit(password: &str) -> bool {
    password.len() <= MAX_PASSWORD_BYTES
}

/// ログイン ID として使えるか（空白だけでなく、254 バイト以下）。
pub fn valid_login(login: &str) -> bool {
    !login.trim().is_empty() && login.len() <= MAX_LOGIN_BYTES
}

/// argon2id（既定の強さ）でハッシュにし、PHC 文字列（`$argon2id$...`）で返す。ソルトは毎回 OS の乱数から作る。
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(AuthError::Hash)
}

/// `hash` がこのパスワードのものか。壊れたハッシュは通さない。
pub fn verify_password(hash: &str, password: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

/// ID が無いときやパスワードが無いときに照合する相手。どのパスワードでも通らないが、照合には同じ時間がかかる。
pub fn dummy_hash() -> &'static str {
    // 元の値は上限を超える長さにする。ログインの入力は計算の前に上限で断るので、この値とは一致しえない
    static DUMMY: LazyLock<String> = LazyLock::new(|| {
        hash_password(&"x".repeat(MAX_PASSWORD_BYTES + 1))
            .expect("argon2id with default params hashes any input")
    });
    &DUMMY
}

/// 256 bit の乱数を 16 進の 64 文字で返す（セッション・フィードのトークン）。
pub fn random_token() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// 初期パスワードの文字。紛らわしい 0・O・1・l・I を除いた英数字（57 文字）。
const INITIAL_CHARS: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// CLI が発行する初期パスワード（20 文字、約 116 bit）。
pub fn initial_password() -> Result<String, AuthError> {
    // 偏らないよう、文字の数の倍数（57 × 4 = 228）未満のバイトだけを使う
    let limit = (256 / INITIAL_CHARS.len() * INITIAL_CHARS.len()) as u8;
    let mut out = String::with_capacity(20);
    let mut buf = [0u8; 64];
    while out.len() < 20 {
        getrandom::fill(&mut buf)?;
        out.extend(
            buf.iter()
                .filter(|b| **b < limit)
                .map(|b| INITIAL_CHARS[usize::from(*b) % INITIAL_CHARS.len()] as char)
                .take(20 - out.len()),
        );
    }
    Ok(out)
}

/// 続けて `failures` 回失敗した後の待ち時間。5 回までは待たず、6〜9 回目は 1・2・4・8 分、10 回目以降は 15 分。
pub fn lockout(failures: u8) -> Option<chrono::Duration> {
    match failures {
        0..=5 => None,
        6..=9 => Some(chrono::Duration::minutes(1 << (failures - 6))),
        _ => Some(chrono::Duration::minutes(15)),
    }
}

/// 失敗を 1 回数えた後の回数（`MAX_FAILURES` で止める）。
pub fn next_failure_count(failures: u8) -> u8 {
    failures.saturating_add(1).min(MAX_FAILURES)
}

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

    /// ダミーのハッシュは埋め込んだ定数で、要求の処理中に計算しない（初めての照合だけ遅くなると、ID の有無が漏れる）。
    /// 照合の時間を揃えるため、強さのパラメータは実際のハッシュと同じ。
    #[test]
    fn dummy_hash_is_a_constant_with_the_real_params() {
        let params = |h: &str| h.rsplitn(3, '$').nth(2).unwrap().to_string();
        assert_eq!(dummy_hash(), DUMMY_HASH);
        assert_eq!(params(DUMMY_HASH), params(&hash_password("x").unwrap()));
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
