//! ソースから取得したバイト列を、記事の候補（`Candidate`）の一覧に変換する。
//! ネットワークには触れない。

use chrono::{DateTime, Utc};
use url::Url;

use crate::config::{Filter, SourceKind};

pub mod html_list;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("failed to parse feed")]
    Feed(#[from] feed_rs::parser::ParseFeedError),
    #[error("failed to parse json")]
    Json(#[from] serde_json::Error),
    #[error("invalid date {value:?}")]
    InvalidDate {
        value: String,
        source: chrono::ParseError,
    },
    #[error("invalid link {href:?}")]
    InvalidLink {
        href: String,
        source: url::ParseError,
    },
    #[error("invalid css selector {selector:?}: {reason}")]
    InvalidSelector { selector: String, reason: String },
    #[error("no link matches {selector:?}")]
    NoFollowLink { selector: String },
    #[error("html_list needs [source.list]; parse it with html_list::parse")]
    NeedsListSettings,
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
        SourceKind::HtmlList => Err(SourceError::NeedsListSettings),
    }
}

/// RSS 0.9x/1.0/2.0 と Atom。文字コードは XML 宣言に従う（Shift_JIS も可）。
fn parse_feed(bytes: &[u8], base: &Url) -> Result<Vec<Candidate>, SourceError> {
    // 相対リンクは空かどうかを確かめてから自分で解決する（feed-rs に base_uri を渡すと、
    // 空の href が取得元の URL に解決されて区別できなくなる）。
    let feed = feed_rs::parser::parse(bytes)?;
    let mut items = Vec::with_capacity(feed.entries.len());
    for entry in feed.entries {
        // 記事は URL で識別するので、リンクの無い項目は保存できない。
        // 空の href は取得元の URL に解決されてしまうので、無いものとして扱う。
        let Some(link) = entry.links.iter().find(|l| {
            l.rel.as_deref().is_none_or(|r| r == "alternate") && !l.href.trim().is_empty()
        }) else {
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
    /// リンクの無い項目は null（2026-09 時点で約 1 割）
    href: Option<String>,
    /// 例 "2026-9-18"
    date: String,
    category: String,
}

fn parse_fepc_json(bytes: &[u8], base: &Url) -> Result<Vec<Candidate>, SourceError> {
    let items: Vec<FepcItem> = serde_json::from_slice(bytes)?;
    let mut candidates = Vec::with_capacity(items.len());
    for it in items {
        // 空の href は一覧 JSON 自体の URL に解決されてしまうので、無いものとして扱う。
        let Some(href) = it.href.as_deref().filter(|h| !h.trim().is_empty()) else {
            tracing::debug!(title = %it.title, "skipping fepc item without href");
            continue;
        };
        // 日付しか無いので JST の 0 時とみなす。
        let date = chrono::NaiveDate::parse_from_str(&it.date, "%Y-%m-%d").map_err(|source| {
            SourceError::InvalidDate {
                value: it.date.clone(),
                source,
            }
        })?;
        let published_at = crate::jst::midnight(date);
        candidates.push(Candidate {
            url: resolve(base, href)?,
            title: it.title,
            published_at,
            summary: Some(it.category),
            content: None,
        });
    }
    Ok(candidates)
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

    /// 原子力産業新聞（JAIF）の実フィード（2026-09-25 取得、2 件に削ったもの）。
    /// content:encoded に本文があるので、記事ページを取りに行かずに済む。
    #[test]
    fn parses_jaif_journal_feed_with_full_text() {
        let items = parse(
            SourceKind::Feed,
            &fixture("jaif_journal.xml"),
            &base("https://www.jaif.or.jp/journal/feed"),
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "加カメコ　GLEと独占的オフテイク契約を締結");
        assert_eq!(
            items[0].url,
            "https://www.jaif.or.jp/journal/oversea/35993.html"
        );
        assert_eq!(items[0].published_at, utc("2026-09-25T08:14:12Z"));
        assert_eq!(
            items[1].url,
            "https://www.jaif.or.jp/journal/japan/35943.html"
        );

        let body = crate::text::html_to_text(items[0].content.as_deref().unwrap());
        assert!(
            body.contains("パデューカ・レーザー濃縮施設（PLEF）"),
            "{body}"
        );
        // 脚注の本文は残り、脚注プラグインのスクリプトは捨てる
        assert!(
            body.contains("買い手が一定量・一定条件で長期的に引き取る"),
            "{body}"
        );
        assert!(!body.contains("jQuery"), "{body}");
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
        // 実データには href が null の項目がある（2026-09 時点で 994 件中 102 件）
        let bytes = br#"[{"title": "blank", "href": "", "date": "2026-9-18", "category": "c"},
                         {"title": "null", "href": null, "date": "2026-9-18", "category": "c"},
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

    /// 除く語はタイトルだけを見る。取り込む条件が空でも、一致しても、除く語が優先する。
    #[test]
    fn titles_with_excluded_words_are_skipped() {
        let only_excludes = Filter {
            title_excludes: vec!["週報".into()],
            ..Filter::default()
        };
        assert!(!matches(
            &only_excludes,
            &candidate("https://e/a", "原子力機構週報（9/12～9/18）", None)
        ));
        assert!(matches(
            &only_excludes,
            &candidate("https://e/a", "研究成果", Some("詳しくは週報で"))
        ));

        let both = Filter {
            keywords: vec!["原子力".into()],
            url_contains: vec!["/atom/".into()],
            title_excludes: vec!["週報".into()],
        };
        assert!(!matches(
            &both,
            &candidate("https://e/atom/1", "原子力週報", None)
        ));
        assert!(matches(
            &both,
            &candidate("https://e/x/1", "原子力の話", None)
        ));
    }
}
