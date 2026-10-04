//! 検索の画面。

use super::bar::BarView;
use super::*;

/// 検索の画面の上部のバーが指す検索。点数・評価・既読・ブックマークはバーの条件で、変えるとほかの条件は
/// そのままに検索し直す。👁・🔖 は既定で絞らず、押すたびに 印のある記事だけ（`read=1`・`bookmarked=1`）→
/// 印の無い記事だけ（`read=0`・`bookmarked=0`）→ 絞らない と切り替える。評価は一覧と同じ選択肢で、既定は絞らない。
#[derive(Clone)]
struct SearchView(Params);

impl SearchView {
    fn with(&self, change: impl FnOnce(&mut Params)) -> Self {
        let mut p = self.0.clone();
        change(&mut p);
        Self(p)
    }
}

impl BarView for SearchView {
    fn action(&self) -> &'static str {
        "/search"
    }
    fn bar_url(&self) -> String {
        match self.0.query_string() {
            q if q.is_empty() => "/search".to_string(),
            q => format!("/search?{q}"),
        }
    }
    fn min(&self) -> Option<u8> {
        // 検索の条件と同じく、前後の空白を除いて読む（無ければ 0：絞らない）
        Some(self.0.min_score.trim().parse().unwrap_or(0))
    }
    fn extra_min(&self) -> Option<u8> {
        None
    }
    fn rating(&self) -> RatingFilter {
        if self.0.unrated {
            RatingFilter::Unrated
        } else if self.0.hide_low {
            RatingFilter::HideLow
        } else {
            // 検索の条件と同じく、前後の空白を除いて読む
            self.0
                .min_rating
                .trim()
                .parse()
                .ok()
                .and_then(Rating::new)
                .map_or(RatingFilter::Any, RatingFilter::AtLeast)
        }
    }
    fn read(&self) -> Option<bool> {
        self.0.read
    }
    fn bookmarked(&self) -> Option<bool> {
        self.0.bookmarked
    }
    fn with_min(&self, min: u8) -> Self {
        self.with(|p| {
            p.min_score = if min == 0 {
                String::new()
            } else {
                min.to_string()
            }
        })
    }
    fn with_rating(&self, rating: RatingFilter) -> Self {
        self.with(|p| {
            p.unrated = rating == RatingFilter::Unrated;
            p.hide_low = rating == RatingFilter::HideLow;
            p.min_rating = match rating {
                RatingFilter::AtLeast(r) => r.get().to_string(),
                _ => String::new(),
            };
        })
    }
    fn with_read(&self, read: Option<bool>) -> Self {
        self.with(|p| p.read = read)
    }
    fn with_bookmarked(&self, bookmarked: Option<bool>) -> Self {
        self.with(|p| p.bookmarked = bookmarked)
    }
    fn min_name(&self) -> &'static str {
        "min_score"
    }
    /// 評価の選択（`rating`、検索の条件の読み取りで min_rating・unrated・hide_low にする）で置き換わる
    fn rating_replaces(&self) -> &'static [&'static str] {
        &["min_rating", "unrated", "hide_low"]
    }
}

