//! ソースから取得したバイト列を、記事の候補（`Candidate`）の一覧に変換する。
//! ネットワークには触れない。

use chrono::{DateTime, Utc};
use url::Url;

use crate::config::{Filter, SourceKind};

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("failed to parse feed: {0}")]
    Feed(#[from] feed_rs::parser::ParseFeedError),
    #[error("failed to parse json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid date {value:?}: {source}")]
    InvalidDate {
        value: String,
        source: chrono::ParseError,
    },
    #[error("invalid link {href:?}: {source}")]
    InvalidLink {
        href: String,
        source: url::ParseError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub url: String,
    pub title: String,
    pub published_at: Option<DateTime<Utc>>,
    /// フィードの概要（HTML を含むことがある）
    pub summary: Option<String>,
    /// フィードに含まれる本文（content:encoded など）
    pub content: Option<String>,
}

/// `base` は取得元の URL で、相対リンクの解決に使う。
pub fn parse(kind: SourceKind, bytes: &[u8], base: &Url) -> Result<Vec<Candidate>, SourceError> {
    match kind {
        SourceKind::Feed => parse_feed(bytes, base),
        SourceKind::FepcJson => parse_fepc_json(bytes, base),
    }
}

/// RSS 0.9x/1.0/2.0 と Atom。文字コードは XML 宣言に従う（Shift_JIS も可）。
fn parse_feed(bytes: &[u8], base: &Url) -> Result<Vec<Candidate>, SourceError> {
    let feed = feed_rs::parser::Builder::new()
        .base_uri(Some(base.as_str()))
        .build()
        .parse(bytes)?;
    let mut items = Vec::with_capacity(feed.entries.len());
    for entry in feed.entries {
        // 記事は URL で識別するので、リンクの無い項目は保存できない。
        let Some(link) = entry
            .links
            .iter()
            .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
        else {
            tracing::warn!(id = %entry.id, "skipping feed entry without link");
            continue;
        };
        items.push(Candidate {
            url: resolve(base, &link.href)?,
            title: entry.title.map(|t| t.content).unwrap_or_default(),
            published_at: entry.published.or(entry.updated),
            summary: entry.summary.map(|t| t.content),
            content: entry.content.and_then(|c| c.body),
        });
    }
    Ok(items)
}

/// 電事連の `/pr/news/index.json`。一覧ページは JS で描画されるので、元の JSON を読む。
#[derive(serde::Deserialize)]
struct FepcItem {
    title: String,
    href: String,
    /// 例 "2026-9-18"
    date: String,
    category: String,
}

fn parse_fepc_json(bytes: &[u8], base: &Url) -> Result<Vec<Candidate>, SourceError> {
    let jst = chrono::FixedOffset::east_opt(9 * 3600).expect("valid offset");
    let items: Vec<FepcItem> = serde_json::from_slice(bytes)?;
    items
        .into_iter()
        .map(|it| {
            // 日付しか無いので JST の 0 時とみなす。
            let date =
                chrono::NaiveDate::parse_from_str(&it.date, "%Y-%m-%d").map_err(|source| {
                    SourceError::InvalidDate {
                        value: it.date.clone(),
                        source,
                    }
                })?;
            let published_at = date
                .and_hms_opt(0, 0, 0)
                .and_then(|t| t.and_local_timezone(jst).single())
                .map(|t| t.to_utc());
            Ok(Candidate {
                url: resolve(base, &it.href)?,
                title: it.title,
                published_at,
                summary: Some(it.category),
                content: None,
            })
        })
        .collect()
}

fn resolve(base: &Url, href: &str) -> Result<String, SourceError> {
    base.join(href)
        .map(String::from)
        .map_err(|source| SourceError::InvalidLink {
            href: href.to_string(),
            source,
        })
}

/// 絞り込み条件に一致するか。条件が空なら常に一致する。
pub fn matches(filter: &Filter, c: &Candidate) -> bool {
    if filter.keywords.is_empty() && filter.url_contains.is_empty() {
        return true;
    }
    let text_has = |k: &String| {
        c.title.contains(k.as_str()) || c.summary.as_deref().is_some_and(|s| s.contains(k.as_str()))
    };
    filter.keywords.iter().any(text_has)
        || filter
            .url_contains
            .iter()
            .any(|u| c.url.contains(u.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn base(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn utc(s: &str) -> Option<DateTime<Utc>> {
        Some(DateTime::parse_from_rfc3339(s).unwrap().to_utc())
    }

    #[test]
    fn parses_rss2() {
        let items = parse(
            SourceKind::Feed,
            &fixture("rss2.xml"),
            &base("https://regulator.example/rss"),
        )
        .unwrap();
        // リンクの無い項目は保存できないので除く
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0],
            Candidate {
                url: "https://regulator.example/events/1001".into(),
                title: "Power Reactor Event: Unit 2 automatic scram".into(),
                published_at: utc("2026-09-25T18:30:00Z"),
                summary: Some("Power Reactor event notification for Unit 2.".into()),
                content: Some("<p>Full text of the <b>event</b>.</p>".into()),
            }
        );
        // 相対リンクは取得元の URL で解決する
        assert_eq!(items[1].url, "https://regulator.example/events/1002");
        assert_eq!(items[1].published_at, utc("2026-09-24T09:00:00Z"));
        assert_eq!(items[1].content, None);
    }

    #[test]
    fn parses_atom() {
        let items = parse(
            SourceKind::Feed,
            &fixture("atom.xml"),
            &base("https://utility.example/press/atom.xml"),
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "島根原子力発電所2号機の運転状況について");
        assert_eq!(items[0].url, "https://utility.example/press/2026/0920.html");
        assert_eq!(items[0].published_at, utc("2026-09-20T01:00:00Z"));
        assert_eq!(
            items[0].summary.as_deref(),
            Some("2号機は定格熱出力一定運転中です。")
        );
    }

    #[test]
    fn parses_rss1_rdf_with_dc_date() {
        let items = parse(
            SourceKind::Feed,
            &fixture("rdf.xml"),
            &base("https://utility2.example/press.rdf"),
        )
        .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "志賀原子力発電所の安全対策工事の進捗");
        assert_eq!(items[0].published_at, utc("2026-09-18T02:00:00Z"));
    }

    #[test]
    fn decodes_shift_jis_feed() {
        let items = parse(
            SourceKind::Feed,
            &fixture("sjis.xml"),
            &base("https://utility3.example/rss/index.xml"),
        )
        .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "女川原子力発電所2号機の定期検査について");
        assert_eq!(
            items[0].url,
            "https://utility3.example/news/atom/2026/0917.html"
        );
    }

    /// 空の href を解決すると取得元（フィード自体）の URL になり、別の記事と区別できなくなる。
    #[test]
    fn skips_entries_with_blank_link() {
        let xml = br#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>t</title><link>https://e.example/</link><description>d</description>
  <item><title>blank</title><link>  </link></item>
  <item><title>ok</title><link>https://e.example/a</link></item>
</channel></rss>"#;
        let items = parse(SourceKind::Feed, xml, &base("https://e.example/rss")).unwrap();
        let titles: Vec<_> = items.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["ok"]);
    }

    #[test]
    fn skips_fepc_items_with_blank_href() {
        let bytes = br#"[{"title": "blank", "href": "", "date": "2026-9-18", "category": "c"},
                         {"title": "ok", "href": "/a", "date": "2026-9-18", "category": "c"}]"#;
        let items = parse(
            SourceKind::FepcJson,
            bytes,
            &base("https://www.fepc.example/pr/news/index.json"),
        )
        .unwrap();
        let titles: Vec<_> = items.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["ok"]);
    }

    #[test]
    fn parses_fepc_json_as_jst_dates() {
        let items = parse(
            SourceKind::FepcJson,
            &fixture("fepc.json"),
            &base("https://www.fepc.example/pr/news/index.json"),
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0],
            Candidate {
                url: "https://www.fepc.example/pr/news/2026/0918.html".into(),
                title: "会長定例記者会見の概要".into(),
                // 日付だけなので JST の 0 時とみなす
                published_at: utc("2026-09-17T15:00:00Z"),
                summary: Some("会長定例記者会見".into()),
                content: None,
            }
        );
        assert_eq!(items[1].published_at, utc("2026-09-04T15:00:00Z"));
    }

    #[test]
    fn fepc_json_with_bad_date_is_error() {
        let bytes = br#"[{"title": "t", "href": "/a", "date": "2026/9/18", "category": "c"}]"#;
        let err = parse(
            SourceKind::FepcJson,
            bytes,
            &base("https://www.fepc.example/pr/news/index.json"),
        )
        .unwrap_err();
        assert!(matches!(err, SourceError::InvalidDate { .. }), "{err}");
    }

    #[test]
    fn broken_feed_is_error() {
        let err = parse(
            SourceKind::Feed,
            b"<html>not a feed</html>",
            &base("https://e.example/rss"),
        )
        .unwrap_err();
        assert!(matches!(err, SourceError::Feed(_)), "{err}");
    }

    fn candidate(url: &str, title: &str, summary: Option<&str>) -> Candidate {
        Candidate {
            url: url.into(),
            title: title.into(),
            published_at: None,
            summary: summary.map(Into::into),
            content: None,
        }
    }

    #[test]
    fn empty_filter_matches_everything() {
        assert!(matches(
            &Filter::default(),
            &candidate("https://e/a", "料金", None)
        ));
    }

    #[test]
    fn filter_by_keyword_in_title_or_summary() {
        let f = Filter {
            keywords: vec!["原子力".into(), "泊".into()],
            url_contains: vec![],
        };
        assert!(matches(
            &f,
            &candidate("https://e/a", "泊発電所の状況", None)
        ));
        assert!(matches(
            &f,
            &candidate("https://e/a", "お知らせ", Some("原子力部門より"))
        ));
        assert!(!matches(
            &f,
            &candidate("https://e/a", "料金改定", Some("電気料金"))
        ));
    }

    #[test]
    fn filter_by_url_substring() {
        let f = Filter {
            keywords: vec![],
            url_contains: vec!["/news/atom/".into()],
        };
        assert!(matches(
            &f,
            &candidate("https://e/news/atom/1.html", "x", None)
        ));
        assert!(!matches(
            &f,
            &candidate("https://e/news/other/1.html", "x", None)
        ));
    }

    #[test]
    fn filter_matches_if_either_condition_holds() {
        let f = Filter {
            keywords: vec!["原子力".into()],
            url_contains: vec!["/atom/".into()],
        };
        assert!(matches(&f, &candidate("https://e/atom/1", "料金", None)));
        assert!(matches(&f, &candidate("https://e/x/1", "原子力", None)));
        assert!(!matches(&f, &candidate("https://e/x/1", "料金", None)));
    }
}
