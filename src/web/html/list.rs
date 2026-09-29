//! 記事の一覧とカード。

use super::*;

/// 一覧を「前回の訪問の後に届いた記事」と「それより前の記事」に分ける。既読の記事は一覧の問い合わせで
/// 除いておく（`ListQuery::unread`）。`boundary`（`Db::begin_visit` の区切り）が無ければ（初回）、
/// すべてを前者にする。
pub fn split_sections(
    items: Vec<ListItem>,
    boundary: Option<&str>,
) -> (Vec<ListItem>, Vec<ListItem>) {
    let Some(boundary) = boundary else {
        return (items, Vec::new());
    };
    items
        .into_iter()
        .partition(|i| i.fetched_at.as_str() > boundary)
}

/// 既読の記事を除く（`include_read` なら除かない）。確認枠はその日に選んだ記事を出し直すので、
/// 選んだ後に既読にした記事をここで除く。
pub fn hide_read(items: Vec<ListItem>, include_read: bool) -> Vec<ListItem> {
    if include_read {
        return items;
    }
    items.into_iter().filter(|i| !i.is_read()).collect()
}

/// 一覧の表示の選択。最低点は `min=N`（既定の最低点なら省く）、既読は `read=1` で持つ。
/// 評価（`rating=N`）・ブックマーク（`bookmarked=1`）で絞るときは、一覧の代わりに該当する記事を出す。
/// 絞り込みは評価した（読んだことの多い）記事を探すので、既定で既読も出し、隠すときに `read=0` を持つ。
#[derive(Clone, Copy)]
pub struct ListView {
    /// 表示する最低点。0 なら評価 1〜2・未採点・軽水炉と無関係の記事も出す（すべて）
    pub min: u8,
    /// 既定の最低点（設定の `web.min_score`）
    pub default_min: u8,
    /// 既読の記事も出す（一覧の既定は出さない、絞り込みの既定は出す）
    pub read: bool,
    /// この評価（1〜5）以上の記事に絞る。0 なら評価の無い記事だけ
    pub rating: Option<u8>,
    /// ブックマークした記事に絞る
    pub bookmarked: bool,
}

