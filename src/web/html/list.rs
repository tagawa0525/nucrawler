//! 記事の一覧とカード。

use super::*;

/// 一覧を「前回の訪問の後に届いた記事」と「それより前の未読の記事」に分ける。後者からは、前回の訪問までに
/// 既読になった記事を除く（今回の訪問で既読にした記事は残す）。`include_read` なら後者に既読の記事も残す。
/// `boundary`（`Db::begin_visit` の区切り）が無ければ（初回）、すべてを前者にする。
pub fn split_sections(
    items: Vec<ListItem>,
    boundary: Option<&str>,
    include_read: bool,
) -> (Vec<ListItem>, Vec<ListItem>) {
    let Some(boundary) = boundary else {
        return (items, Vec::new());
    };
    let (new, earlier): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|i| i.fetched_at.as_str() > boundary);
    (new, hide_read_before(earlier, Some(boundary), include_read))
}

/// 前の訪問までに既読になった記事を除く（`include_read` なら除かない）。今回の訪問で既読にした記事は、
/// 再読み込みしても残す。`boundary`（`Db::begin_visit` の区切り）が無ければ（初回）、除かない。
pub fn hide_read_before(
    items: Vec<ListItem>,
    boundary: Option<&str>,
    include_read: bool,
) -> Vec<ListItem> {
    let Some(boundary) = boundary.filter(|_| !include_read) else {
        return items;
    };
    items
        .into_iter()
        .filter(|i| i.read_at.as_deref().is_none_or(|at| at > boundary))
        .collect()
}

/// 一覧の表示の選択。最低点は `min=N`（既定の最低点なら省く）、過去の既読は `read=1` で持つ。
#[derive(Clone, Copy)]
pub struct ListView {
    /// 表示する最低点。0 なら評価 1〜2・未採点・軽水炉と無関係の記事も出す（すべて）
    pub min: u8,
    /// 既定の最低点（設定の `web.min_score`）
    pub default_min: u8,
    /// 過去の欄に既読の記事も出す
    pub read: bool,
}

impl Default for ListView {
    fn default() -> Self {
        let min = crate::config::WebConfig::default().min_score;
        Self {
            min,
            default_min: min,
            read: false,
        }
    }
}

/// 最低点の選択肢の刻み（0〜90）
const MIN_STEP: u8 = 10;

impl ListView {
    /// 「すべて」の表示か（評価 1〜2・未採点・軽水炉と無関係の記事も出す）。
    pub fn shows_all(self) -> bool {
        self.min == 0
    }

    /// この表示の一覧の URL（HTML の属性値としてエスケープ済み）。
    fn href(self) -> String {
        let min = format!("min={}", self.min);
        let query: Vec<&str> = [
            (self.min != self.default_min, min.as_str()),
            (self.read, "read=1"),
        ]
        .into_iter()
        .filter_map(|(on, q)| on.then_some(q))
        .collect();
        if query.is_empty() {
            "/".to_string()
        } else {
            format!("/?{}", query.join("&amp;"))
        }
    }
}

pub fn list_page(new: &[ListItem], earlier: &[ListItem], view: ListView, page: &Page) -> String {
    list_page_with_explore(new, earlier, &[], view, page)
}

/// 表示する最低点の選択。0〜90 の 10 刻みと既定・今の最低点から選び、選ぶとすぐ表示を切り替える
/// （JavaScript が無ければ「表示」のボタンで）。過去の既読の表示は引き継ぐ。
fn min_select(view: ListView) -> String {
    let mut values: Vec<u8> = (0..100).step_by(MIN_STEP.into()).collect();
    values.extend([view.default_min, view.min]);
    values.sort_unstable();
    values.dedup();
    let options: String = values
        .iter()
        .map(|v| {
            let selected = if *v == view.min { " selected" } else { "" };
            format!("<option value=\"{v}\"{selected}>{v}</option>")
        })
        .collect();
    let read = if view.read {
        "<input type=\"hidden\" name=\"read\" value=\"1\">"
    } else {
        ""
    };
    format!(
        "<form class=\"min\" method=\"get\" action=\"/\"><select name=\"min\" aria-label=\"表示する最低点\" \
         title=\"表示する最低点\" onchange=\"this.form.submit()\">{options}</select>{read}\
         <noscript><button>表示</button></noscript></form>"
    )
}

