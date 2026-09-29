//! 検索の入口（Web・JSON API・CLI・MCP）で共通の、入力の解釈：検索語の分け方と、
//! 日付（日本時間の年・年月・年月日）の範囲。

use chrono::{DateTime, NaiveDate, Utc};

use crate::config::Lang;
use crate::db::{Rating, SearchOrder, SearchQuery};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SearchError {
    #[error("{name} must be YYYY, YYYY-MM or YYYY-MM-DD, got {value:?}")]
    InvalidDate { name: &'static str, value: String },
    #[error("lang must be en or ja, got {0:?}")]
    InvalidLang(String),
    #[error("min_score must be 0..=100, got {0:?}")]
    InvalidScore(String),
    #[error("min_rating must be 1..=5, got {0:?}")]
    InvalidRating(String),
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
    pub unread: bool,
    pub bookmarked: bool,
    /// 評価の無い記事だけ
    pub unrated: bool,
    /// この評価（1〜5）以上
    pub min_rating: String,
    pub min_score: String,
    /// `newest`（既定）か `score`
    pub sort: String,
}

impl Params {
    /// クエリ文字列（`?` を除く）から読む。知らないキーは無視する。真偽は `1` のときだけ真。
    pub fn from_query(raw: &str) -> Self {
        let mut p = Self::default();
        for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
            let value = value.into_owned();
            match key.as_ref() {
                "q" => p.q = value,
                "since" => p.since = value,
                "until" => p.until = value,
                "topic" if !value.trim().is_empty() => p.topics.push(value),
                "source" if !value.trim().is_empty() => p.sources.push(value),
                "lang" => p.lang = value,
                "translated" => p.translated = value == "1",
                "unread" => p.unread = value == "1",
                "bookmarked" => p.bookmarked = value == "1",
                "unrated" => p.unrated = value == "1",
                "min_rating" => p.min_rating = value,
                "min_score" => p.min_score = value,
                "sort" => p.sort = value,
                _ => {}
            }
        }
        p
    }

    /// 条件が 1 つも無い（検索画面では結果を出さず、フォームだけを出す）。並びは条件に数えない。
    pub fn is_empty(&self) -> bool {
        [
            &self.q,
            &self.since,
            &self.until,
            &self.lang,
            &self.min_rating,
            &self.min_score,
        ]
        .iter()
        .all(|v| v.trim().is_empty())
            && self.topics.is_empty()
            && self.sources.is_empty()
            && !(self.translated || self.unread || self.bookmarked || self.unrated)
    }

    /// 検索の条件にする。一覧で隠す記事も含める。
    pub fn to_query<'a>(
        &self,
        user_id: i64,
        profile_hash: Option<&'a str>,
        limit: usize,
    ) -> Result<SearchQuery<'a>, SearchError> {
        let lang = match given(&self.lang) {
            None => None,
            Some("en") => Some(Lang::En),
            Some("ja") => Some(Lang::Ja),
            Some(other) => return Err(SearchError::InvalidLang(other.to_string())),
        };
        let min_score = given(&self.min_score)
            .map(|v| {
                v.parse::<u8>()
                    .ok()
                    .filter(|score| *score <= 100)
                    .ok_or_else(|| SearchError::InvalidScore(v.to_string()))
            })
            .transpose()?;
        let min_rating = given(&self.min_rating)
            .map(|v| {
                v.parse::<u8>()
                    .ok()
                    .and_then(Rating::new)
                    .ok_or_else(|| SearchError::InvalidRating(v.to_string()))
            })
            .transpose()?;
        let order = match given(&self.sort) {
            None | Some("newest") => SearchOrder::Newest,
            Some("score") => SearchOrder::Score,
            Some(other) => return Err(SearchError::InvalidSort(other.to_string())),
        };
        Ok(SearchQuery {
            user_id,
            profile_hash,
            terms: parse_terms(&self.q),
            since: given(&self.since).map(since).transpose()?,
            until: given(&self.until).map(until).transpose()?,
            topics: self.topics.clone(),
            sources: self.sources.clone(),
            lang,
            translated: self.translated,
            unread: self.unread,
            bookmarked: self.bookmarked,
            unrated: self.unrated,
            min_rating,
            min_score,
            hide_below: None,
            order,
            limit,
        })
    }
}

/// 検索語を空白（全角の空白を含む）で分ける。
pub fn parse_terms(q: &str) -> Vec<String> {
    q.split(char::is_whitespace)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}

/// `since`：その日（年月ならその月の 1 日、年ならその年の 1 月 1 日）の日本時間 0 時。この時刻以降に絞る。
pub fn since(value: &str) -> Result<DateTime<Utc>, SearchError> {
    let (start, _) = period("since", value)?;
    Ok(start)
}

/// `until`：その日（年月ならその月、年ならその年）の終わり。翌日（翌月・翌年の 1 日）の日本時間 0 時を返し、
/// この時刻より前に絞る。
pub fn until(value: &str) -> Result<DateTime<Utc>, SearchError> {
    let (_, end) = period("until", value)?;
    Ok(end)
}