impl Default for ListView {
    fn default() -> Self {
        let min = crate::config::WebConfig::default().min_score;
        Self {
            min,
            default_min: min,
            read: false,
            rating: None,
            bookmarked: false,
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

    /// 評価・ブックマークで絞っているか。
    pub fn filtered(self) -> bool {
        self.rating.is_some() || self.bookmarked
    }

    /// この表示での最低点の既定。一覧は設定の最低点、絞り込みは 0（点数で絞らない）。
    fn min_default(self) -> u8 {
        if self.filtered() { 0 } else { self.default_min }
    }

    /// 絞り込みを変えた表示。一覧と絞り込みを行き来するときは、既読の表示と最低点を行き先の既定に戻す。
    fn with_filters(self, rating: Option<u8>, bookmarked: bool) -> Self {
        let mut next = Self {
            rating,
            bookmarked,
            ..self
        };
        if next.filtered() != self.filtered() {
            next.read = next.filtered();
            next.min = next.min_default();
        }
        next
    }

    /// この表示の一覧の URL（HTML の属性値としてエスケープ済み）。
    fn href(self) -> String {
        escape(&self.url())
    }

    /// この表示の一覧の正規の URL。既定と同じ値は付けない（最低点・既読の表示は、この表示での既定と
    /// 違うときだけ）。
    pub fn url(self) -> String {
        let min = format!("min={}", self.min);
        let rating = format!("rating={}", self.rating.unwrap_or_default());
        let filtered = self.filtered();
        let query: Vec<&str> = [
            (self.min != self.min_default(), min.as_str()),
            (!filtered && self.read, "read=1"),
            (self.rating.is_some(), rating.as_str()),
            (filtered && !self.read, "read=0"),
            (self.bookmarked, "bookmarked=1"),
        ]
        .into_iter()
        .filter_map(|(on, q)| on.then_some(q))
        .collect();
        if query.is_empty() {
            "/".to_string()
        } else {
            format!("/?{}", query.join("&"))
        }
    }
}

pub fn list_page(new: &[ListItem], earlier: &[ListItem], view: ListView, page: &Page) -> String {
    list_page_with_explore(new, earlier, &[], view, page)
}

/// 選択を選んだときに移る先。選択肢ごとに、その表示の正規の URL を `data-href` に持たせる
/// （一覧と絞り込みを行き来するときに、既読の表示と最低点を行き先の既定に戻すため）。
const JUMP: &str = "location.href=this.selectedOptions[0].dataset.href";

/// JavaScript が無いときに選択と一緒に送る、今の表示のほかの条件（正規の URL の `except` 以外）。
fn state_inputs(view: ListView, except: &str) -> String {
    let url = view.url();
    url.split_once('?')
        .map_or("", |(_, query)| query)
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(key, _)| *key != except)
        .map(|(key, value)| format!("<input type=\"hidden\" name=\"{key}\" value=\"{value}\">"))
        .collect()
}

/// 選択肢。今の選択なら `selected`。
fn option(value: &str, target: ListView, selected: bool, label: &str) -> String {
    format!(
        "<option value=\"{value}\" data-href=\"{}\"{}>{label}</option>",
        target.href(),
        if selected { " selected" } else { "" },
    )
}

/// 「絞らない」の選択肢。開いた一覧では「-」、閉じた選択では `closed`（00・★）と書く（`BAR_SCRIPT`）。
fn blank_option(value: &str, target: ListView, selected: bool, closed: &str) -> String {
    format!(
        "<option value=\"{value}\" data-href=\"{}\" data-closed=\"{closed}\"{}>-</option>",
        target.href(),
        if selected { " selected" } else { "" },
    )
}

/// 上部のバーの選択の「絞らない」を、閉じているときは 00・★、開いた一覧では「-」と書き分ける。
/// JavaScript が無ければ「-」のまま。
pub(super) const BAR_SCRIPT: &str =
    concat!("<script>\n", include_str!("assets/bar.js"), "</script>");

/// 表示する最低点の選択。0〜90 の 10 刻みと既定・今の最低点から選び、選ぶとすぐ表示を切り替える
/// （JavaScript が無ければ「表示」のボタンで）。0 は点数で絞らない（すべて。開いた一覧では「-」、閉じた選択では 00）で、
/// それ以外のあいだは緑にする。
fn min_select(view: ListView) -> String {
    let mut values: Vec<u8> = (0..100).step_by(MIN_STEP.into()).collect();
    values.extend([view.default_min, view.min]);
    values.sort_unstable();
    values.dedup();
    let options: String = values
        .into_iter()
        .map(|v| {
            let target = ListView { min: v, ..view };
            if v == 0 {
                // 絞らない
                blank_option("0", target, v == view.min, "00")
            } else {
                // 桁をそろえる（1 桁は 0 を付ける）
                option(&v.to_string(), target, v == view.min, &format!("{v:02}"))
            }
        })
        .collect();
    format!(
        "<form class=\"min{}\" method=\"get\" action=\"/\"><select name=\"min\" aria-label=\"表示する最低点\" \
         title=\"表示する最低点\" onchange=\"{JUMP}\">{options}</select>{}\
         <noscript><button>表示</button></noscript></form>",
        if view.min == 0 { "" } else { " on" },
        state_inputs(view, "min"),
    )
}

/// 評価で絞る選択。最低点の数字と見分けられるよう ★ で示す。「-」（閉じた選択では数字の無い ★）は絞らない、
/// ★1〜★5 は最低点と同じく小さい順で「以上」の印は付けない（★4 は ★4 以上）、最後の白抜きの「☆」は
/// 評価の無い記事だけ。絞っているあいだは緑にする。
/// 選ぶとすぐ表示を切り替える（JavaScript が無ければ「表示」のボタンで、絞り込みの中ならほかの条件も引き継ぐ）。
fn rating_select(view: ListView) -> String {
    let choices = [
        (Some(1), "★1"),
        (Some(2), "★2"),
        (Some(3), "★3"),
        (Some(4), "★4"),
        (Some(5), "★5"),
        (Some(0), "☆"),
    ];
    // 絞らない（閉じた選択では数字の無い ★）
    let blank = blank_option(
        "",
        view.with_filters(None, view.bookmarked),
        view.rating.is_none(),
        "★",
    );
    let options: String = std::iter::once(blank)
        .chain(choices.iter().map(|(rating, label)| {
            let value = rating.map(|r| r.to_string()).unwrap_or_default();
            let target = view.with_filters(*rating, view.bookmarked);
            option(&value, target, *rating == view.rating, label)
        }))
        .collect();
    // 一覧から絞り込みへ移るときは、一覧の条件を持ち込まない（絞り込みの既定にする）
    let inputs = if view.filtered() {
        state_inputs(view, "rating")
    } else {
        String::new()
    };
    format!(
        "<form class=\"stars{}\" method=\"get\" action=\"/\"><select name=\"rating\" aria-label=\"評価で絞る\" \
         title=\"評価で絞る\" onchange=\"{JUMP}\">{options}</select>{inputs}\
         <noscript><button>表示</button></noscript></form>",
        if view.rating.is_some() { " on" } else { "" },
    )
}

/// 一覧の上部のバー。検索・点数・評価・既読・ブックマーク・設定の順で、カードの下の印と同じ並びにする。
fn bar(view: ListView) -> String {
    let bookmark = button(
        &view.with_filters(view.rating, !view.bookmarked).href(),
        "ブックマークだけ表示",
        "🔖",
        Some(view.bookmarked),
    );
    let min = min_select(view);
    let read_toggle = ListView {
        read: !view.read,
        ..view
    };
    let read = button(&read_toggle.href(), "既読も表示", "👁", Some(view.read));
    format!(
        "<nav class=\"bar\">{}{min}{}{read}{bookmark}{}</nav>{BAR_SCRIPT}",
        button("/search", "検索", "🔍", None),
        rating_select(view),
        button("/settings", "設定", "⚙️", None),
    )
}

/// 評価・ブックマークで絞った記事。上部のバーは一覧と同じで、検索のフォームは出さない。
pub fn filtered_page(items: &[ListItem], view: ListView, page: &Page) -> String {
    let mut body = bar(view);
    if items.is_empty() {
        body.push_str("<p class=\"meta\">該当する記事はありません</p>");
    } else {
        // 欄に絞り込みの条件を持たせ、条件から外れたカードをその場で隠す（`MARKS_SCRIPT`）
        let hide_read = if view.read {
            ""
        } else {
            " data-hide-read=\"1\""
        };
        let rating = match view.rating {
            None => String::new(),
            Some(0) => " data-unrated=\"1\"".to_string(),
            Some(r) => format!(" data-min-rating=\"{r}\""),
        };
        let bookmarked = if view.bookmarked {
            " data-bookmarked=\"1\""
        } else {
            ""
        };
        body.push_str(&format!(
            "<h2 class=\"count\">{} 件</h2><div class=\"sections\"{hide_read}{rating}{bookmarked}>",
            items.len()
        ));
        body.extend(items.iter().map(|i| card(i, true, page)));
        body.push_str("</div>");
    }
    body.push_str(MARKS_SCRIPT);
    layout("一覧", page, &body)
}

/// 一覧に、閾値未満から無作為に選んだ確認枠（`explore`）を添える。
pub fn list_page_with_explore(
    new: &[ListItem],
    earlier: &[ListItem],
    explore: &[ListItem],
    view: ListView,
    page: &Page,
) -> String {
    let mut body = bar(view);
    // 既読を隠す一覧では、既読にしたカードをその場で隠す（`MARKS_SCRIPT`）
    body.push_str(if view.read {
        "<div class=\"sections\">"
    } else {
        "<div class=\"sections\" data-hide-read=\"1\">"
    });
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
    body.push_str("</div>");
    body.push_str(MARKS_SCRIPT);
    layout("一覧", page, &body)
}

/// 一覧のカードの印（`marks`）を、ページを移らずにその場で付け外しする。既読を隠す一覧
/// （`data-hide-read`）や、評価・ブックマークで絞った画面（`data-min-rating`・`data-bookmarked`）では、印を付け外しして
/// 欄の条件から外れたカードを隠し、しばらく「元に戻す」を出す（u キーでも戻す）。
/// 左右のスワイプでも印を付けられる（右でブックマーク、左で既読）。縦のスクロールはブラウザに任せ
/// （`touch-action: pan-y`）、画面の端から始まる操作はブラウザの「戻る」に譲る。
/// キーボードでは j/k・↓/↑ でカードを選び、1〜5 で評価、0 で評価なし、l/→ でブックマーク、h/← で既読。
/// スワイプ・キーはカードのボタンと同じ送信を通す。
pub(super) const MARKS_SCRIPT: &str =
    concat!("<script>\n", include_str!("assets/marks.js"), "</script>");

/// 評価（1〜5 の星）と既読・ブックマークの印。一覧のカードと詳細で共有する。並びは上部のバーと同じで、
/// キーの h（既読）が左、l（ブックマーク）が右。`lead` は行の先頭に置くもの（点数）。
/// 星は今の評価まで塗り、今の評価の星を押すと評価なしに戻る。星は記号だけなので、段階の意味を
/// 読み上げの名前（aria-label）にも付け、`data-label` にも持たせて画面の側で付け直せるようにする。
/// ブックマーク・既読は押すと今の逆にするボタンで、状態を `aria-pressed` で示す。
/// JavaScript が無ければフォームの送信で付け、詳細に戻る。
pub(super) fn marks(i: &ListItem, lead: &str) -> String {
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
        "<div class=\"actions marks\">{lead}<form method=\"post\" action=\"/articles/{id}/rating\" class=\"rating\">\
         {stars}</form>{}{}</div>",
        toggle("read", "既読", "👁", i.is_read()),
        toggle("bookmark", "ブックマーク", "🔖", i.bookmarked),
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

/// 推薦点の印。LLM の点数と違えば、title に LLM の点数と補正を出す（例：`LLM 72・補正 +9`）。
pub(super) fn score_badge(i: &ListItem) -> String {
    // 未採点も、数字の無い印を置いてカードの並びをそろえる（色だけでは伝わらないので読み上げの名前を付ける）
    let Some(score) = i.score else {
        return "<span class=\"score\" role=\"img\" aria-label=\"未採点\" title=\"未採点\">&nbsp;</span>".to_string();
    };
    match i.llm_score.filter(|llm| *llm != score) {
        Some(llm) => format!(
            "<span class=\"score\" title=\"LLM {llm}・補正 {:+}\">{score}</span>",
            i32::from(score) - i32::from(llm)
        ),
        None => format!("<span class=\"score\">{score}</span>"),
    }
}

/// 記事のカード。`swipe` なら一覧のカードとして、印（`marks`）を付けてその場で付け外しできるようにする
/// （`MARKS_SCRIPT`）。そうでなければ（検索の結果）、印は見出しの下の行に記号で示す。
pub(super) fn card(i: &ListItem, swipe: bool, page: &Page) -> String {
    let title = display_title(i.title_ja.as_deref(), i);
    let score = score_badge(i);
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
        "<div class=\"card{read}\"{swipe}>{title_score}<a class=\"title\" href=\"/articles/{id}\">{title}</a>\
         <div class=\"meta\">{source} ・{at}{rating}{bookmarked}{lock}{translation}</div>{matches}{summary}{marks}</div>",
        read = if i.is_read() { " read" } else { "" },
        swipe = if swipe {
            format!(" data-id=\"{}\" tabindex=\"0\"", i.article_id)
        } else {
            String::new()
        },
        // 一覧のカードでは点数を印の行の先頭に置く。検索の結果は印の行が無いので見出しの左に
        title_score = if swipe { String::new() } else { score.clone() },
        marks = if swipe {
            marks(i, &score)
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
    use crate::db::Rating;
    use crate::web::html::test_support::*;

    #[test]
    fn splits_new_and_earlier_unread() {
        let read_at = |id: i64, at: &str| {
            let mut i = item(id, "2026-09-26T00:00:00.000Z");
            i.read_at = Some(at.into());
            i
        };
        let mut read_new = item(5, "2026-09-27T05:00:00.000Z");
        read_new.read_at = Some("2026-09-27T06:00:00.000Z".into());
        let items = vec![
            item(1, "2026-09-27T05:00:00.000Z"),
            item(2, "2026-09-26T00:00:00.000Z"),
            // 前の訪問より前に既読
            read_at(3, "2026-09-26T12:00:00.000Z"),
            // 今回の訪問で既読
            read_at(4, "2026-09-27T06:00:00.000Z"),
            // 前回の後に届き、今回の訪問で既読
            read_new,
        ];
        let ids = |items: &[ListItem]| items.iter().map(|i| i.article_id).collect::<Vec<_>>();
        let boundary = Some("2026-09-27T00:00:00.000Z");
        // 前回の訪問の後に届いた記事と、それより前の記事に分ける（既読は一覧の問い合わせで除いておく）
        let (new, earlier) = split_sections(items.clone(), boundary);
        assert_eq!(ids(&new), [1, 5]);
        assert_eq!(ids(&earlier), [2, 3, 4]);
        // 既読は、いつ付いたかによらず除く（確認枠）。既読も出すなら残す
        assert_eq!(ids(&hide_read(items.clone(), false)), [1, 2]);
        assert_eq!(ids(&hide_read(items.clone(), true)), [1, 2, 3, 4, 5]);
        // 初回（区切りが無い）は、すべて前回からの欄
        let (new, earlier) = split_sections(items, None);
        assert_eq!(ids(&new), [1, 2, 3, 4, 5]);
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
        // カードの下は 点数・評価・既読・ブックマーク の順（h で既読、l でブックマーク）。
        // 点数は見出しの左から、この行の先頭に移す
        let second = html.split(r#"data-id="2""#).nth(1).unwrap();
        assert!(
            second.contains(r#"<div class="actions marks"><span class="score">80</span><form method="post" action="/articles/2/rating""#),
            "{second}"
        );
        assert!(
            second.find("/articles/2/read").unwrap() < second.find("/articles/2/bookmark").unwrap(),
            "{second}"
        );
        assert!(
            !second.contains(r#"tabindex="0"><span class="score">"#),
            "{second}"
        );
        // 未採点の記事も、点数の場所に数字の無い青い印を置いて並びをそろえる
        let mut unscored = item(3, "2026-09-27T05:00:00.000Z");
        unscored.score = None;
        let html = card(&unscored, true, &Page::default());
        assert!(
            html.contains(r#"<div class="actions marks"><span class="score" role="img" aria-label="未採点" title="未採点">&nbsp;</span><form"#),
            "{html}"
        );
    }

    #[test]
    fn list_page_links_to_search() {
        let html = list_page(&[], &[], ListView::default(), &Page::default());
        // バーは 検索・点数・評価・既読・ブックマーク・設定 の順（カードの下の印と同じ並び）
        let at = |needle: &str| html.find(needle).expect(needle);
        let order = [
            at(r#"href="/search""#),
            at(r#"name="min""#),
            at(r#"name="rating""#),
            at(r#"aria-label="既読も表示"#),
            at(r#"href="/?bookmarked=1""#),
            at(r#"href="/settings""#),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{html}");
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
        // 評価しても既読にはしない（既読の印は評価と別）
        assert!(!html.contains("setRead(marks, true)"), "{html}");
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
        // 既読を隠す一覧では、既読にしたカードをその場で隠し、しばらく「元に戻す」（u キー）を出す
        assert!(
            html.contains(r#"<div class="sections" data-hide-read="1">"#),
            "{html}"
        );
        assert!(
            html.contains("card.hidden = true") && html.contains("元に戻す"),
            "{html}"
        );
        assert!(html.contains(r#"e.key === "u""#), "{html}");
        // カードは欄の条件（既読を隠す・評価・ブックマーク）に合うかで出し隠しする。印を付け外しした後、
        // 送信に失敗した後（元に戻すが失敗したら隠し直す）、戻るボタンで戻ったときの読み直しの後のどれでも
        assert!(
            html.contains("f.hideRead")
                && html.contains("f.minRating")
                && html.contains("f.bookmarked"),
            "{html}"
        );
        assert_eq!(
            html.matches("setVisibility(card, matches(card))").count(),
            3,
            "{html}"
        );
        let shown = ListView {
            read: true,
            ..ListView::default()
        };
        let html = list_page(
            &[item(1, "2026-09-27T05:00:00.000Z")],
            &[],
            shown,
            &Page::default(),
        );
        assert!(!html.contains(r#"data-hide-read="1""#), "{html}");
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
    /// 切り替えは今の状態を ON（緑）/ OFF（白）で示す。表示する最低点は数字で選ぶ（0 はすべて）。
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
        // 選ぶと、その選択の正規の URL へ移る。00 は絞らない（すべて）で、絞っているあいだは緑
        assert!(
            html.contains(
                r#"<form class="min" method="get" action="/"><select name="min" aria-label="表示する最低点" title="表示する最低点" onchange="location.href=this.selectedOptions[0].dataset.href">"#
            ),
            "{html}"
        );
        assert!(
            // 「絞らない」は開いた一覧では「-」、閉じた選択では 00 と書く（`BAR_SCRIPT` が書き換える）
            html.contains(
                r#"<option value="0" data-href="/?min=0" data-closed="00" selected>-</option>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="50" data-href="/">50</option>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="90" data-href="/?min=90">90</option>"#),
            "{html}"
        );
        assert!(!html.contains(r#"<option value="100">"#), "{html}");
        assert!(
            html.contains(
                r#"<a class="btn off" href="/?min=0&amp;read=1" aria-label="既読も表示：OFF" title="既読も表示：OFF">👁</a>"#
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
            ..ListView::default()
        };
        let html = list_page(&[], &[], odd, &Page::default());
        let at = |v: &str| html.find(&format!(r#"<option value="{v}""#)).unwrap();
        assert!(at("50") < at("55") && at("55") < at("60"), "{html}");
        assert!(
            html.contains(r#"<option value="55" data-href="/" selected>55</option>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<form class="min on" method="get" action="/">"#),
            "{html}"
        );
        assert!(!html.contains(r#"name="read""#), "{html}");
    }

    /// 評価の選択は評価（★1〜5）以上、🔖 はブックマークだけに、一覧の上部のバーで絞る（検索画面へは移らない）。
    /// 評価の選択は、最低点の数字と見分けられるよう ★ で示す。並びは最低点と同じく小さい順で、
    /// 最低点と同じく「以上」の印（↑）は付けない。絞っているときは切り替えの ON と同じ緑にする。
    #[test]
    fn list_page_filters_by_rating_and_bookmark_in_the_bar() {
        assert!(
            STYLE.contains(
                ".btn.on, .marks button[aria-pressed=true], .bar .stars.on select, .bar .min.on select {"
            ),
            "{STYLE}"
        );
        let html = list_page(&[], &[], ListView::default(), &Page::default());
        assert!(!html.contains("/search?"), "{html}");
        assert!(
            html.contains(
                r#"<form class="stars" method="get" action="/"><select name="rating" aria-label="評価で絞る" title="評価で絞る" onchange="location.href=this.selectedOptions[0].dataset.href"><option value="" data-href="/" data-closed="★" selected>-</option><option value="1" data-href="/?rating=1">★1</option><option value="2" data-href="/?rating=2">★2</option><option value="3" data-href="/?rating=3">★3</option><option value="4" data-href="/?rating=4">★4</option><option value="5" data-href="/?rating=5">★5</option><option value="0" data-href="/?rating=0">☆</option></select>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="btn off" href="/?bookmarked=1" aria-label="ブックマークだけ表示：OFF" title="ブックマークだけ表示：OFF">🔖</a>"#
            ),
            "{html}"
        );
    }

    /// 絞り込んだ画面は、上部のバーと該当する記事だけを出す。検索のフォームも、効かない最低点と 👁 も出さない。
    /// 絞り込みはもう一方の状態を引き継ぎ、👍 を選び直すか 🔖 を外すと一覧に戻る。
    #[test]
    fn filtered_page_shows_the_bar_and_the_matches() {
        // 絞り込んだ画面の既定は、既読も出す
        let view = ListView {
            min: 0,
            rating: Some(4),
            read: true,
            ..ListView::default()
        };
        let mut rated = item(1, "2026-09-27T05:00:00.000Z");
        rated.rating = Rating::new(4);
        let html = filtered_page(&[rated], view, &Page::default());
        assert!(!html.contains(r#"action="/search""#), "{html}");
        // 最低点も絞れる（既定は 00 で絞らない）。👁 は既読も絞れる（OFF にすると `read=0`）
        assert!(
            html.contains(
                r#"<option value="0" data-href="/?rating=4" data-closed="00" selected>-</option>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="60" data-href="/?min=60&amp;rating=4">60</option>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="btn on" href="/?rating=4&amp;read=0" aria-label="既読も表示：ON" title="既読も表示：ON">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<form class="stars on" method="get" action="/">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="4" data-href="/?rating=4" selected>★4</option>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="btn off" href="/?rating=4&amp;bookmarked=1" aria-label="ブックマークだけ表示：OFF" title="ブックマークだけ表示：OFF">🔖</a>"#
            ),
            "{html}"
        );
        // 条件から外れたカードはその場で隠して「元に戻す」を出し、件数も合わせる（`MARKS_SCRIPT`）
        assert!(
            html.contains(
                r#"<h2 class="count">1 件</h2><div class="sections" data-min-rating="4">"#
            ),
            "{html}"
        );
        assert!(
            html.contains("評価を外しました") && html.contains("ブックマークを外しました"),
            "{html}"
        );
        // 一覧と同じく、カードの印をその場で付け外しできる
        assert!(
            html.contains(r#"data-id="1""#) && html.contains(MARKS_SCRIPT),
            "{html}"
        );

        let view = ListView {
            min: 0,
            rating: Some(4),
            bookmarked: true,
            read: true,
            ..ListView::default()
        };
        let html = filtered_page(
            &[item(2, "2026-09-27T05:00:00.000Z")],
            view,
            &Page::default(),
        );
        assert!(
            html.contains(r#"<div class="sections" data-min-rating="4" data-bookmarked="1">"#),
            "{html}"
        );
        let html = filtered_page(&[], view, &Page::default());
        assert!(
            html.contains(r#"<input type="hidden" name="bookmarked" value="1">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="btn on" href="/?rating=4" aria-label="ブックマークだけ表示：ON""#
            ),
            "{html}"
        );
        assert!(html.contains("該当する記事はありません"), "{html}");
        let view = ListView {
            min: 0,
            bookmarked: true,
            read: true,
            ..ListView::default()
        };
        let html = filtered_page(&[], view, &Page::default());
        assert!(
            html.contains(r#"href="/" aria-label="ブックマークだけ表示：ON""#),
            "{html}"
        );

        // 👁 を OFF にした絞り込みは、既読を隠し（その場でも隠す）、絞り込みを変えても OFF を引き継ぐ
        let view = ListView {
            min: 0,
            rating: Some(4),
            ..ListView::default()
        };
        let html = filtered_page(
            &[item(3, "2026-09-27T05:00:00.000Z")],
            view,
            &Page::default(),
        );
        assert!(
            html.contains(r#"href="/?rating=4" aria-label="既読も表示：OFF""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="sections" data-hide-read="1" data-min-rating="4">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"href="/?rating=4&amp;read=0&amp;bookmarked=1""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="read" value="0">"#),
            "{html}"
        );
        // 一覧と絞り込みを行き来するときは、👁 を行き先の既定に戻す（一覧は OFF、絞り込みは ON）
        let bookmark_off = ListView {
            min: 0,
            bookmarked: true,
            ..ListView::default()
        };
        let html = filtered_page(&[], bookmark_off, &Page::default());
        assert!(
            html.contains(r#"href="/" aria-label="ブックマークだけ表示：ON""#),
            "{html}"
        );
        let from_list = ListView {
            read: true,
            ..ListView::default()
        };
        let html = list_page(&[], &[], from_list, &Page::default());
        assert!(
            html.contains(r#"href="/?bookmarked=1" aria-label="ブックマークだけ表示：OFF""#),
            "{html}"
        );
        // 一覧の評価の選択は、一覧の 👁 を絞り込みへ持ち込まない
        let stars = html.split(r#"<form class="stars""#).nth(1).unwrap();
        let stars = stars.split("</form>").next().unwrap();
        assert!(!stars.contains(r#"name="read""#), "{stars}");
    }

    /// 「絞らない」の選択肢は、閉じた選択では 00・★ と書き、開いた一覧では「-」に戻す。
    #[test]
    fn bar_script_relabels_the_blank_choice() {
        let html = list_page(&[], &[], ListView::default(), &Page::default());
        assert!(html.contains(BAR_SCRIPT), "{html}");
        assert!(
            BAR_SCRIPT.contains("dataset.closed") && BAR_SCRIPT.contains("pointerdown"),
            "{BAR_SCRIPT}"
        );
        // キーボードで開いたとき（Alt+↓・F4・Space・Enter）も「-」にし、Escape・Tab で閉じたら戻す
        assert!(
            ["keydown", "ArrowDown", "F4", "Escape", "Tab"]
                .iter()
                .all(|k| BAR_SCRIPT.contains(k)),
            "{BAR_SCRIPT}"
        );
        let html = filtered_page(
            &[],
            ListView {
                min: 0,
                rating: Some(4),
                read: true,
                ..ListView::default()
            },
            &Page::default(),
        );
        assert!(html.contains(BAR_SCRIPT), "{html}");
    }

    /// 「☆」は評価の無い記事だけに絞る（数字の無い「★」は評価で絞らない）。絞り込みの中では最低点を引き継ぎ、
    /// 一覧と行き来するときは最低点も行き先の既定に戻す（一覧は設定の最低点、絞り込みは 00）。
    #[test]
    fn rating_select_offers_unrated_and_resets_the_score_across_modes() {
        let unrated = ListView {
            min: 0,
            read: true,
            rating: Some(0),
            ..ListView::default()
        };
        let html = filtered_page(
            &[item(4, "2026-09-27T05:00:00.000Z")],
            unrated,
            &Page::default(),
        );
        assert!(
            html.contains(r#"<option value="0" data-href="/?rating=0" selected>☆</option>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="sections" data-unrated="1">"#),
            "{html}"
        );
        assert!(html.contains("f.unrated"), "{html}");
        let scored = ListView { min: 60, ..unrated };
        let html = filtered_page(&[], scored, &Page::default());
        // 絞り込みの中では最低点を引き継ぐ
        assert!(
            html.contains(r#"<option value="3" data-href="/?min=60&amp;rating=3">★3</option>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<form class="min on" method="get" action="/">"#),
            "{html}"
        );
        // 一覧へ戻ると、最低点は設定の最低点に戻る
        assert!(
            html.contains(r#"<option value="" data-href="/" data-closed="★">-</option>"#),
            "{html}"
        );
        // 一覧から絞り込みへ移ると、最低点は 00 になる
        let list = ListView {
            min: 30,
            ..ListView::default()
        };
        let html = list_page(&[], &[], list, &Page::default());
        assert!(
            html.contains(r#"<option value="4" data-href="/?rating=4">★4</option>"#),
            "{html}"
        );
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

    /// 推薦点が LLM の点数と違えば、点数の title に LLM の点数と補正を出す。
    #[test]
    fn card_shows_the_llm_score_behind_the_recommended_score() {
        let mut adjusted = item(1, "2026-09-27T05:00:00.000Z");
        adjusted.score = Some(81);
        adjusted.llm_score = Some(72);
        let html = card(&adjusted, false, &Page::default());
        assert!(
            html.contains(r#"<span class="score" title="LLM 72・補正 +9">81</span>"#),
            "{html}"
        );
        let mut lowered = adjusted.clone();
        lowered.score = Some(60);
        let html = card(&lowered, false, &Page::default());
        assert!(html.contains(r#"title="LLM 72・補正 -12""#), "{html}");
        let html = card(
            &item(2, "2026-09-27T05:00:00.000Z"),
            false,
            &Page::default(),
        );
        assert!(html.contains(r#"<span class="score">80</span>"#), "{html}");
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
