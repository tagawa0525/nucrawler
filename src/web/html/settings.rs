//! 設定と訳語集の画面。

use super::*;

/// 管理の画面への入口（一覧の ⚙ から入る）。
pub fn settings_page(glossary_terms: usize, pending_reports: i64, page: &Page) -> String {
    let body = format!(
        "<p class=\"meta\"><a href=\"/\">← 一覧</a></p><h1>設定</h1>\
         <ul class=\"menu\"><li><a href=\"/glossary\">訳語集</a> \
         <span class=\"meta\">{glossary_terms} 語</span></li>\
         <li><a href=\"/reports\">受付箱</a> \
         <span class=\"meta\">受付中 {pending_reports} 件</span></li></ul>"
    );
    layout("設定", page, &body)
}

/// 訳語集。訳語ごとに畳み、開いたときだけ編集のフォームを出す（一覧の密度を上げない）。
/// 並びは最初の原語の順（大文字小文字を問わない）。
pub fn glossary_page(entries: &[crate::glossary::Entry], page: &Page) -> String {
    let mut sorted: Vec<&crate::glossary::Entry> = entries.iter().collect();
    sorted.sort_by_cached_key(|e| e.term.sources.first().map(|s| s.to_lowercase()));
    let mut body = format!(
        "<p class=\"meta\"><a href=\"/settings\">← 設定</a></p><h1>訳語集</h1>\
         <details class=\"add\"><summary>＋ 訳語を追加</summary>\
         <form method=\"post\" action=\"/glossary\">{}<button>追加</button></form></details>",
        glossary_fields(None)
    );
    for e in sorted {
        let id = e.id;
        let t = &e.term;
        let abbr = t
            .abbr
            .as_ref()
            .map(|a| format!("（{}）", escape(a)))
            .unwrap_or_default();
        let changed = e
            .changed_at()
            .map(|at| {
                format!(
                    "<p class=\"meta\">変更 {}</p>",
                    crate::jst::format_local(at)
                )
            })
            .unwrap_or_default();
        body.push_str(&format!(
            "<details class=\"term\" id=\"term-{id}\"><summary><b>{}{abbr}</b>\
             <span class=\"meta\">{}</span></summary>\
             <form method=\"post\" action=\"/glossary/{id}\">{}<button>保存</button></form>\
             <form method=\"post\" action=\"/glossary/{id}/delete\" \
             onsubmit=\"return confirm('この訳語を削除しますか')\"><button>削除</button></form>\
             {changed}</details>",
            escape(&t.target),
            escape(&t.sources.join(" / ")),
            glossary_fields(Some(t)),
        ));
    }
    layout("訳語集", page, &body)
}

/// 訳語の入力欄。原語は 1 行に 1 つ。
fn glossary_fields(term: Option<&crate::glossary::Term>) -> String {
    let value = |v: Option<&str>| escape(v.unwrap_or_default());
    format!(
        "<label>訳語<input class=\"wide\" name=\"target\" value=\"{}\" required></label>\
         <label>略語（任意）<input class=\"wide\" name=\"abbr\" value=\"{}\"></label>\
         <label>原語（1 行に 1 つ）<textarea class=\"wide\" name=\"sources\" rows=\"3\" required>{}</textarea></label>\
         <label>メモ（任意）<input class=\"wide\" name=\"note\" value=\"{}\"></label>",
        value(term.map(|t| t.target.as_str())),
        value(term.and_then(|t| t.abbr.as_deref())),
        escape(&term.map(|t| t.sources.join("\n")).unwrap_or_default()),
        value(term.and_then(|t| t.note.as_deref())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::html::test_support::*;

    #[test]
    fn settings_page_leads_to_the_glossary() {
        let html = settings_page(15, 3, &Page::default());
        assert!(html.contains(r#"href="/""#), "back to the list: {html}");
        assert!(html.contains(r#"<a href="/glossary">訳語集</a>"#), "{html}");
        assert!(html.contains("15 語"), "{html}");
        assert!(html.contains(r#"<a href="/reports">受付箱</a>"#), "{html}");
        assert!(html.contains("受付中 3 件"), "{html}");
    }

    /// 訳語は原語の順に並べ、1 行目に訳語（略語）、2 行目に原語を出す。
    /// 編集のフォームと削除は、開いたときだけ出す。
    #[test]
    fn glossary_page_folds_each_term_with_its_form() {
        let entries = [
            entry(2, &["spent fuel", "used fuel"], "使用済燃料", None),
            entry(
                1,
                &["Accident tolerant fuel", "ATF"],
                "事故耐性燃料",
                Some("ATF"),
            ),
        ];
        let html = glossary_page(&entries, &Page::default());
        assert!(
            html.contains(r#"href="/settings""#),
            "back to settings: {html}"
        );
        assert!(
            html.contains(r#"<details class="add"><summary>＋ 訳語を追加</summary><form method="post" action="/glossary">"#),
            "{html}"
        );
        let atf = html.find(r#"id="term-1""#).unwrap();
        let spent = html.find(r#"id="term-2""#).unwrap();
        assert!(atf < spent, "sorted by source: {html}");
        assert!(
            html.contains(
                r#"<details class="term" id="term-1"><summary><b>事故耐性燃料（ATF）</b><span class="meta">Accident tolerant fuel / ATF</span></summary>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<form method="post" action="/glossary/1">"#),
            "{html}"
        );
        assert!(
            html.contains("Accident tolerant fuel\nATF</textarea>"),
            "{html}"
        );
        assert!(html.contains(r#"name="abbr" value="ATF""#), "{html}");
        assert!(html.contains(r#"value="注&lt;記&gt;""#), "{html}");
        assert!(
            html.contains(
                r#"<form method="post" action="/glossary/1/delete" onsubmit="return confirm("#
            ),
            "{html}"
        );
    }
}
