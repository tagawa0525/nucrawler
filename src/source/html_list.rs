//! RSS の無いサイトのニュース一覧ページ（HTML）から、記事へのリンクを候補にする。

use chrono::{DateTime, NaiveDate, Utc};
use scraper::{ElementRef, Html, Selector};
use url::Url;

use super::{Candidate, SourceError};
use crate::config::{HtmlList, UrlDate};

/// 一覧ページから、`list.link` に一致するリンクを候補にする。`base` はページの URL。
pub fn parse(list: &HtmlList, html: &str, base: &Url) -> Result<Vec<Candidate>, SourceError> {
    let doc = Html::parse_document(html);
    let link = selector(&list.link)?;
    let skip = list.title_skip.as_deref().map(selector).transpose()?;
    let mut items = Vec::new();
    for a in doc.select(&link) {
        let Some(url) = page_link(a, base) else {
            continue;
        };
        let title = title(a, skip.as_ref());
        if title.is_empty() {
            tracing::debug!(%url, "skipping link without text");
            continue;
        }
        items.push(Candidate {
            published_at: list.date_in_url.and_then(|f| date_in_url(&url, f)),
            url: url.into(),
            title,
            summary: None,
            content: None,
        });
    }
    Ok(items)
}

/// 入口のページで `follow` に一致する最初のリンク（一覧ページ）の URL。
pub fn follow(follow: &str, html: &str, base: &Url) -> Result<Url, SourceError> {
    let doc = Html::parse_document(html);
    doc.select(&selector(follow)?)
        .find_map(|a| page_link(a, base))
        .ok_or_else(|| SourceError::NoFollowLink {
            selector: follow.to_string(),
        })
}

fn selector(s: &str) -> Result<Selector, SourceError> {
    Selector::parse(s).map_err(|e| SourceError::InvalidSelector {
        selector: s.to_string(),
        reason: e.to_string(),
    })
}

