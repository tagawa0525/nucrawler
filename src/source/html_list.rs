//! RSS の無いサイトのニュース一覧ページ（HTML）から、記事へのリンクを候補にする。

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
