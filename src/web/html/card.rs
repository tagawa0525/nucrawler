//! 記事のカードと、評価・既読・ブックマークの印。一覧・検索・記事の詳細で共有する。

use super::*;

/// 一覧のカードの印（`marks`）を、ページを移らずにその場で付け外しする。既読を隠す一覧
/// （`data-read`・`data-bookmarked`）や、評価で絞った画面（`data-min-rating`・`data-unrated`）では、印を付け外しして
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
    match i.base_score.filter(|llm| *llm != score) {
        Some(llm) => format!(
            "<span class=\"score\" title=\"LLM {llm}・補正 {:+}\">{score}</span>",
            i32::from(score) - i32::from(llm)
        ),
        None => format!("<span class=\"score\">{score}</span>"),
    }
}

/// 同じ報道のグループのほかの記事の数とソース（ソースは重ねない）。グループでなければ空。
fn story_others(i: &ListItem, page: &Page) -> String {
    if i.story_others.is_empty() {
        return String::new();
    }
    let mut sources: Vec<&str> = Vec::new();
    for s in &i.story_others {
        let label = page.source(s);
        if !sources.contains(&label) {
            sources.push(label);
        }
    }
    format!(
        " ・他 {} 件（{}）",
        i.story_others.len(),
        escape(&sources.join("・"))
    )
}

/// 記事のカード。`swipe` なら一覧のカードとして、印（`marks`）を付けてその場で付け外しできるようにする
/// （`MARKS_SCRIPT`）。そうでなければ（検索の結果）、印は見出しの下の行に記号で示す。
pub(super) fn card(i: &ListItem, swipe: bool, page: &Page) -> String {
    let title = i.display_title();
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
         <div class=\"meta\">{source} ・{at}{rating}{bookmarked}{lock}{translation}{story}</div>{matches}{summary}{marks}</div>",
        story = story_others(i, page),
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
    use crate::web::html::test_support::*;

    /// 同じ報道のグループの代表には、ほかの記事の数とソース（重ねずに）を添える。
    #[test]
    fn card_mentions_other_reports_of_the_story() {
        let labels = crate::config::SourceLabels::from([("wnn".to_string(), "WNN".to_string())]);
        let page = Page {
            labels: &labels,
            ..Page::default()
        };
        let mut i = item(1, "2026-09-27T05:00:00.000Z");
        i.story_others = vec!["wnn".into(), "iaea".into(), "wnn".into()];
        let html = card(&i, true, &page);
        assert!(html.contains("他 3 件（WNN・iaea）"), "{html}");
        let html = card(&item(2, "2026-09-27T05:00:00.000Z"), true, &page);
        assert!(!html.contains("他 "), "{html}");
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

    /// 推薦点が LLM の点数と違えば、点数の title に LLM の点数と補正を出す。
    #[test]
    fn card_shows_the_llm_score_behind_the_recommended_score() {
        let mut adjusted = item(1, "2026-09-27T05:00:00.000Z");
        adjusted.score = Some(81);
        adjusted.base_score = Some(72);
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