/// 一覧に、閾値未満から無作為に選んだ確認枠（`explore`）を添える。
pub fn list_page_with_explore(
    new: &[ListItem],
    earlier: &[ListItem],
    explore: &[ListItem],
    view: ListView,
    page: &Page,
) -> String {
    let read_toggle = ListView {
        read: !view.read,
        ..view
    };
    let mut body = format!(
        "<nav class=\"bar\">{}{}{}{}{}{}</nav>",
        button("/search", "検索", "🔍", None),
        button("/search?min_rating=4", "評価 4 以上", "👍", None),
        button("/search?bookmarked=1", "ブックマーク", "🔖", None),
        min_select(view),
        button(
            &read_toggle.href(),
            "過去の既読も表示",
            "👁",
            Some(view.read)
        ),
        button("/settings", "設定", "⚙️", None),
    );
    body.push_str("<h2>前回から</h2>");
    if new.is_empty() {
        body.push_str("<p class=\"meta\">新しい記事はありません</p>");
    }
    body.extend(new.iter().map(|i| card(i, true, page)));
    if !earlier.is_empty() {
        body.push_str(if view.read {
            "<h2>過去の記事</h2>"
        } else {
            "<h2>過去の未読</h2>"
        });
        body.extend(earlier.iter().map(|i| card(i, true, page)));
    }
    if !explore.is_empty() {
        body.push_str(
            "<h2>確認枠</h2><p class=\"meta\">おすすめの閾値に届かなかった記事から無作為に選んでいます。\
             開いて ★ で評価してください（似た記事を読んだだけなら、左のスワイプで既読に）</p>",
        );
        body.extend(explore.iter().map(|i| card(i, true, page)));
    }
    body.push_str(MARKS_SCRIPT);
    layout("一覧", page, &body)
}

/// 一覧のカードの印（`marks`）を、ページを移らずにその場で付け外しする。カードは消さない。
/// 左右のスワイプでも印を付けられる（右でブックマーク、左で既読）。縦のスクロールはブラウザに任せ
/// （`touch-action: pan-y`）、画面の端から始まる操作はブラウザの「戻る」に譲る。
/// キーボードでは j/k・↓/↑ でカードを選び、1〜5 で評価、0 で評価なし、l/→ でブックマーク、h/← で既読。
/// スワイプ・キーはカードのボタンと同じ送信を通す。
pub(super) const MARKS_SCRIPT: &str =
    concat!("<script>\n", include_str!("assets/marks.js"), "</script>");

/// 評価（1〜5 の星）とブックマーク・既読の印。一覧のカードと詳細で共有する。
/// 星は今の評価まで塗り、今の評価の星を押すと評価なしに戻る。星は記号だけなので、段階の意味を
/// 読み上げの名前（aria-label）にも付け、`data-label` にも持たせて画面の側で付け直せるようにする。
/// ブックマーク・既読は押すと今の逆にするボタンで、状態を `aria-pressed` で示す。
/// JavaScript が無ければフォームの送信で付け、詳細に戻る。
pub(super) fn marks(i: &ListItem) -> String {
    let id = i.article_id;
    let stars: String = Rating::all()
        .map(|r| {
            let on = i.rating.is_some_and(|c| r <= c);
            let label = format!("{} {}", r.get(), r.meaning());
            let (value, title) = if i.rating == Some(r) {
                (String::new(), format!("{label}（押すと評価なし）"))
            } else {
                (r.get().to_string(), label.clone())
            };
            format!(
                "<button name=\"value\" value=\"{value}\" data-label=\"{label}\" aria-label=\"{title}\" \
                 title=\"{title}\"{}>{}</button>",
                if on { " class=\"on\"" } else { "" },
                if on { '★' } else { '☆' }
            )
        })
        .collect();
    let toggle = |mark: &str, label: &str, glyph: &str, on: bool| {
        format!(
            "<form method=\"post\" action=\"/articles/{id}/{mark}\">\
             <button name=\"on\" value=\"{}\" aria-pressed=\"{on}\" aria-label=\"{label}\" title=\"{label}\"{}>\
             {glyph}</button></form>",
            if on { "0" } else { "1" },
            if on { " class=\"on\"" } else { "" },
        )
    };
    format!(
        "<div class=\"actions marks\"><form method=\"post\" action=\"/articles/{id}/rating\" class=\"rating\">\
         {stars}</form>{}{}</div>",
        toggle("bookmark", "ブックマーク", "🔖", i.bookmarked),
        toggle("read", "既読", "👁", i.is_read()),
    )
}

