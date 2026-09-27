//! 検索の入口（Web・JSON API・CLI・MCP）で共通の、入力の解釈：検索語の分け方と、
//! 日付（日本時間の年月か年月日）の範囲。

use chrono::{DateTime, NaiveDate, Utc};

use crate::config::Lang;
use crate::db::{SearchOrder, SearchQuery};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SearchError {
    #[error("{name} must be YYYY-MM or YYYY-MM-DD, got {value:?}")]
    InvalidDate { name: &'static str, value: String },
    #[error("lang must be en or ja, got {0:?}")]
    InvalidLang(String),
    #[error("min_score must be 0..=100, got {0:?}")]
    InvalidScore(String),
    #[error("sort must be newest or score, got {0:?}")]
    InvalidSort(String),
}

/// 検索画面・JSON API・CLI の条件。名前はクエリ文字列のキーと同じ（`topic` と `source` は繰り返せる）。
/// 値は入力のまま持ち、`to_query` で解釈する。空の値は指定しなかったものとして扱う。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params {
    pub q: String,
    pub since: String,
    pub until: String,
    pub topics: Vec<String>,
    pub sources: Vec<String>,
    pub lang: String,
    pub translated: bool,
    pub liked: bool,
    pub unread: bool,
    pub min_score: String,
    /// `newest`（既定）か `score`
    pub sort: String,
}

impl Params {
    /// クエリ文字列（`?` を除く）から読む。知らないキーは無視する。真偽は `1` のときだけ真。
    pub fn from_query(_raw: &str) -> Self {
        Self::default()
    }

    /// 条件が 1 つも無い（検索画面では結果を出さず、フォームだけを出す）。
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn to_query<'a>(
        &self,
        _user_id: i64,
        _profile_hash: Option<&'a str>,
        _limit: usize,
    ) -> Result<SearchQuery<'a>, SearchError> {
        Ok(SearchQuery::default())
    }
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
    fn reads_params_from_a_query_string() {
        let p = Params::from_query(
            "q=%E7%82%89%E5%BF%83+NRC&topic=%E7%87%83%E6%96%99&topic=PWR&source=nra&source=wnn\
             &since=2026-09&until=&lang=ja&translated=1&liked=0&unread=on&min_score=60&sort=score&x=1",
        );
        assert_eq!(
            p,
            Params {
                q: "炉心 NRC".into(),
                since: "2026-09".into(),
                topics: vec!["燃料".into(), "PWR".into()],
                sources: vec!["nra".into(), "wnn".into()],
                lang: "ja".into(),
                translated: true,
                min_score: "60".into(),
                sort: "score".into(),
                ..Params::default()
            }
        );
        assert!(!p.is_empty());
        assert!(Params::from_query("").is_empty());
        assert!(
            Params::from_query("q=&since=&sort=score").is_empty(),
            "sort alone is not a condition"
        );
    }

    #[test]
    fn builds_a_search_query() {
        let p = Params {
            q: "炉心　NRC".into(),
            since: "2026-09".into(),
            until: "2026-09-20".into(),
            topics: vec!["燃料".into()],
            sources: vec!["nra".into()],
            lang: "en".into(),
            translated: true,
            liked: true,
            unread: true,
            min_score: "60".into(),
            sort: "score".into(),
        };
        let q = p.to_query(7, Some("h"), 30).unwrap();
        assert_eq!((q.user_id, q.profile_hash, q.limit), (7, Some("h"), 30));
        assert_eq!(q.terms, ["炉心", "NRC"]);
        assert_eq!(q.since, Some(utc("2026-08-31T15:00:00Z")));
        assert_eq!(q.until, Some(utc("2026-09-20T15:00:00Z")));
        assert_eq!(q.topics, ["燃料"]);
        assert_eq!(q.sources, ["nra"]);
        assert_eq!(q.lang, Some(Lang::En));
        assert!(q.translated && q.liked && q.unread);
        assert_eq!(q.min_score, Some(60));
        assert_eq!(q.order, SearchOrder::Score);
        assert_eq!(q.hide_below, None, "search shows what the list hides");

        let empty = Params::default().to_query(7, None, 30).unwrap();
        assert!(empty.terms.is_empty() && empty.since.is_none() && empty.until.is_none());
        assert_eq!(
            (empty.lang, empty.min_score, empty.order),
            (None, None, SearchOrder::Newest)
        );
    }

    #[test]
    fn rejects_invalid_params() {
        let err = |p: Params| p.to_query(1, None, 10).unwrap_err();
        assert!(matches!(
            err(Params {
                since: "x".into(),
                ..Params::default()
            }),
            SearchError::InvalidDate { name: "since", .. }
        ));
        assert!(matches!(
            err(Params {
                until: "2026-13".into(),
                ..Params::default()
            }),
            SearchError::InvalidDate { name: "until", .. }
        ));
        assert_eq!(
            err(Params {
                lang: "fr".into(),
                ..Params::default()
            }),
            SearchError::InvalidLang("fr".into())
        );
        assert_eq!(
            err(Params {
                min_score: "101".into(),
                ..Params::default()
            }),
            SearchError::InvalidScore("101".into())
        );
        assert_eq!(
            err(Params {
                min_score: "x".into(),
                ..Params::default()
            }),
            SearchError::InvalidScore("x".into())
        );
        assert_eq!(
            err(Params {
                sort: "old".into(),
                ..Params::default()
            }),
            SearchError::InvalidSort("old".into())
        );
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