/// CLI の結果の 1 行：公開日時（日本時間）、点数（未採点は -）、見出し、URL。
pub fn result_line(item: &crate::db::ListItem) -> String {
    let score = item
        .score
        .map_or_else(|| "-".to_string(), |s| s.to_string());
    // 取得した題名には改行が混ざりうるので、空白をまとめて 1 行にする
    let title = crate::web::html::display_title(item.title_ja.as_deref(), item)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{}  {score:>3}  {title}  {}",
        crate::jst::format_local(&item.at),
        item.url
    )
}

/// 入力された値（前後の空白を除く）。空なら指定しなかったもの。
fn given(value: &str) -> Option<&str> {
    Some(value.trim()).filter(|v| !v.is_empty())
}

/// 年月日ならその日、年月ならその月、年ならその年の、日本時間での始まりと終わり（終わりは含まない）。
fn period(name: &'static str, value: &str) -> Result<(DateTime<Utc>, DateTime<Utc>), SearchError> {
    let invalid = || SearchError::InvalidDate {
        name,
        value: value.to_string(),
    };
    let (first, next) = if let Ok(day) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        (day, day.succ_opt())
    } else if value.len() == 4 && value.bytes().all(|b| b.is_ascii_digit()) {
        let year = NaiveDate::parse_from_str(&format!("{value}-01-01"), "%Y-%m-%d")
            .map_err(|_| invalid())?;
        (year, year.checked_add_months(chrono::Months::new(12)))
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
             &since=2026-09&until=&lang=ja&translated=1&min_rating=4&unread=on&bookmarked=1&min_score=60&sort=score&x=1",
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
                bookmarked: true,
                min_rating: "4".into(),
                min_score: "60".into(),
                sort: "score".into(),
                ..Params::default()
            }
        );
        assert!(!p.is_empty());
        assert!(Params::from_query("").is_empty());
        assert!(!Params::from_query("bookmarked=1").is_empty());
        assert!(!Params::from_query("min_rating=4").is_empty());
        // 評価の無い記事だけ
        assert!(Params::from_query("unrated=1").unrated);
        assert!(!Params::from_query("unrated=1").is_empty());
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
            unread: true,
            bookmarked: true,
            unrated: true,
            min_rating: "4".into(),
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
        assert!(q.translated && q.unread && q.bookmarked && q.unrated);
        assert_eq!(q.min_rating, crate::db::Rating::new(4));
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
        for bad in ["0", "6", "x"] {
            assert_eq!(
                err(Params {
                    min_rating: bad.into(),
                    ..Params::default()
                }),
                SearchError::InvalidRating(bad.into())
            );
        }
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
    fn formats_result_lines() {
        let mut item = crate::db::ListItem {
            article_id: 1,
            source_id: "wnn".into(),
            url: "https://e.com/1".into(),
            title: "Original".into(),
            lang: "en".into(),
            at: "2026-09-26T00:00:00.000Z".into(),
            fetched_at: "2026-09-26T00:00:00.000Z".into(),
            title_ja: Some("見出し".into()),
            summary_ja: None,
            lwr_relevant: Some(true),
            score: Some(80),
            llm_score: Some(80),
            reason: None,
            matched: Vec::new(),
            excluded: Vec::new(),
            read_at: None,
            rating: None,
            has_translation: false,
            translation_requested: false,
            bookmarked: false,
            locked_by: vec![],
        };
        assert_eq!(
            result_line(&item),
            "2026-09-26 09:00   80  見出し  https://e.com/1"
        );
        item.score = None;
        item.title_ja = None;
        assert_eq!(
            result_line(&item),
            "2026-09-26 09:00    -  Original  https://e.com/1"
        );
        // 題名の改行やタブで 1 件が複数行に割れないようにする
        item.title = "Line\r\none\ttwo  ".into();
        assert_eq!(
            result_line(&item),
            "2026-09-26 09:00    -  Line one two  https://e.com/1"
        );
    }

    #[test]
    fn splits_terms_on_any_whitespace() {
        assert_eq!(parse_terms(" 炉心　溶融  NRC\t"), ["炉心", "溶融", "NRC"]);
        assert!(parse_terms("　 ").is_empty());
    }

    #[test]
    fn reads_days_months_and_years_in_jst() {
        assert_eq!(since("2026-09-20").unwrap(), utc("2026-09-19T15:00:00Z"));
        assert_eq!(until("2026-09-20").unwrap(), utc("2026-09-20T15:00:00Z"));
        assert_eq!(since("2026-09").unwrap(), utc("2026-08-31T15:00:00Z"));
        assert_eq!(until("2026-09").unwrap(), utc("2026-09-30T15:00:00Z"));
        assert_eq!(until("2026-12").unwrap(), utc("2026-12-31T15:00:00Z"));
        assert_eq!(since("2026").unwrap(), utc("2025-12-31T15:00:00Z"));
        assert_eq!(until("2026").unwrap(), utc("2026-12-31T15:00:00Z"));
    }

    #[test]
    fn rejects_malformed_dates() {
        for bad in [
            "2026/09/01",
            "2026-13",
            "2026-02-30",
            "",
            "26",
            "20260",
            "+2026",
        ] {
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
