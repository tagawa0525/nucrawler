//! 日本時間。表示や日付だけの入力（"2026-9-18" など）の解釈に使う。DB には UTC で書く。

use chrono::{DateTime, FixedOffset, Utc};

pub fn offset() -> FixedOffset {
    FixedOffset::east_opt(9 * 3600).expect("+09:00 is a valid offset")
}

/// DB の UTC 時刻（RFC 3339）を日本時間の "YYYY-MM-DD HH:MM" にする。解釈できなければそのまま。
pub fn format_local(utc: &str) -> String {
    DateTime::parse_from_rfc3339(utc).map_or_else(
        |_| utc.to_string(),
        |t| {
            t.with_timezone(&offset())
                .format("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
}

/// 日付を、日本時間のその日の 0 時（UTC）にする。
pub fn midnight(date: chrono::NaiveDate) -> Option<DateTime<Utc>> {
    date.and_hms_opt(0, 0, 0)?
        .and_local_timezone(offset())
        .single()
        .map(|t| t.to_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_in_jst() {
        assert_eq!(format_local("2026-09-25T18:30:00.000Z"), "2026-09-26 03:30");
        assert_eq!(format_local("not a time"), "not a time");
        let d = chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        assert_eq!(
            midnight(d).unwrap().to_rfc3339(),
            "2026-09-19T15:00:00+00:00"
        );
    }
}
