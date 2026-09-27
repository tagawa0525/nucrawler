//! 記事の一覧とカード。

use super::*;

/// 一覧を「前回の訪問の後に届いた記事」と「それより前の未読の記事」に分ける。
/// `include_read` なら後者に既読の記事も残す。
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
    (
        new,
        earlier
            .into_iter()
            .filter(|i| include_read || !i.read)
            .collect(),
    )
}

/// 一覧の表示の切り替え。どちらもリンク（`all=1` / `read=1`）で切り替える。
#[derive(Clone, Copy, Default)]
pub struct ListView {
    /// 👎・見ない・低い点・未採点の記事も出す
    pub all: bool,
    /// 過去の欄に既読の記事も出す
    pub read: bool,
}

impl ListView {
    /// この表示の一覧の URL（HTML の属性値としてエスケープ済み）。
    fn href(self) -> String {
        let query: Vec<_> = [(self.all, "all=1"), (self.read, "read=1")]
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
    let all_toggle = ListView {
        all: !view.all,
        ..view
    };
    let read_toggle = ListView {
        read: !view.read,
        ..view
    };
    let mut body = format!(
        "<nav class=\"bar\">{}{}{}{}{}{}</nav>",
        button("/search", "検索", "🔍", None),
        button("/search?liked=1", "いいね", "👍", None),
        button("/search?bookmarked=1", "ブックマーク", "🔖", None),
        button(
            &all_toggle.href(),
            "おすすめだけ表示",
            "⭐",
            Some(!view.all)
        ),
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
    body.push_str(SWIPE_SCRIPT);
    layout("一覧", page, &body)
}

/// 一覧のカードを左右にスワイプして振り分ける（右でブックマーク、左で見ない）。
/// 振り分けたカードは隠し、しばらく「元に戻す」を出す。縦のスクロールはブラウザに任せ
/// （`touch-action: pan-y`）、画面の端から始まる操作はブラウザの「戻る」に譲る。
/// キーボードでは j/k・↓/↑ でカードを選び、l/→ と h/← で振り分け、u で取り消す。
const SWIPE_SCRIPT: &str = concat!("<script>\n", include_str!("assets/swipe.js"), "</script>");

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

/// 記事のカード。`swipe` なら一覧の振り分けの対象にする（`SWIPE_SCRIPT`）。
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
    let liked = if i.feedback == Some(crate::db::Feedback::Up) {
        " 👍"
    } else {
        ""
    };
    let bookmarked = if i.bookmarked { " 🔖" } else { "" };
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
         <div class=\"meta\">{source} ・{at}{liked}{bookmarked}{lock}{translation}</div>{matches}{summary}</div>",
        read = if i.read { " read" } else { "" },
        swipe = if swipe {
            format!(" data-id=\"{}\" tabindex=\"0\"", i.article_id)
        } else {
            String::new()
        },
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
    use crate::db::Feedback;
    use crate::web::html::test_support::*;

    #[test]
    fn splits_new_and_earlier_unread() {
        let mut read = item(3, "2026-09-26T00:00:00.000Z");
        read.read = true;
        let items = vec![
            item(1, "2026-09-27T05:00:00.000Z"),
            item(2, "2026-09-26T00:00:00.000Z"),
            read,
        ];
        let boundary = Some("2026-09-27T00:00:00.000Z");
        let (new, earlier) = split_sections(items.clone(), boundary, false);
        assert_eq!(new.iter().map(|i| i.article_id).collect::<Vec<_>>(), [1]);
        // 前回より前の記事は、未読のものだけを残す
        assert_eq!(
            earlier.iter().map(|i| i.article_id).collect::<Vec<_>>(),
            [2]
        );
        // 既読も出すなら、前回より前の記事をすべて残す
        let (new, earlier) = split_sections(items.clone(), boundary, true);
        assert_eq!(new.iter().map(|i| i.article_id).collect::<Vec<_>>(), [1]);
        assert_eq!(
            earlier.iter().map(|i| i.article_id).collect::<Vec<_>>(),
            [2, 3]
        );
        let (new, earlier) = split_sections(items, None, false);
        assert_eq!(new.len(), 3);
        assert!(earlier.is_empty());
    }

    #[test]
    fn list_page_links_to_search() {
        let html = list_page(&[], &[], ListView::default(), &Page::default());
        assert!(html.contains(r#"href="/search""#), "{html}");
        assert!(html.contains(r#"href="/search?bookmarked=1""#), "{html}");
        // 検索とブックマークの間に、いいねした記事へのボタンを置く
        let search = html.find(r#"href="/search""#).unwrap();
        let liked = html
            .find(r#"<a class="btn" href="/search?liked=1" aria-label="いいね" title="いいね">👍</a>"#)
            .expect(&html);
        let bookmarked = html.find(r#"href="/search?bookmarked=1""#).unwrap();
        assert!(search < liked && liked < bookmarked, "{html}");
    }

    /// 一覧のカードは左右のスワイプで振り分けられる（ブックマーク・見ない）。
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
        assert!(html.contains("/feedback/undo"), "{html}");
        // h/l・←/→ で振り分け、j/k・↓/↑ で選び、u で取り消す
        for key in ["ArrowRight", "ArrowLeft", "ArrowDown", "ArrowUp"] {
            assert!(html.contains(key), "{key}: {html}");
        }
        // 検索の結果は振り分けの対象にしない
        let p = Params::from_query("q=x");
        let results = [item(1, "2026-09-27T05:00:00.000Z")];
        let html = search_page(&p, Some(&results), &[], None, &Page::default());
        assert!(
            !html.contains(r#"data-id=""#) && !html.contains(SWIPE_SCRIPT),
            "{html}"
        );
    }

    /// 一覧の上部は見出しも説明も出さず、絵文字のボタンだけを並べる。
    /// 切り替えは今の状態を ON（緑）/ OFF（赤）で示す。
    /// ⭐ は「おすすめだけ」なので、すべて表示のとき OFF になる。
    #[test]
    fn list_page_shows_only_emoji_buttons_above_the_cards() {
        let view = ListView {
            all: true,
            read: false,
        };
        let html = list_page(&[], &[], view, &Page::default());
        assert!(!html.contains("<h1>"), "{html}");
        for text in ["おすすめだけ表示", "過去の既読", "スワイプ", "l / →"] {
            assert!(!html.contains(&format!(">{text}")), "{text}: {html}");
        }
        assert!(
            html.contains(r#"<a class="btn" href="/search" aria-label="検索" title="検索">🔍</a>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="btn off" href="/" aria-label="おすすめだけ表示：OFF" title="おすすめだけ表示：OFF">⭐</a>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="btn off" href="/?all=1&amp;read=1" aria-label="過去の既読も表示：OFF" title="過去の既読も表示：OFF">👁</a>"#
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

    /// 切り替えのリンクは、もう一方の切り替えの状態を引き継ぐ。
    #[test]
    fn list_page_toggles_keep_the_other_view() {
        let links = |all, read| {
            let html = list_page(&[], &[], ListView { all, read }, &Page::default());
            let mut hrefs: Vec<_> = html
                .match_indices(r#"href="/"#)
                .map(|(at, _)| {
                    let rest = &html[at + 6..];
                    rest[..rest.find('"').unwrap()].to_string()
                })
                .filter(|h| h == "/" || h.starts_with("/?"))
                .collect();
            hrefs.sort();
            hrefs
        };
        assert_eq!(links(false, false), ["/?all=1", "/?read=1"]);
        assert_eq!(links(true, false), ["/", "/?all=1&amp;read=1"]);
        assert_eq!(links(false, true), ["/", "/?all=1&amp;read=1"]);
        assert_eq!(links(true, true), ["/?all=1", "/?read=1"]);
    }

    #[test]
    fn list_page_names_the_earlier_section_by_whether_read_is_shown() {
        let mut read = item(2, "2026-09-26T00:00:00.000Z");
        read.read = true;
        let earlier = [read];
        let html = list_page(&[], &earlier, ListView::default(), &Page::default());
        assert!(html.contains("<h2>過去の未読</h2>"), "{html}");
        let view = ListView {
            all: false,
            read: true,
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
        assert!(html.contains(r#"href="/?all=1""#), "toggle to show all");
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
    fn card_marks_liked_and_bookmarked_articles() {
        let mut marked = item(1, "2026-09-27T05:00:00.000Z");
        marked.feedback = Some(Feedback::Up);
        marked.bookmarked = true;
        let html = card(&marked, false, &Page::default());
        assert!(html.contains(" 👍 🔖</div>"), "{html}");
        let mut disliked = item(2, "2026-09-27T05:00:00.000Z");
        disliked.feedback = Some(Feedback::Down);
        for i in [item(3, "2026-09-27T05:00:00.000Z"), disliked] {
            let html = card(&i, false, &Page::default());
            assert!(!html.contains('👍') && !html.contains('🔖'), "{html}");
        }
    }
}
