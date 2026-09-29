//! 検索の画面。

use super::*;

/// 検索画面。`results` が None なら（条件が無いときは）フォームだけを出す。
/// トピックの選択肢は要約に付いている語だけを、軸ごとに付いている数の多い順に並べる。
pub fn search_page(
    params: &Params,
    results: Option<&[ListItem]>,
    vocabulary: &[TopicUsage],
    error: Option<&str>,
    page: &Page,
) -> String {
    // 上部は一覧と同じバー（先頭の 🏠 で一覧へ戻る）
    let mut body = super::list::home_bar(page);
    body.push_str("<h1>検索</h1>");
    if let Some(error) = error {
        body.push_str(&format!("<div class=\"warn\">{}</div>", escape(error)));
    }
    body.push_str(&search_form(params, vocabulary, page));
    body.push_str(CALENDAR_SCRIPT);
    if let Some(items) = results {
        if items.is_empty() {
            body.push_str("<p class=\"meta\">該当する記事はありません</p>");
        } else {
            body.push_str(&format!("<h2>{} 件</h2>", items.len()));
            body.extend(items.iter().map(|i| card(i, false, page)));
        }
    }
    layout("検索", page, &body)
}

fn search_form(p: &Params, vocabulary: &[TopicUsage], page: &Page) -> String {
    let text = |name: &str, value: &str, extra: &str| {
        format!("<input name=\"{name}\" value=\"{}\"{extra}>", escape(value))
    };
    let checkbox = |name: &str, value: &str, checked: bool, label: &str| {
        format!(
            "<label><input type=\"checkbox\" name=\"{name}\" value=\"{}\"{}> {}</label> ",
            escape(value),
            if checked { " checked" } else { "" },
            escape(label)
        )
    };
    let select = |name: &str, current: &str, options: &[(&str, &str)]| {
        let options: String = options
            .iter()
            .map(|(value, label)| {
                let selected = if *value == current { " selected" } else { "" };
                format!("<option value=\"{value}\"{selected}>{label}</option>")
            })
            .collect();
        format!("<select name=\"{name}\">{options}</select>")
    };
    // 要約に付いている語だけを、軸ごとに付いている数の多い順に（選んだ語は数によらず出す）
    let mut topics = String::new();
    for facet in crate::topics::Facet::ALL {
        let mut words: Vec<&TopicUsage> = vocabulary
            .iter()
            .filter(|u| u.facet == facet && (u.uses > 0 || p.topics.contains(&u.name)))
            .collect();
        words.sort_by_key(|u| std::cmp::Reverse(u.uses));
        if words.is_empty() {
            continue;
        }
        topics.push_str(&format!("<div class=\"meta\">{}</div>", facet.as_str()));
        for u in words {
            let label = format!("{} ({})", u.name, u.uses);
            topics.push_str(&checkbox(
                "topic",
                &u.name,
                p.topics.contains(&u.name),
                &label,
            ));
        }
    }
    // 語彙に無い語（統合した語の別名など）も、選んでいれば残す
    let others: Vec<&String> = p
        .topics
        .iter()
        .filter(|t| vocabulary.iter().all(|u| &u.name != *t))
        .collect();
    if !others.is_empty() {
        topics.push_str("<div class=\"meta\">その他</div>");
        for t in others {
            topics.push_str(&checkbox("topic", t, true, t));
        }
    }
    let sources: String = page
        .labels
        .iter()
        .map(|(id, label)| checkbox("source", id, p.sources.contains(id), label))
        .collect();
    let open = |any: bool| if any { " open" } else { "" };
    format!(
        "<form method=\"get\" action=\"/search\">\
         <p>{q}</p>\
         <p>期間 {since}{since_cal} 〜 {until}{until_cal}</p>\
         <details{topics_open}><summary>トピック</summary>{topics}</details>\
         <details{sources_open}><summary>ソース</summary>{sources}</details>\
         <p>言語 {lang} 並び {sort}</p>\
         <p>{translated}{unread}{bookmarked}{unrated}評価 {min_rating} 最低点 {min_score}</p>\
         <p><button type=\"submit\">検索</button></p></form>",
        q = text(
            "q",
            &p.q,
            " class=\"wide\" type=\"search\" placeholder=\"語（空白で区切るとすべてを含む）\""
        ),
        since = text("since", &p.since, " size=\"10\" placeholder=\"2026-09\""),
        until = text("until", &p.until, " size=\"10\" placeholder=\"2026-09-30\""),
        since_cal = calendar("since", "開始日"),
        until_cal = calendar("until", "終了日"),
        topics_open = open(!p.topics.is_empty()),
        sources_open = open(!p.sources.is_empty()),
        lang = select(
            "lang",
            &p.lang,
            &[("", "すべて"), ("en", "英語"), ("ja", "日本語")]
        ),
        sort = select(
            "sort",
            &p.sort,
            &[("newest", "新しい順"), ("score", "点数順")]
        ),
        translated = checkbox("translated", "1", p.translated, "和訳あり"),
        unread = checkbox("unread", "1", p.unread, "未読"),
        bookmarked = checkbox("bookmarked", "1", p.bookmarked, "🔖"),
        unrated = checkbox("unrated", "1", p.unrated, "評価なし"),
        min_rating = select(
            "min_rating",
            &p.min_rating,
            &[
                ("", "問わない"),
                ("5", "★5"),
                ("4", "★4 以上"),
                ("3", "★3 以上"),
                ("2", "★2 以上"),
                ("1", "★1 以上"),
            ]
        ),
        min_score = text(
            "min_score",
            &p.min_score,
            " type=\"number\" min=\"0\" max=\"100\" size=\"3\""
        ),
    )
}

