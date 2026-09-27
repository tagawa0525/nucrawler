//! 検索の入口（Web・JSON API・CLI・MCP）で共通の、入力の解釈：検索語の分け方と、
//! 日付（日本時間の年月か年月日）の範囲。

use chrono::{DateTime, NaiveDate, Utc};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SearchError {
    #[error("{name} must be YYYY-MM or YYYY-MM-DD, got {value:?}")]
    InvalidDate { name: &'static str, value: String },
}

/// 検索語を空白（全角の空白を含む）で分ける。
pub fn parse_terms(q: &str) -> Vec<String> {
    q.split(char::is_whitespace)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}

/// `since`：その日（年月ならその月の 1 日）の日本時間 0 時。この時刻以降に絞る。
pub fn since(value: &str) -> Result<DateTime<Utc>, SearchError> {
    let (start, _) = period("since", value)?;
    Ok(start)
}

/// `until`：その日（年月ならその月）の終わり。翌日（翌月 1 日）の日本時間 0 時を返し、この時刻より前に絞る。
pub fn until(value: &str) -> Result<DateTime<Utc>, SearchError> {
    let (_, end) = period("until", value)?;
    Ok(end)
}

/// 年月日ならその日、年月ならその月の、日本時間での始まりと終わり（終わりは含まない）。
fn period(name: &'static str, value: &str) -> Result<(DateTime<Utc>, DateTime<Utc>), SearchError> {
    let invalid = || SearchError::InvalidDate {
        name,
        value: value.to_string(),
    };
    let (first, next) = if let Ok(day) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        (day, day.succ_opt())
    } else {
        let month =
            NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d").map_err(|_| invalid())?;
        (month, month.checked_add_months(chrono::Months::new(1)))
    };
    let start = crate::jst::midnight(first).ok_or_else(invalid)?;
    let end = next.and_then(crate::jst::midnight).ok_or_else(invalid)?;
    Ok((start, end))
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
