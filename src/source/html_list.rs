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
    let date = list.date.as_deref().map(selector).transpose()?;
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
        let published_at = date
            .as_ref()
            .and_then(|d| date_in_item(a, &link, d))
            .or_else(|| list.date_in_url.and_then(|f| date_in_url(&url, f)));
        items.push(Candidate {
            published_at,
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

/// ページへのリンク（http/https、フラグメントを除く）。javascript: などと、`base`（一覧ページ
/// 自身）へのリンク（「ページの先頭へ」など）は None。
fn page_link(a: ElementRef, base: &Url) -> Option<Url> {
    let href = a.value().attr("href")?.trim();
    if href.is_empty() {
        return None;
    }
    let url = base
        .join(href)
        .inspect_err(|e| tracing::debug!(href, "skipping unparsable link: {e}"))
        .ok()?;
    let url = without_fragment(url);
    (matches!(url.scheme(), "http" | "https") && url != without_fragment(base.clone()))
        .then_some(url)
}

/// フラグメントと空のクエリ（`?` だけ）を除いた URL。
fn without_fragment(mut url: Url) -> Url {
    url.set_fragment(None);
    if url.query() == Some("") {
        url.set_query(None);
    }
    url
}

/// リンクの文字列。script などの画面に出ない要素と `skip` に一致する要素は除き、`<br>` は空白にし、連続する空白は 1 つにする
/// （全角の空白は見出しの一部なので残す）。
fn title(a: ElementRef, skip: Option<&Selector>) -> String {
    fn walk(el: ElementRef, skip: Option<&Selector>, out: &mut String) {
        for child in el.children() {
            if let Some(text) = child.value().as_text() {
                out.push_str(text);
            } else if let Some(child) = ElementRef::wrap(child) {
                // 画面に出ない要素と、設定で除く要素の中身は入れない
                let hidden = matches!(
                    child.value().name(),
                    "script" | "style" | "noscript" | "template"
                );
                if hidden || skip.is_some_and(|s| s.matches(&child)) {
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

/// リンク `a` を含む項目の中の、`date` に一致する最初の要素が表す日付（日本時間の 0 時）。
/// 項目は、`a` 自身から祖先を内側へたどり、ほかのリンク（`link` に一致する要素）を含む手前まで
/// （項目全体がリンクで、日付がリンクの中にあるページもある）。
/// 項目に日付の要素が無いときや読めないときは None（隣の項目の日付を使わない）。
fn date_in_item(a: ElementRef, link: &Selector, date: &Selector) -> Option<DateTime<Utc>> {
    fn matching<'a>(scope: ElementRef<'a>, sel: &Selector) -> impl Iterator<Item = ElementRef<'a>> {
        scope
            .descendants()
            .filter_map(ElementRef::wrap)
            .filter(move |el| sel.matches(el))
    }
    for scope in std::iter::once(a).chain(a.ancestors().filter_map(ElementRef::wrap)) {
        if matching(scope, link).nth(1).is_some() {
            return None;
        }
        if let Some(el) = matching(scope, date).next() {
            let text = el.text().collect::<String>();
            let parsed = date_in_text(&text);
            if parsed.is_none() {
                tracing::debug!(text = text.trim(), "skipping unreadable date");
            }
            return parsed;
        }
    }
    None
}

/// 文字列の中の、年（4 桁）・月・日の順に並ぶ最初の数字（日本時間の 0 時）。
fn date_in_text(text: &str) -> Option<DateTime<Utc>> {
    let mut runs = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty());
    let year = runs.find(|run| run.len() == 4)?.parse().ok()?;
    let month = runs.next()?.parse().ok()?;
    let day = runs.next()?.parse().ok()?;
    crate::jst::midnight(NaiveDate::from_ymd_opt(year, month, day)?)
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
            date: None,
            also: vec![],
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

    /// 日本原燃の一覧は月ごとの表。日付はファイル名（20260820-1.html）から取り、年度のリンクは拾わない。
    #[test]
    fn jnfl_press_links_with_date_from_url() {
        let items = parse(
            &HtmlList {
                date_in_url: Some(UrlDate::Yyyymmdd),
                ..list("table.list td a")
            },
            include_str!("../../tests/fixtures/jnfl_press.html"),
            &base("https://www.jnfl.co.jp/ja/release/press/"),
        )
        .unwrap();
        assert_eq!(items.len(), 3, "{items:#?}");
        assert_eq!(
            items[0].url,
            "https://www.jnfl.co.jp/ja/release/press/2026/detail/20260820-1.html"
        );
        assert_eq!(
            items[2].title,
            "六ヶ所ウラン濃縮工場に係る新増設等計画書に関する事前了解について"
        );
        assert_eq!(items[0].published_at, jst_midnight(2026, 8, 20));
        assert_eq!(items[2].published_at, jst_midnight(2026, 7, 24));
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

    /// 規制委の一覧は日付を dt に書く。URL の数字（元の募集の日付など）は公表日ではない。
    #[test]
    fn nra_links_with_date_from_the_list() {
        let items = parse(
            &HtmlList {
                date: Some(".news__date".into()),
                ..list("dl.news__list dd.news__title a")
            },
            include_str!("../../tests/fixtures/nra_news.html"),
            &base("https://www.nra.go.jp/news/index.html"),
        )
        .unwrap();
        assert_eq!(items.len(), 4, "{items:#?}");
        assert_eq!(
            items[1].url,
            "https://www.nra.go.jp/news_only/20260917_ILC.html"
        );
        assert_eq!(
            items[1].title,
            "国際原子力機関(IAEA)と共同で実施した分析機関間比較(ILC2024)の報告書の公表"
        );
        assert_eq!(items[0].published_at, jst_midnight(2026, 9, 18));
        // URL は 20230918 を含むが、公表日は 2026 年 9 月 15 日
        assert_eq!(items[2].published_at, jst_midnight(2026, 9, 15));
    }

    /// 規制委のトップの新着情報は、月をまたいで最新の 5 件を載せる（2026 年 9 月 2 日の実ページ）。
    /// 新着履歴と同じ読み方で読める。
    #[test]
    fn nra_top_page_lists_the_latest_items_across_months() {
        let items = parse(
            &HtmlList {
                date: Some(".news__date".into()),
                ..list("dl.news__list dd.news__title a")
            },
            include_str!("../../tests/fixtures/nra_top.html"),
            &base("https://www.nra.go.jp/"),
        )
        .unwrap();
        assert_eq!(items.len(), 5, "{items:#?}");
        assert_eq!(
            items[0].url,
            "https://www.nra.go.jp/news_only/20251114.html"
        );
        assert_eq!(items[0].published_at, jst_midnight(2026, 9, 2));
        assert_eq!(
            items[1].title,
            "令和9年度概算要求及び機構・定員要求のHP掲載について"
        );
        assert_eq!(items[1].published_at, jst_midnight(2026, 8, 31));
        assert_eq!(items[4].published_at, jst_midnight(2026, 8, 13));
    }

    /// 月（年度）が替わった直後の新着履歴は項目が無い（2026 年 4 月 1 日の実ページ）。
    /// 項目が無いのはエラーではない。
    #[test]
    fn nra_month_list_without_items_is_empty() {
        let items = parse(
            &HtmlList {
                date: Some(".news__date".into()),
                ..list("dl.news__list dd.news__title a")
            },
            include_str!("../../tests/fixtures/nra_news_empty.html"),
            &base("https://www.nra.go.jp/news/index.html"),
        )
        .unwrap();
        assert!(items.is_empty(), "{items:#?}");
    }

    /// 原子力機構のトップの新着は種類ごとに印が付く。プレス発表だけを、dt の日付で取る。
    #[test]
    fn jaea_press_links_with_date_from_the_list() {
        let items = parse(
            &HtmlList {
                date: Some("dt".into()),
                ..list(r#"li[data-info-category="newsPress"] dd a"#)
            },
            include_str!("../../tests/fixtures/jaea_top.html"),
            &base("https://www.jaea.go.jp/"),
        )
        .unwrap();
        assert_eq!(items.len(), 4, "{items:#?}");
        assert!(
            items.iter().all(|c| c.url.contains("/02/press2026/")),
            "{items:#?}"
        );
        assert_eq!(items[0].title, "原子力機構週報（9/12～9/18）");
        assert_eq!(items[0].published_at, jst_midnight(2026, 9, 18));
        assert_eq!(items[3].published_at, jst_midnight(2026, 8, 7));
    }

    /// 規制委の資料公開（N-ADRES）のトップの最新情報は、項目全体がリンクで、日付と資料の分類も
    /// リンクの中にある（2026 年 10 月 4 日の実ページ）。
    #[test]
    fn nra_archive_dates_inside_the_link() {
        let items = parse(
            &HtmlList {
                date: Some(".p-top-latest__date".into()),
                title_skip: Some(".p-top-latest__date, .p-top-latest__category".into()),
                ..list("a.p-top-latest__item")
            },
            include_str!("../../tests/fixtures/nra_archive_top.html"),
            &base("https://www.da.nra.go.jp/"),
        )
        .unwrap();
        assert_eq!(items.len(), 4, "{items:#?}");
        assert_eq!(items[0].url, "https://www.da.nra.go.jp/detail/NRA100020538");
        assert_eq!(
            items[1].title,
            "第1437回原子力発電所の新規制基準適合性に係る審査会合 令和8年10月02日"
        );
        assert_eq!(items[0].published_at, jst_midnight(2026, 10, 2));
        assert_eq!(items[2].published_at, jst_midnight(2026, 9, 30));
        assert_eq!(items[3].published_at, jst_midnight(2026, 9, 28));
    }

    /// 日付は、リンクを含む項目（ほかのリンクを含まない最も大きいまとまり）の中から探す。
    /// 項目に日付が無いときや読めないときは、隣の項目の日付を使わずに None にする。
    #[test]
    fn dates_come_only_from_the_item_of_the_link() {
        let html = r#"<ul>
            <li><span class="d">2026/09/07：</span><p><a href="/a.html">A</a></p></li>
            <li><a href="/b.html">B</a></li>
            <li><span class="d">日付未定</span><a href="/c.html">C</a></li>
        </ul>"#;
        let items = parse(
            &HtmlList {
                date: Some(".d".into()),
                ..list("li a")
            },
            html,
            &base("https://e.example/"),
        )
        .unwrap();
        assert_eq!(
            items.iter().map(|c| c.published_at).collect::<Vec<_>>(),
            [jst_midnight(2026, 9, 7), None, None]
        );
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

    /// 画面に出ない要素（script など）の中身は見出しに入れない。
    #[test]
    fn titles_ignore_non_rendered_elements() {
        let html = r#"<a href="/a.html">見出し<script>var x = 1;</script><style>.x{}</style><noscript>JS</noscript><template>t</template></a>"#;
        let items = parse(&list("a"), html, &base("https://e.example/")).unwrap();
        assert_eq!(items[0].title, "見出し");
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