/// 期間の欄の横の 📅。日付の入力を透明にして絵文字に重ね、押すとカレンダーが開く
/// （`CALENDAR_SCRIPT`）。名前を持たないので送られず、選んだ日付は `name` の欄へ入る。
/// キーボードでも操作できるよう、日付の入力はフォーカスでき、読み上げの名前を持つ。
fn calendar(name: &str, label: &str) -> String {
    format!(
        "<label class=\"cal\" title=\"カレンダー\">📅<input type=\"date\" data-for=\"{name}\" \
         aria-label=\"{label}をカレンダーで選ぶ\"></label>"
    )
}

/// 📅 のカレンダーで選んだ日付を、隣の期間の欄に入れる。欄は月だけの指定もできるよう文字の
/// 入力のまま残す。欄が日付ならその日から、そうでなければ（月だけや空なら）今日から開く。
const CALENDAR_SCRIPT: &str = concat!(
    "<script>\n",
    include_str!("assets/calendar.js"),
    "</script>"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::html::test_support::*;

    fn usage(name: &str, facet: crate::topics::Facet, uses: i64) -> TopicUsage {
        TopicUsage {
            name: name.into(),
            facet,
            added_at: None,
            uses,
        }
    }

    /// 検索の画面にも一覧と同じ上部のバーを出す。先頭は 🔍 ではなく、一覧へ戻る 🏠。
    #[test]
    fn search_page_shows_the_list_bar_with_home() {
        let html = search_page(&Params::default(), None, &[], None, &Page::default());
        assert!(
            html.contains(r#"<nav class="bar"><a class="btn" href="/" aria-label="ホーム" title="ホーム">🏠</a>"#),
            "{html}"
        );
        assert!(!html.contains(r#"aria-label="検索""#), "{html}");
        assert!(!html.contains("一覧へ"), "{html}");
        assert!(
            html.contains(r#"name="rating""#) && html.contains("既読も表示"),
            "{html}"
        );
    }

    /// 検索の画面のバーは、点数・評価・既読・ブックマークの検索の条件を持つ（フォームには重ねない）。
    /// バーを変えるとほかの条件はそのままに検索し直し、フォームで送るときはバーの条件を引き継ぐ。
    /// 👁 は既定で ON（既読も出す）、OFF で未読だけ。評価の ☆ は評価の無い記事だけ。
    #[test]
    fn search_bar_holds_the_mark_conditions() {
        let params = Params {
            q: "炉心".into(),
            unread: true,
            min_rating: "4".into(),
            min_score: "60".into(),
            ..Params::default()
        };
        let html = search_page(&params, Some(&[]), &[], None, &Page::default());
        let bar = html.split("</nav>").next().unwrap();
        assert!(
            bar.contains(r#"<form class="min on" method="get" action="/search">"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<option value="60" data-href="/search?q=%E7%82%89%E5%BF%83&amp;unread=1&amp;min_rating=4&amp;min_score=60" selected>60</option>"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<option value="0" data-href="/search?q=%E7%82%89%E5%BF%83&amp;unread=1&amp;min_rating=4" data-closed="00">-</option>"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<option value="3" data-href="/search?q=%E7%82%89%E5%BF%83&amp;unread=1&amp;min_rating=3&amp;min_score=60">★3</option>"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<option value="0" data-href="/search?q=%E7%82%89%E5%BF%83&amp;unread=1&amp;unrated=1&amp;min_score=60">☆</option>"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<a class="btn off" href="/search?q=%E7%82%89%E5%BF%83&amp;min_rating=4&amp;min_score=60" aria-label="既読も表示：OFF""#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<a class="btn off" href="/search?q=%E7%82%89%E5%BF%83&amp;unread=1&amp;bookmarked=1&amp;min_rating=4&amp;min_score=60" aria-label="ブックマークだけ表示：OFF""#),
            "{bar}"
        );
        // JavaScript が無いときは、選択と一緒にほかの条件を送る
        assert!(
            bar.contains(r#"<input type="hidden" name="q" value="炉心">"#),
            "{bar}"
        );
        // フォームには重ねず、送るときにバーの条件を引き継ぐ
        let form = html
            .split(r#"<form method="get" action="/search">"#)
            .nth(1)
            .unwrap();
        let form = form.split("</form>").next().unwrap();
        for name in ["unread", "bookmarked", "unrated", "min_rating", "min_score"] {
            assert!(
                !form.contains(&format!(r#"name="{name}" value="1" checked"#))
                    && !form.contains(&format!(r#"<select name="{name}""#))
                    && !form.contains(&format!(r#"name="{name}" value="" "#)),
                "{name}: {form}"
            );
        }
        for (name, value) in [("unread", "1"), ("min_rating", "4"), ("min_score", "60")] {
            assert!(
                form.contains(&format!(
                    r#"<input type="hidden" name="{name}" value="{value}">"#
                )),
                "{name}: {form}"
            );
        }
        assert!(!form.contains(r#"name="bookmarked""#), "{form}");
        // 和訳ありはバーに無いのでフォームに残す
        assert!(form.contains(r#"name="translated" value="1">"#), "{form}");
    }

    #[test]
    fn search_page_keeps_the_conditions_in_the_form() {
        use crate::topics::Facet;
        let labels = SourceLabels::from([
            ("nra".to_string(), "原子力規制委員会".to_string()),
            ("wnn".to_string(), "WNN".to_string()),
        ]);
        let params = Params {
            q: "炉心 \"<b>\"".into(),
            since: "2026-09".into(),
            topics: vec!["PWR".into()],
            sources: vec!["nra".into()],
            lang: "ja".into(),
            unread: true,
            bookmarked: true,
            unrated: true,
            min_rating: "4".into(),
            min_score: "60".into(),
            sort: "score".into(),
            ..Params::default()
        };
        let vocabulary = [
            usage("燃料", Facet::Field, 3),
            usage("規制・審査", Facet::Field, 9),
            usage("高経年化", Facet::Field, 0),
            usage("PWR", Facet::Reactor, 2),
        ];
        let html = search_page(
            &params,
            None,
            &vocabulary,
            None,
            &Page {
                labels: &labels,
                ..Page::default()
            },
        );
        assert!(
            html.contains(r#"<form method="get" action="/search">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="q" value="炉心 &quot;&lt;b&gt;&quot;""#),
            "{html}"
        );
        assert!(html.contains(r#"name="since" value="2026-09""#), "{html}");
        // 付いている数の多い順。要約に付いていない語は出さない
        let field = html.find("規制・審査").unwrap();
        assert!(field < html.find("燃料").unwrap(), "{html}");
        assert!(!html.contains("高経年化"), "{html}");
        assert!(
            html.contains(r#"name="topic" value="PWR" checked"#),
            "{html}"
        );
        assert!(html.contains(r#"name="topic" value="燃料">"#), "{html}");
        assert!(
            html.contains(r#"name="source" value="nra" checked"#),
            "{html}"
        );
        assert!(html.contains("原子力規制委員会"), "{html}");
        assert!(html.contains(r#"<option value="ja" selected>"#), "{html}");
        assert!(html.contains(r#"name="translated" value="1">"#), "{html}");
        // 未読・ブックマーク・評価・評価なし・最低点はバーの条件で、フォームは hidden で引き継ぐ
        for (name, value) in [
            ("unread", "1"),
            ("bookmarked", "1"),
            ("unrated", "1"),
            ("min_rating", "4"),
            ("min_score", "60"),
        ] {
            assert!(
                html.contains(&format!(
                    r#"<input type="hidden" name="{name}" value="{value}">"#
                )),
                "{name}: {html}"
            );
        }
        assert!(!html.contains(r#"name="liked""#), "{html}");
        assert!(
            html.contains(r#"<option value="score" selected>"#),
            "{html}"
        );
        assert!(
            !html.contains("件"),
            "no results section without results: {html}"
        );
    }

    /// 検索語の欄は画面の幅いっぱいに広げる。
    #[test]
    fn search_page_widens_the_query_field() {
        let html = search_page(&Params::default(), None, &[], None, &Page::default());
        let at = html.find(r#"<input name="q""#).expect(&html);
        let input = &html[at..at + html[at..].find('>').unwrap()];
        assert!(input.contains(r#"class="wide""#), "{input}");
        assert!(html.contains(".wide { width: 100%;"), "{html}");
    }

    /// 期間は文字でも 📅 のカレンダーでも入れられる。カレンダーは名前を持たず送られない。
    #[test]
    fn search_page_offers_a_calendar_for_the_period() {
        let params = Params {
            since: "2026-09".into(),
            ..Params::default()
        };
        let html = search_page(&params, None, &[], None, &Page::default());
        // 月だけの指定もできるよう、文字の欄は残す
        assert!(html.contains(r#"name="since" value="2026-09""#), "{html}");
        // キーボードでも操作できるよう、日付の入力はフォーカスでき、名前を持つ
        for (name, label) in [("since", "開始日"), ("until", "終了日")] {
            let cal = format!(
                r#"<label class="cal" title="カレンダー">📅<input type="date" data-for="{name}" aria-label="{label}をカレンダーで選ぶ"></label>"#
            );
            let text = html.find(&format!(r#"name="{name}""#)).expect(&html);
            let at = html.find(&cal).expect(&html);
            assert!(text < at, "the calendar follows the text field: {html}");
        }
        assert!(html.contains("showPicker"), "{html}");
    }

    /// 語彙に無い語（統合した語の別名など）で検索しても、フォームを送り直して条件が消えないようにする。
    #[test]
    fn search_page_keeps_topics_outside_the_vocabulary() {
        let params = Params {
            topics: vec!["新設炉".into()],
            ..Params::default()
        };
        let vocabulary = [usage("燃料", crate::topics::Facet::Field, 3)];
        let html = search_page(&params, Some(&[]), &vocabulary, None, &Page::default());
        assert!(
            html.contains(r#"name="topic" value="新設炉" checked"#),
            "{html}"
        );
    }

    #[test]
    fn search_page_shows_results_errors_and_empty_results() {
        let params = Params {
            q: "炉心".into(),
            ..Params::default()
        };
        let items = [
            item(1, "2026-09-27T00:00:00.000Z"),
            item(2, "2026-09-26T00:00:00.000Z"),
        ];
        let html = search_page(&params, Some(&items), &[], None, &Page::default());
        assert!(html.contains("2 件"), "{html}");
        assert!(
            html.contains("/articles/1") && html.contains("/articles/2"),
            "{html}"
        );
        let html = search_page(&params, Some(&[]), &[], None, &Page::default());
        assert!(html.contains("該当する記事はありません"), "{html}");
        let html = search_page(
            &params,
            None,
            &[],
            Some("since must be <YYYY-MM>"),
            &Page::default(),
        );
        assert!(html.contains("since must be &lt;YYYY-MM&gt;"), "{html}");
    }
}
