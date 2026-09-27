//! 検索の入口（Web・JSON API・CLI・MCP）で共通の、入力の解釈：検索語の分け方と、
//! 日付（日本時間の年月か年月日）の範囲。

use chrono::{DateTime, Utc};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SearchError {
    #[error("{name} must be YYYY-MM or YYYY-MM-DD, got {value:?}")]
    InvalidDate { name: &'static str, value: String },
}

/// 検索語を空白（全角の空白を含む）で分ける。
pub fn parse_terms(_q: &str) -> Vec<String> {
    Vec::new()
}

/// `since`：その日（年月ならその月の 1 日）の日本時間 0 時。この時刻以降に絞る。
pub fn since(_value: &str) -> Result<DateTime<Utc>, SearchError> {
    Ok(DateTime::UNIX_EPOCH)
}

/// `until`：その日（年月ならその月）の終わり。翌日（翌月 1 日）の日本時間 0 時を返し、この時刻より前に絞る。
pub fn until(_value: &str) -> Result<DateTime<Utc>, SearchError> {
    Ok(DateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    #[test]
    fn splits_terms_on_any_whitespace() {
        assert_eq!(parse_terms(" 炉心　溶融  NRC\t"), ["炉心", "溶融", "NRC"]);
        assert!(parse_terms("　 ").is_empty());
    }

    #[test]
    fn reads_days_and_months_in_jst() {
        assert_eq!(since("2026-09-20").unwrap(), utc("2026-09-19T15:00:00Z"));
        assert_eq!(until("2026-09-20").unwrap(), utc("2026-09-20T15:00:00Z"));
        assert_eq!(since("2026-09").unwrap(), utc("2026-08-31T15:00:00Z"));
        assert_eq!(until("2026-09").unwrap(), utc("2026-09-30T15:00:00Z"));
        assert_eq!(until("2026-12").unwrap(), utc("2026-12-31T15:00:00Z"));
    }

    #[test]
    fn rejects_malformed_dates() {
        for bad in ["2026/09/01", "2026-13", "2026-02-30", "", "2026"] {
            assert_eq!(
                since(bad),
                Err(SearchError::InvalidDate {
                    name: "since",
                    value: bad.into()
                }),
                "{bad}"
            );
            assert!(until(bad).is_err(), "{bad}");
        }
    }
}