/// ページへのリンク（http/https）。ページ内の移動や javascript: などは None。
fn page_link(a: ElementRef, base: &Url) -> Option<Url> {
    let href = a.value().attr("href")?.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let url = base
        .join(href)
        .inspect_err(|e| tracing::debug!(href, "skipping unparsable link: {e}"))
        .ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// リンクの文字列。`skip` に一致する要素は除き、`<br>` は空白にし、連続する空白は 1 つにする
/// （全角の空白は見出しの一部なので残す）。
fn title(a: ElementRef, skip: Option<&Selector>) -> String {
    fn walk(el: ElementRef, skip: Option<&Selector>, out: &mut String) {
        for child in el.children() {
            if let Some(text) = child.value().as_text() {
                out.push_str(text);
            } else if let Some(child) = ElementRef::wrap(child) {
                if skip.is_some_and(|s| s.matches(&child)) {
                    continue;
                }
                if child.value().name() == "br" {
                    out.push(' ');
                } else {
                    walk(child, skip, out);
                }
            }
        }
    }
    let mut raw = String::new();
    walk(a, skip, &mut raw);
    raw.split(|c: char| c.is_ascii_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// URL のファイル名に含まれる日付（日本時間の 0 時）。見つからなければ None。
fn date_in_url(url: &Url, format: UrlDate) -> Option<DateTime<Utc>> {
    let name = url.path_segments()?.next_back()?;
    let len = match format {
        UrlDate::Yyyymmdd => 8,
        UrlDate::Yymmdd => 6,
    };
    let digits = name
        .split(|c: char| !c.is_ascii_digit())
        .find(|run| run.len() == len)?;
    let (year, month_day) = digits.split_at(len - 4);
    let year: i32 = year.parse().ok()?;
    let year = if len == 6 { 2000 + year } else { year };
    let date = NaiveDate::from_ymd_opt(
        year,
        month_day[..2].parse().ok()?,
        month_day[2..].parse().ok()?,
    )?;
    crate::jst::midnight(date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HtmlList, UrlDate};

    fn base(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn jst_midnight(y: i32, m: u32, d: u32) -> Option<chrono::DateTime<chrono::Utc>> {
        crate::jst::midnight(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap())
    }

    fn list(link: &str) -> HtmlList {
        HtmlList {
            link: link.into(),
            date_in_url: None,
            title_skip: None,
            follow: None,
        }
    }

    #[test]
    fn kepco_links_with_date_from_url() {
        let items = parse(
            &HtmlList {
                date_in_url: Some(UrlDate::Yyyymmdd),
                ..list(".prlist_cont dd a")
            },
            include_str!("../../tests/fixtures/kepco_pr.html"),
            &base("https://www.kepco.co.jp/corporate/pr/"),
        )
        .unwrap();
        assert_eq!(items.len(), 3, "{items:#?}");
        assert_eq!(
            items[0].url,
            "https://www.kepco.co.jp/corporate/pr/2026/pdf/20260925_1j.pdf"
        );
        assert!(items[0].title.starts_with("美浜発電所３号機"), "{items:#?}");
        assert_eq!(items[0].published_at, jst_midnight(2026, 9, 25));
        // ナビゲーションのリンクは拾わない
        assert!(items.iter().all(|c| c.url.contains("/corporate/pr/")));
    }

    /// 東電の見出しは会社名のラベルを含み、`<br>` で改行している。
    #[test]
    fn tepco_titles_skip_labels_and_undated_urls_have_no_date() {
        let items = parse(
            &HtmlList {
                date_in_url: Some(UrlDate::Yymmdd),
                title_skip: Some(".doc-add-icon".into()),
                ..list(".news-element-wrapper dd > a")
            },
            include_str!("../../tests/fixtures/tepco_release.html"),
            &base("https://www.tepco.co.jp/press/release/index-j.html"),
        )
        .unwrap();
        assert_eq!(items.len(), 5, "{items:#?}");
        assert!(
            items.iter().all(|c| !c.title.contains("株式会社")),
            "{items:#?}"
        );
        assert_eq!(
            items[1].title,
            "上久屋発電所1号機の営業運転再開について ～リプレース工事完了～"
        );
        assert_eq!(items[0].published_at, jst_midnight(2026, 9, 25));
        // "26x4101.pdf" には日付が無い
        assert_eq!(items[4].published_at, None, "{:?}", items[4]);
    }

    #[test]
    fn japc_follows_the_latest_list_then_trims_titles() {
        let next = follow(
            "#main_cont h3 > a",
            include_str!("../../tests/fixtures/japc_news.html"),
            &base("https://www.japc.co.jp/news/index.html"),
        )
        .unwrap();
        assert_eq!(
            next.as_str(),
            "https://www.japc.co.jp/news/press/2026/index.html"
        );
        let items = parse(
            &HtmlList {
                date_in_url: Some(UrlDate::Yyyymmdd),
                ..list("#main_cont dd > a")
            },
            include_str!("../../tests/fixtures/japc_press.html"),
            &next,
        )
        .unwrap();
        assert_eq!(items.len(), 3, "{items:#?}");
        assert_eq!(items[1].title, "役員人事");
        assert_eq!(items[0].published_at, jst_midnight(2026, 8, 28));
    }

    /// ページ内の見出しへのリンクは記事ではない。記事の URL からはフラグメントを除く。
    #[test]
    fn fragments_are_dropped_and_links_to_the_list_itself_skipped() {
        let html = r##"<ul>
            <li><a href="index.html#top">ページの先頭へ</a></li>
            <li><a href="/news/index.html?#x">一覧</a></li>
            <li><a href="/a.html#section2">記事</a></li>
        </ul>"##;
        let items = parse(
            &list("li a"),
            html,
            &base("https://e.example/news/index.html"),
        )
        .unwrap();
        assert_eq!(
            items.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(),
            ["https://e.example/a.html"]
        );
    }

    #[test]
    fn follow_without_a_match_is_error() {
        let err = follow(
            "#nope a",
            include_str!("../../tests/fixtures/japc_news.html"),
            &base("https://www.japc.co.jp/news/index.html"),
        )
        .unwrap_err();
        assert!(matches!(err, SourceError::NoFollowLink { .. }), "{err}");
    }

    #[test]
    fn skips_links_that_are_not_pages() {
        let html = r##"<ul>
            <li><a href="#">top</a></li>
            <li><a href="javascript:void(0)">menu</a></li>
            <li><a href="mailto:x@example.com">mail</a></li>
            <li><a>no href</a></li>
            <li><a href="/a.html">記事</a></li>
        </ul>"##;
        let items = parse(&list("li a"), html, &base("https://e.example/news/")).unwrap();
        assert_eq!(
            items.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(),
            ["https://e.example/a.html"]
        );
    }
}