/// 検索画面。`results` が None なら（条件が無いときは）フォームだけを出す。
/// トピックの選択肢は要約に付いている語だけを、軸ごとに付いている数の多い順に並べる。
pub fn search_page(
    params: &Params,
    results: Option<&[ListItem]>,
    vocabulary: &[TopicUsage],
    error: Option<&str>,
    page: &Page,
) -> String {
    // 上部は一覧と同じバー（先頭の 🏠 で一覧へ戻る）で、点数・評価・既読・ブックマークの検索の条件を持つ
    let mut body = bar(&SearchView(params.clone()), true);
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

/// 上部のバーの条件（点数・評価・評価なし・既読・ブックマーク）を、フォームで送るための hidden の入力にする。
fn hidden_bar_conditions(p: &Params) -> String {
    let marks = [("read", p.read), ("bookmarked", p.bookmarked)];
    let texts = [("min_rating", &p.min_rating), ("min_score", &p.min_score)];
    marks
        .into_iter()
        .filter_map(|(name, v)| v.map(|on| (name, if on { "1" } else { "0" }.to_string())))
        .chain(p.unrated.then(|| ("unrated", "1".to_string())))
        .chain(p.hide_low.then(|| ("hide_low", "1".to_string())))
        .chain(
            texts
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(name, v)| (name, v.clone())),
        )
        .map(|(name, value)| {
            format!(
                "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
                escape(&value)
            )
        })
        .collect()
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
         <p>{translated}</p>{bar_conditions}\
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
        // 点数・評価・既読・ブックマークは上部のバーの条件なので、フォームで送るときは hidden で引き継ぐ
        bar_conditions = hidden_bar_conditions(p),
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
            html.contains(r#"name="rating""#) && html.contains("既読："),
            "{html}"
        );
    }

    /// バーは、検索と同じく前後の空白を除いた値を読む（検索の条件とバーの表示を合わせる）。
    #[test]
    fn search_bar_reads_trimmed_values() {
        let params = Params::from_query("min_score=%2060%20&min_rating=%204");
        let html = search_page(&params, Some(&[]), &[], None, &Page::default());
        let bar = html.split("</nav>").next().unwrap();
        assert!(bar.contains(r#"selected>60</option>"#), "{bar}");
        assert!(bar.contains(r#"selected>★4</option>"#), "{bar}");
    }

    /// 検索の画面のバーは、点数・評価・既読・ブックマークの検索の条件を持つ（フォームには重ねない）。
    /// バーを変えるとほかの条件はそのままに検索し直し、フォームで送るときはバーの条件を引き継ぐ。
    /// 👁 は既定で ON（既読も出す）、OFF で未読だけ。評価の ☆ は評価の無い記事だけ。
    #[test]
    fn search_bar_holds_the_mark_conditions() {
        let params = Params {
            q: "炉心".into(),
            read: Some(false),
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
            bar.contains(r#"<option value="60" data-href="/search?q=%E7%82%89%E5%BF%83&amp;read=0&amp;min_rating=4&amp;min_score=60" selected>60</option>"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<option value="0" data-href="/search?q=%E7%82%89%E5%BF%83&amp;read=0&amp;min_rating=4" data-closed="00">-</option>"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<option value="3" data-href="/search?q=%E7%82%89%E5%BF%83&amp;read=0&amp;min_rating=3&amp;min_score=60">★3</option>"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<option value="0" data-href="/search?q=%E7%82%89%E5%BF%83&amp;read=0&amp;unrated=1&amp;min_score=60">☆</option>"#),
            "{bar}"
        );
        // 評価の選択は一覧と同じ選択肢（検索の既定は絞らない）
        assert!(
            bar.contains(r#"<option value="any" data-href="/search?q=%E7%82%89%E5%BF%83&amp;read=0&amp;min_score=60" data-closed="★">-</option><option value="hide-low" data-href="/search?q=%E7%82%89%E5%BF%83&amp;read=0&amp;hide_low=1&amp;min_score=60" data-closed="★3+☆">★1〜2 を隠す</option>"#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<a class="btn not" href="/search?q=%E7%82%89%E5%BF%83&amp;min_rating=4&amp;min_score=60" aria-label="既読：未読だけ（押すと絞らない）""#),
            "{bar}"
        );
        assert!(
            bar.contains(r#"<a class="btn" href="/search?q=%E7%82%89%E5%BF%83&amp;read=0&amp;bookmarked=1&amp;min_rating=4&amp;min_score=60" aria-label="ブックマーク：絞らない（押すとブックマーク中だけ）""#),
            "{bar}"
        );
        // JavaScript が無いときは、選択を検索の欄で送り、ほかの条件を hidden で送る（選択で置き換わる条件は送らない）
        assert!(
            bar.contains(r#"<input type="hidden" name="q" value="炉心">"#),
            "{bar}"
        );
        let min_form = bar.split(r#"<form class="min on""#).nth(1).unwrap();
        let min_form = min_form.split("</form>").next().unwrap();
        assert!(
            min_form.contains(r#"<select name="min_score""#),
            "{min_form}"
        );
        assert!(
            !min_form.contains(r#"type="hidden" name="min_score""#),
            "{min_form}"
        );
        assert!(
            min_form.contains(r#"type="hidden" name="min_rating" value="4""#),
            "{min_form}"
        );
        let stars_form = bar.split(r#"<form class="stars on""#).nth(1).unwrap();
        let stars_form = stars_form.split("</form>").next().unwrap();
        assert!(
            stars_form.contains(r#"<select name="rating""#),
            "{stars_form}"
        );
        assert!(
            !stars_form.contains(r#"name="min_rating""#)
                && !stars_form.contains(r#"name="unrated""#)
                && !stars_form.contains(r#"name="hide_low""#),
            "{stars_form}"
        );
        assert!(
            stars_form.contains(r#"type="hidden" name="min_score" value="60""#),
            "{stars_form}"
        );
        // フォームには重ねず、送るときにバーの条件を引き継ぐ
        let form = html
            .split(r#"<form method="get" action="/search">"#)
            .nth(1)
            .unwrap();
        let form = form.split("</form>").next().unwrap();
        for name in [
            "read",
            "bookmarked",
            "unrated",
            "hide_low",
            "min_rating",
            "min_score",
        ] {
            assert!(
                !form.contains(&format!(r#"name="{name}" value="1" checked"#))
                    && !form.contains(&format!(r#"<select name="{name}""#))
                    && !form.contains(&format!(r#"name="{name}" value="" "#)),
                "{name}: {form}"
            );
        }
        for (name, value) in [("read", "0"), ("min_rating", "4"), ("min_score", "60")] {
            assert!(
                form.contains(&format!(
                    r#"<input type="hidden" name="{name}" value="{value}">"#
                )),
                "{name}: {form}"
            );
        }
        assert!(!form.contains(r#"name="bookmarked""#), "{form}");
        let hide_low = Params {
            q: "炉心".into(),
            hide_low: true,
            ..Params::default()
        };
        let html = search_page(&hide_low, Some(&[]), &[], None, &Page::default());
        assert!(
            html.contains(r#"<input type="hidden" name="hide_low" value="1">"#),
            "{html}"
        );
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
            read: Some(false),
            bookmarked: Some(true),
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
            ("read", "0"),
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
