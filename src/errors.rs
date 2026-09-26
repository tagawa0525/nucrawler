//! エラー表示の共通処理。
//!
//! 規約：各エラー型の Display は自分の文脈だけを書き、原因は `#[source]`/`#[from]` で
//! `source()` として返す。原因をつないで表示するのは `error_chain` だけにする。

use std::fmt::Write as _;

/// エラーと、その原因（`source()`）を ": " でつないだ文字列。
pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        let _ = write!(out, ": {c}");
        cause = c.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 各エラーの Display は自分の文脈だけを書き、原因は source() に任せる。
    /// そうしないと error_chain で原因の文言が 2 回出る。
    #[test]
    fn error_chain_does_not_repeat_causes() {
        let cause = url::ParseError::RelativeUrlWithoutBase.to_string();
        let errors: Vec<Box<dyn std::error::Error>> = vec![
            Box::new(crate::check::SourceFailure::InvalidUrl {
                url: "::".into(),
                source: url::ParseError::RelativeUrlWithoutBase,
            }),
            Box::new(crate::source::SourceError::InvalidLink {
                href: "::".into(),
                source: url::ParseError::RelativeUrlWithoutBase,
            }),
            Box::new(crate::db::DbError::InvalidUrl {
                url: "::".into(),
                source: url::ParseError::RelativeUrlWithoutBase,
            }),
            Box::new(crate::config::ConfigError::Read {
                path: "x".into(),
                source: std::io::Error::other(cause.clone()),
            }),
        ];
        for e in errors {
            let chain = error_chain(e.as_ref());
            assert_eq!(chain.matches(&cause).count(), 1, "{chain}");
        }
        let json =
            crate::source::SourceError::Json(serde_json::from_str::<u8>("\"a\"").unwrap_err());
        let chain = error_chain(&json);
        assert_eq!(chain.matches("invalid type").count(), 1, "{chain}");
    }
}