/// 点数が当たったプロファイルの語（関心分野と、除外に当たった話題）。
pub(super) fn matches(i: &ListItem) -> String {
    let matched = i
        .matched
        .iter()
        .map(|t| format!("<span class=\"match\">{}</span>", escape(t)));
    let excluded = i
        .excluded
        .iter()
        .map(|t| format!("<span class=\"match excluded\">除外 {}</span>", escape(t)));
    matched.chain(excluded).collect()
}

/// 記事のカード。`swipe` なら一覧のカードとして、印（`marks`）を付けてその場で付け外しできるようにする
/// （`MARKS_SCRIPT`）。そうでなければ（検索の結果）、印は見出しの下の行に記号で示す。
pub(super) fn card(i: &ListItem, swipe: bool, page: &Page) -> String {
    let title = display_title(i.title_ja.as_deref(), i);
    let score = i
        .score
        .map_or_else(String::new, |s| format!("<span class=\"score\">{s}</span>"));
    let lock = if i.locked_by.is_empty() {
        String::new()
    } else {
        format!(" 🔒 {}限定", escape(&i.locked_by.join("・")))
    };
    // 一覧のカードでは印のボタンが状態を示すので、見出しの下の行には出さない
    let rating = i
        .rating
        .filter(|_| !swipe)
        .map(|r| format!(" ★{}", r.get()))
        .unwrap_or_default();
    let bookmarked = if i.bookmarked && !swipe { " 🔖" } else { "" };
    let translation = if i.has_translation {
        " ・和訳あり"
    } else if i.translation_requested {
        " ・和訳待ち"
    } else {
        ""
    };
    let summary = i
        .summary_ja
        .as_deref()
        .map_or_else(String::new, |s| format!("<div>{}</div>", escape(s)));
    format!(
        "<div class=\"card{read}\"{swipe}>{score}<a class=\"title\" href=\"/articles/{id}\">{title}</a>\
         <div class=\"meta\">{source} ・{at}{rating}{bookmarked}{lock}{translation}</div>{matches}{summary}{marks}</div>",
        read = if i.is_read() { " read" } else { "" },
        swipe = if swipe {
            format!(" data-id=\"{}\" tabindex=\"0\"", i.article_id)
        } else {
            String::new()
        },
        marks = if swipe { marks(i) } else { String::new() },
        id = i.article_id,
        title = escape(title),
        source = escape(page.source(&i.source_id)),
        at = crate::jst::format_local(&i.at),
        matches = matches(i),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Rating;
    use crate::web::html::test_support::*;

    #[test]
    fn splits_new_and_earlier_unread() {
        let read_at = |id: i64, at: &str| {
            let mut i = item(id, "2026-09-26T00:00:00.000Z");
            i.read_at = Some(at.into());
            i
        };
        let items = vec![
            item(1, "2026-09-27T05:00:00.000Z"),
            item(2, "2026-09-26T00:00:00.000Z"),
            // 前の訪問より前に既読
            read_at(3, "2026-09-26T12:00:00.000Z"),
            // 今回の訪問で既読（印を付けたカードは再読み込みしても残る）
            read_at(4, "2026-09-27T06:00:00.000Z"),
        ];
        let boundary = Some("2026-09-27T00:00:00.000Z");
        let (new, earlier) = split_sections(items.clone(), boundary, false);
        assert_eq!(new.iter().map(|i| i.article_id).collect::<Vec<_>>(), [1]);
        // 前回より前の記事は、前の訪問までに既読になったものを除く
        assert_eq!(
            earlier.iter().map(|i| i.article_id).collect::<Vec<_>>(),
            [2, 4]
        );
        // 既読も出すなら、前回より前の記事をすべて残す
        let (new, earlier) = split_sections(items.clone(), boundary, true);
        assert_eq!(new.iter().map(|i| i.article_id).collect::<Vec<_>>(), [1]);
        assert_eq!(
            earlier.iter().map(|i| i.article_id).collect::<Vec<_>>(),
            [2, 3, 4]
        );
        let (new, earlier) = split_sections(items, None, false);
        assert_eq!(new.len(), 4);
        assert!(earlier.is_empty());
    }

    /// 一覧のカードでは、評価・ブックマーク・既読の印をその場で付け外しできる。
    /// 印の状態はボタンが示すので、見出しの下の行には重ねて出さない。
    #[test]
    fn list_cards_offer_marks_in_place() {
        let mut marked = item(1, "2026-09-27T05:00:00.000Z");
        marked.rating = Rating::new(4);
        marked.bookmarked = true;
        marked.read_at = Some("2026-09-27T06:00:00.000Z".into());
        let html = list_page(
            &[marked, item(2, "2026-09-27T05:00:00.000Z")],
            &[],
            ListView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(r#"<div class="card read" data-id="1" tabindex="0">"#),
            "{html}"
        );
        assert!(html.contains(r#"action="/articles/1/rating""#), "{html}");
        assert!(
            html.contains(r#"<button name="value" value="" data-label="4 読んでよかった" aria-label="4 読んでよかった（押すと評価なし）" title="4 読んでよかった（押すと評価なし）" class="on">★</button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<form method="post" action="/articles/1/bookmark"><button name="on" value="0" aria-pressed="true" aria-label="ブックマーク" title="ブックマーク" class="on">🔖</button></form>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<form method="post" action="/articles/1/read"><button name="on" value="0" aria-pressed="true" aria-label="既読" title="既読" class="on">👁</button></form>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<form method="post" action="/articles/2/bookmark"><button name="on" value="1" aria-pressed="false" aria-label="ブックマーク" title="ブックマーク">🔖</button></form>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<form method="post" action="/articles/2/read"><button name="on" value="1" aria-pressed="false" aria-label="既読" title="既読">👁</button></form>"#),
            "{html}"
        );
        assert!(!html.contains(" ★4"), "{html}");
    }

    #[test]
    fn list_page_links_to_search() {
        let html = list_page(&[], &[], ListView::default(), &Page::default());
        assert!(html.contains(r#"href="/search""#), "{html}");
        assert!(html.contains(r#"href="/search?bookmarked=1""#), "{html}");
        // 検索とブックマークの間に、評価 4 以上の記事へのボタンを置く
        let search = html.find(r#"href="/search""#).unwrap();
        let liked = html
            .find(r#"<a class="btn" href="/search?min_rating=4" aria-label="評価 4 以上" title="評価 4 以上">👍</a>"#)
            .expect(&html);
        let bookmarked = html.find(r#"href="/search?bookmarked=1""#).unwrap();
        assert!(search < liked && liked < bookmarked, "{html}");
    }

    /// 一覧のカードは左右のスワイプで印を付けられる（ブックマーク・既読）。
    #[test]
    fn list_page_cards_can_be_swiped() {
        let html = list_page(
            &[item(1, "2026-09-27T05:00:00.000Z")],
            &[],
            ListView::default(),
            &Page::default(),
        );
        // キーボードでも選べるよう、カードにフォーカスを置ける
        assert!(
            html.contains(r#"<div class="card" data-id="1" tabindex="0">"#),
            "{html}"
        );
        assert!(html.contains("<script>"), "{html}");
        // スワイプ・キーは、カードのボタンと同じ送信を通す（印を付けてもカードは消さない）
        assert!(html.contains("requestSubmit"), "{html}");
        // 戻るボタンで戻ったときは、ブラウザが残していた古いページを出すので、印だけを読み直して合わせる
        // （一覧を丸ごと取り直さない。取り直すと訪問として記録される）
        assert!(
            html.contains("pageshow") && html.contains("back_forward"),
            "{html}"
        );
        assert!(
            html.contains("/api/marks?ids=") && !html.contains("fetch(location.href"),
            "{html}"
        );
        assert!(
            !html.contains("card.hidden = true") && !html.contains("元に戻す"),
            "{html}"
        );
        // ←/→ で既読・ブックマーク、↓/↑ で選ぶ
        for key in ["ArrowRight", "ArrowLeft", "ArrowDown", "ArrowUp"] {
            assert!(html.contains(key), "{key}: {html}");
        }
        // 検索の結果は印の対象にしない
        let p = Params::from_query("q=x");
        let results = [item(1, "2026-09-27T05:00:00.000Z")];
        let html = search_page(&p, Some(&results), &[], None, &Page::default());
        assert!(
            !html.contains(r#"data-id=""#) && !html.contains(MARKS_SCRIPT),
            "{html}"
        );
    }

    /// 一覧の上部は見出しも説明も出さず、ボタンだけを並べる。
    /// 切り替えは今の状態を ON（緑）/ OFF（赤）で示す。表示する最低点は数字で選ぶ（0 はすべて）。
    #[test]
    fn list_page_shows_only_buttons_above_the_cards() {
        let view = ListView {
            min: 0,
            ..ListView::default()
        };
        let html = list_page(&[], &[], view, &Page::default());
        assert!(!html.contains("<h1>"), "{html}");
        for text in ["おすすめだけ表示", "過去の既読", "スワイプ", "l / →"] {
            assert!(!html.contains(&format!(">{text}")), "{text}: {html}");
        }
        assert!(!html.contains('⭐'), "{html}");
        assert!(
            html.contains(r#"<a class="btn" href="/search" aria-label="検索" title="検索">🔍</a>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<form class="min" method="get" action="/"><select name="min" aria-label="表示する最低点" title="表示する最低点" onchange="this.form.submit()">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="0" selected>0</option>"#),
            "{html}"
        );
        assert!(html.contains(r#"<option value="50">50</option>"#), "{html}");
        assert!(html.contains(r#"<option value="90">90</option>"#), "{html}");
        assert!(!html.contains(r#"<option value="100">"#), "{html}");
        assert!(
            html.contains(
                r#"<a class="btn off" href="/?min=0&amp;read=1" aria-label="過去の既読も表示：OFF" title="過去の既読も表示：OFF">👁</a>"#
            ),
            "{html}"
        );
        // 管理の画面へは右端の ⚙ から入る
        assert!(
            html.contains(
                r#"<a class="btn" href="/settings" aria-label="設定" title="設定">⚙️</a></nav>"#
            ),
            "{html}"
        );
    }

    /// 最低点の選択と 👁 は、もう一方の状態を引き継ぐ。既定の最低点は URL に出さない。
    /// 設定の最低点が 10 刻みでなくても選べる。
    #[test]
    fn list_page_controls_keep_the_other_view() {
        let eye = |min, read| {
            let view = ListView {
                min,
                read,
                ..ListView::default()
            };
            let html = list_page(&[], &[], view, &Page::default());
            let at = html.find("👁").unwrap();
            let start = html[..at].rfind("href=\"").unwrap() + 6;
            html[start..start + html[start..].find('"').unwrap()].to_string()
        };
        assert_eq!(eye(50, false), "/?read=1");
        assert_eq!(eye(50, true), "/");
        assert_eq!(eye(30, false), "/?min=30&amp;read=1");
        assert_eq!(eye(30, true), "/?min=30");
        let read = ListView {
            read: true,
            ..ListView::default()
        };
        let html = list_page(&[], &[], read, &Page::default());
        assert!(
            html.contains(r#"<input type="hidden" name="read" value="1">"#),
            "{html}"
        );
        let odd = ListView {
            min: 55,
            default_min: 55,
            read: false,
        };
        let html = list_page(&[], &[], odd, &Page::default());
        let at = |v: &str| html.find(&format!(r#"<option value="{v}""#)).unwrap();
        assert!(at("50") < at("55") && at("55") < at("60"), "{html}");
        assert!(
            html.contains(r#"<option value="55" selected>55</option>"#),
            "{html}"
        );
        assert!(!html.contains(r#"name="read""#), "{html}");
    }

    #[test]
    fn list_page_names_the_earlier_section_by_whether_read_is_shown() {
        let mut read = item(2, "2026-09-26T00:00:00.000Z");
        read.read_at = Some("2026-09-26T12:00:00.000Z".into());
        let earlier = [read];
        let html = list_page(&[], &earlier, ListView::default(), &Page::default());
        assert!(html.contains("<h2>過去の未読</h2>"), "{html}");
        let view = ListView {
            read: true,
            ..ListView::default()
        };
        let html = list_page(&[], &earlier, view, &Page::default());
        assert!(html.contains("<h2>過去の記事</h2>"), "{html}");
        assert!(html.contains(r#"class="card read""#), "{html}");
    }

    #[test]
    fn list_page_renders_cards_with_escaped_text() {
        let mut locked = item(2, "2026-09-26T00:00:00.000Z");
        locked.locked_by = vec!["日本原子力学会".into()];
        locked.translation_requested = true;
        let mut untitled = item(3, "2026-09-26T00:00:00.000Z");
        untitled.title_ja = None;
        untitled.score = None;
        let html = list_page(
            &[item(1, "2026-09-27T05:00:00.000Z")],
            &[locked, untitled],
            ListView::default(),
            &Page::default(),
        );
        assert!(html.contains("前回から"));
        assert!(html.contains(r#"href="/articles/1""#));
        assert!(html.contains("見出し1"));
        assert!(html.contains("要約&lt;b&gt;"), "summary is escaped");
        assert!(!html.contains("要約<b>"));
        assert!(html.contains("80"));
        assert!(html.contains("🔒") && html.contains("日本原子力学会"));
        assert!(html.contains("和訳待ち"));
        // digest が無ければ原題を出す
        assert!(html.contains("Title 3"));
        assert!(html.contains(r#"<select name="min""#), "threshold: {html}");
    }

    /// カードのソース・日付の横に、いいねとブックマークの印を出す。
    /// 点数が当たったプロファイルの語を、点数の意味として一緒に出す。
    #[test]
    fn card_shows_the_terms_the_score_matched() {
        let mut i = item(1, "2026-09-27T05:00:00.000Z");
        i.matched = vec!["燃料".into(), "規制<審査>".into()];
        i.excluded = vec!["核融合".into()];
        let html = card(&i, false, &Page::default());
        assert!(
            html.contains(
                "<span class=\"match\">燃料</span><span class=\"match\">規制&lt;審査&gt;</span>"
            ),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"match excluded\">除外 核融合</span>"),
            "{html}"
        );
        let plain = card(
            &item(2, "2026-09-27T05:00:00.000Z"),
            false,
            &Page::default(),
        );
        assert!(!plain.contains("class=\"match"), "{plain}");
    }

    #[test]
    fn list_page_adds_the_explore_section() {
        let picked = item(9, "2026-09-27T05:00:00.000Z");
        let html = list_page_with_explore(
            &[],
            &[],
            std::slice::from_ref(&picked),
            ListView::default(),
            &Page::default(),
        );
        assert!(html.contains("<h2>確認枠</h2>"), "{html}");
        // 確認枠は評価を集めるためのもの。見送り（今は既読の印）では集まらない
        assert!(html.contains("開いて ★ で評価してください"), "{html}");
        assert!(!html.contains("見送"), "{html}");
        assert!(html.contains("無作為"), "{html}");
        // 他のカードと同じく振り分けられる
        assert!(html.contains("data-id=\"9\""), "{html}");
        let none = list_page(&[], &[], ListView::default(), &Page::default());
        assert!(!none.contains("確認枠"), "{none}");
    }

    #[test]
    fn card_marks_rated_and_bookmarked_articles() {
        let mut marked = item(1, "2026-09-27T05:00:00.000Z");
        marked.rating = Rating::new(4);
        marked.bookmarked = true;
        let html = card(&marked, false, &Page::default());
        assert!(html.contains(" ★4 🔖</div>"), "{html}");
        let mut low = item(2, "2026-09-27T05:00:00.000Z");
        low.rating = Rating::new(1);
        let html = card(&low, false, &Page::default());
        assert!(html.contains(" ★1</div>"), "{html}");
        let html = card(
            &item(3, "2026-09-27T05:00:00.000Z"),
            false,
            &Page::default(),
        );
        assert!(!html.contains('★') && !html.contains('🔖'), "{html}");
    }
}
