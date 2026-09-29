//! 記事の詳細（要約・和訳・操作・指摘・コメント）。

use super::*;

/// 詳細に並べる、記事への書き込み（指摘とコメント）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Notes<'a> {
    pub reports: &'a [Report],
    pub comments: &'a [Comment],
}

/// 詳細画面の表示の選択。
#[derive(Debug, Clone, Copy, Default)]
pub struct DetailView {
    /// 表示する digest の版（無ければ最新）
    pub digest: Option<i64>,
    /// 全文和訳を表示する（`translation` があればその版、無ければ最新）
    pub show_translation: bool,
    pub translation: Option<i64>,
    /// 訳語の指摘を受け付けた直後
    pub reported: bool,
}

pub fn detail_page(d: &ArticleDetail, notes: &Notes, view: DetailView, page: &Page) -> String {
    let i = &d.item;
    let id = i.article_id;
    let digest = view
        .digest
        .and_then(|v| d.digests.iter().find(|x| x.id == v))
        .or(d.digests.first());
    let field = |key: &str| {
        digest
            .and_then(|x| x.payload[key].as_str())
            .map(str::to_string)
    };
    let title = display_title(field("title_ja").as_deref(), i).to_string();
    let mut body = format!(
        "<p class=\"meta\"><a href=\"/\">← 一覧</a></p><h1>{}</h1>",
        escape(&title)
    );
    body.push_str(&format!(
        "<p class=\"meta\">{} ・{} ・<a href=\"{}\">原文</a>{}</p>",
        escape(page.source(&i.source_id)),
        crate::jst::format_local(&i.at),
        escape(&i.url),
        if i.locked_by.is_empty() {
            String::new()
        } else {
            format!(" 🔒 {}限定", escape(&i.locked_by.join("・")))
        }
    ));
    if let Some(score) = i.score {
        body.push_str(&format!(
            "<p><span class=\"score\">{score}</span>{}{}</p>",
            super::list::matches(i),
            escape(i.reason.as_deref().unwrap_or(""))
        ));
    }
    if let Some(summary) = field("summary_ja") {
        body.push_str(&format!("<p>{}</p>", escape(&summary)));
    }
    if let Some(points) = digest.and_then(|x| x.payload["points_ja"].as_array()) {
        body.push_str("<ul>");
        for p in points.iter().filter_map(|p| p.as_str()) {
            body.push_str(&format!("<li>{}</li>", escape(p)));
        }
        body.push_str("</ul>");
    }
    if let Some(implications) = field("implications_ja").filter(|s| !s.is_empty()) {
        body.push_str(&format!(
            "<p><b>日本の軽水炉への示唆：</b>{}</p>",
            escape(&implications)
        ));
    }
    if let Some(topics) = digest.and_then(|x| x.payload["topics"].as_array()) {
        let topics: Vec<&str> = topics.iter().filter_map(|t| t.as_str()).collect();
        body.push_str(&format!(
            "<p class=\"meta\">トピック：{}</p>",
            escape(&topics.join("、"))
        ));
    }
    body.push_str(&feedback_forms(id, i.feedback, i.bookmarked));
    if d.digests.len() > 1 {
        body.push_str("<p class=\"versions meta\">要約の版：");
        for v in &d.digests {
            body.push_str(&format!(
                "<a href=\"/articles/{id}?digest={}\">{} {}</a>",
                v.id,
                escape(&v.model),
                crate::jst::format_local(&v.created_at)
            ));
        }
        body.push_str("</p>");
    }
    body.push_str(&translation_section(d, view));
    let has_japanese = !d.digests.is_empty() || !d.translations.is_empty();
    body.push_str(&comment_section(id, notes.comments, view));
    body.push_str(&report_section(id, notes.reports, has_japanese, view));
    layout(&title, page, &body)
}

/// コメントの欄。コメントは改行を保って並べ、書く欄と自分のコメントの編集は畳んでおく。
fn comment_section(id: i64, comments: &[Comment], view: DetailView) -> String {
    let back = if view.show_translation {
        "<input type=\"hidden\" name=\"view\" value=\"translation\">"
    } else {
        ""
    };
    // 既定は非公開（チェックしたときだけ公開）
    let public = |checked: bool| {
        format!(
            "<label><input type=\"checkbox\" name=\"public\" value=\"1\"{}> 公開する</label>",
            if checked { " checked" } else { "" }
        )
    };
    let mut out = String::from("<section class=\"comments\" id=\"comments\">");
    for c in comments {
        let body = escape(&c.body).replace('\n', "<br>");
        let updated = if c.updated_at != c.created_at {
            format!("（更新 {}）", crate::jst::format_local(&c.updated_at))
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<div class=\"comment\"><p>{body}</p><p class=\"meta\">{} ・{}{updated}</p>",
            match c.visibility {
                Visibility::Private => "🔒 非公開",
                Visibility::Public => "公開",
            },
            crate::jst::format_local(&c.created_at)
        ));
        if c.mine {
            out.push_str(&format!(
                "<details><summary>編集</summary>\
                 <form method=\"post\" action=\"/comments/{cid}\">{back}\
                 <textarea class=\"wide\" name=\"body\" rows=\"3\" required>{text}</textarea>{public}\
                 <button>保存</button></form>\
                 <form method=\"post\" action=\"/comments/{cid}/delete\" \
                 onsubmit=\"return confirm('このコメントを削除しますか')\">{back}<button>削除</button></form>\
                 </details>",
                cid = c.id,
                text = escape(&c.body),
                public = public(c.visibility == Visibility::Public),
            ));
        }
        out.push_str("</div>");
    }
    out.push_str(&format!(
        "<details class=\"comment-add\"><summary>コメントを書く</summary>\
         <form method=\"post\" action=\"/articles/{id}/comments\">{back}\
         <textarea class=\"wide\" name=\"body\" rows=\"3\" required></textarea>{}\
         <button>保存</button></form></details></section>",
        public(false)
    ));
    out
}

/// 指摘の欄。これまでの指摘を対応状況とともに小さく並べ、訳語の指摘とその他の指摘の
/// フォームは畳んでおく（読む画面の密度を上げない）。訳語の指摘は日本語（要約か和訳）があるときだけ。
fn report_section(id: i64, reports: &[Report], has_japanese: bool, view: DetailView) -> String {
    let field = |name: &str, label: &str, extra: &str| {
        format!("<label>{label}<input class=\"wide\" name=\"{name}\"{extra}></label>")
    };
    let back = if view.show_translation {
        "<input type=\"hidden\" name=\"view\" value=\"translation\">"
    } else {
        ""
    };
    let mut out = String::from("<section class=\"reports\" id=\"reports\">");
    if view.reported {
        out.push_str("<p class=\"meta\">指摘を受け付けました</p>");
    }
    for r in reports {
        out.push_str(&format!(
            "<p class=\"meta\">{}（{}）</p>",
            report_summary(r),
            status_label(r.status)
        ));
    }
    if has_japanese {
        out.push_str(&format!(
            "<details class=\"report\" id=\"term-report\"><summary>訳語の指摘</summary>\
             <form method=\"post\" action=\"/articles/{id}/report\">\
             <input type=\"hidden\" name=\"kind\" value=\"term\">{back}{}{}{}\
             <label>メモ（任意）<textarea class=\"wide\" name=\"note\" rows=\"2\"></textarea></label>\
             <button>送る</button></form></details>",
            field("found", "気になった訳", " required"),
            field("wanted", "希望する訳（任意）", ""),
            field("source", "原語（任意）", ""),
        ));
    }
    let kinds: String = ReportKind::ALL
        .into_iter()
        .filter(|&k| k != ReportKind::Term)
        .map(|k| {
            format!(
                "<option value=\"{}\">{}</option>",
                k.as_str(),
                kind_label(k)
            )
        })
        .collect();
    out.push_str(&format!(
        "<details class=\"report\" id=\"other-report\"><summary>その他の指摘</summary>\
         <form method=\"post\" action=\"/articles/{id}/report\">{back}\
         <label>種類<select name=\"kind\">{kinds}</select></label>\
         <label>内容<textarea class=\"wide\" name=\"note\" rows=\"3\" required></textarea></label>\
         <button>送る</button></form></details></section>"
    ));
    out
}

/// 指摘の要旨（エスケープ済み）。訳語は「気になった訳 → 希望する訳」、ほかは「種類：内容」。
fn report_summary(r: &Report) -> String {
    match r.kind {
        ReportKind::Term => format!(
            "{}{}",
            escape(r.found.as_deref().unwrap_or_default()),
            r.wanted
                .as_ref()
                .map(|w| format!(" → {}", escape(w)))
                .unwrap_or_default()
        ),
        kind => format!(
            "{}：{}",
            kind_label(kind),
            escape(r.note.as_deref().unwrap_or_default())
        ),
    }
}

/// 👍/👎 とブックマーク。ブックマーク済みなら、同じボタンで外す。
fn feedback_forms(id: i64, current: Option<crate::db::Feedback>, bookmarked: bool) -> String {
    use crate::db::Feedback;
    let button = |value: &str, label: &str, on: bool| {
        format!(
            "<form method=\"post\" action=\"/articles/{id}/feedback\">\
             <button name=\"kind\" value=\"{value}\"{}>{label}</button></form>",
            if on { " class=\"on\"" } else { "" }
        )
    };
    format!(
        "<div class=\"actions\">{}{}{}</div>",
        button("up", "👍", current == Some(Feedback::Up)),
        button("down", "👎", current == Some(Feedback::Down)),
        if bookmarked {
            button("unbookmark", "🔖", true)
        } else {
            button("bookmark", "🔖", false)
        }
    )
}

fn translation_section(d: &ArticleDetail, view: DetailView) -> String {
    let id = d.item.article_id;
    if view.show_translation {
        let chosen = view
            .translation
            .and_then(|v| d.translations.iter().find(|x| x.id == v))
            .or(d.translations.first());
        if let Some(t) = chosen {
            let paragraphs: String = t.payload["body_ja"]
                .as_str()
                .unwrap_or_default()
                .split("\n\n")
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| format!("<p>{}</p>", escape(p)))
                .collect();
            let mut out = format!("<h2>全文和訳</h2><div class=\"translation\">{paragraphs}</div>");
            if d.translations.len() > 1 {
                out.push_str("<p class=\"versions meta\">和訳の版：");
                for v in &d.translations {
                    out.push_str(&format!(
                        "<a href=\"/articles/{id}?view=translation&amp;translation={}\">{} {}</a>",
                        v.id,
                        escape(&v.model),
                        crate::jst::format_local(&v.created_at)
                    ));
                }
                out.push_str("</p>");
            }
            return out;
        }
    }
    if d.item.has_translation && !d.translations.is_empty() {
        format!("<p><a href=\"/articles/{id}?view=translation\">全文和訳を読む</a></p>")
    } else if d.item.translation_requested {
        "<p class=\"meta\">和訳待ち（次の依頼処理で和訳します）</p>".to_string()
    } else if d.can_request_translation() {
        format!(
            "<div class=\"actions\"><form method=\"post\" action=\"/articles/{id}/translation-request\">\
             <button>全文和訳を依頼</button></form></div>"
        )
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ArtifactVersion;
    use crate::web::html::test_support::*;

    #[test]
    fn detail_page_shows_latest_digest_and_version_links() {
        let html = detail_page(
            &detail(),
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains("新版") && !html.contains("<h1>旧版"),
            "{html}"
        );
        assert!(html.contains("要点A") && html.contains("示唆") && html.contains("燃料"));
        assert!(
            html.contains(r#"href="https://e.com/7""#),
            "link to original"
        );
        assert!(
            html.contains(r#"href="/articles/7?digest=10""#),
            "switch to older version"
        );
        assert!(html.contains("sonnet"));
        assert!(html.contains(r#"action="/articles/7/feedback""#));
        // 評価は 1〜5 の星で、今の評価（4）まで塗る。今の評価を押すと評価なしに戻る
        assert!(html.contains(r#"action="/articles/7/rating""#), "{html}");
        assert!(
            html.contains(
                r#"<button name="value" value="3" title="3 どちらでもない" class="on">★</button>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<button name="value" value="" title="4 読んでよかった（押すと評価なし）" class="on">★</button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button name="value" value="5" title="5 必読">☆</button>"#),
            "{html}"
        );
        assert!(!html.contains("👍") && !html.contains("👎"), "{html}");
        assert!(
            html.contains(r#"<button name="kind" value="bookmark">🔖</button>"#),
            "{html}"
        );
        // 英語で本文があり和訳が無いので、依頼ボタンを出す
        assert!(
            html.contains(r#"action="/articles/7/translation-request""#),
            "{html}"
        );
    }

    #[test]
    fn detail_page_shows_the_terms_the_score_matched() {
        let mut d = detail();
        d.item.matched = vec!["燃料".into()];
        d.item.excluded = vec!["核融合".into()];
        let html = detail_page(
            &d,
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(
                "<span class=\"match\">燃料</span><span class=\"match excluded\">除外 核融合</span>"
            ),
            "{html}"
        );
    }

    /// ブックマーク済みなら、同じボタンで外す。
    #[test]
    fn detail_page_offers_to_remove_the_bookmark() {
        let mut d = detail();
        d.item.bookmarked = true;
        let html = detail_page(
            &d,
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(r#"<button name="kind" value="unbookmark" class="on">🔖</button>"#),
            "{html}"
        );
    }

    #[test]
    fn detail_page_switches_digest_version() {
        let view = DetailView {
            digest: Some(10),
            ..DetailView::default()
        };
        let html = detail_page(&detail(), &Notes::default(), view, &Page::default());
        assert!(html.contains("旧版"));
    }

    #[test]
    fn detail_page_shows_translation_or_its_link() {
        let mut d = detail();
        d.item.has_translation = true;
        d.translations = vec![ArtifactVersion {
            id: 20,
            backend: "claude-cli".into(),
            model: "sonnet".into(),
            prompt_version: 1,
            created_at: "2026-09-26T00:00:00.000Z".into(),
            payload: serde_json::json!({"body_ja": "第一段落。\n\n第二段落<script>"}),
        }];
        let html = detail_page(
            &d,
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(r#"href="/articles/7?view=translation""#),
            "{html}"
        );
        assert!(!html.contains("第一段落"));
        let view = DetailView {
            show_translation: true,
            ..DetailView::default()
        };
        let html = detail_page(&d, &Notes::default(), view, &Page::default());
        assert!(html.contains("<p>第一段落。</p>"), "{html}");
        assert!(html.contains("第二段落&lt;script&gt;"));
        assert!(!html.contains(r#"translation-request"#));
    }

    #[test]
    fn detail_page_shows_waiting_when_requested() {
        let mut d = detail();
        d.item.translation_requested = true;
        let html = detail_page(
            &d,
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(html.contains("和訳待ち"));
        assert!(!html.contains(r#"action="/articles/7/translation-request""#));
    }

    /// 訳語の指摘は畳んでおき、開いたときだけフォームを出す。和訳を読んでいれば和訳に戻る。
    #[test]
    fn detail_page_offers_a_folded_term_report() {
        let html = detail_page(
            &detail(),
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(
                r#"<details class="report" id="term-report"><summary>訳語の指摘</summary><form method="post" action="/articles/7/report"><input type="hidden" name="kind" value="term">"#
            ),
            "{html}"
        );
        assert!(html.contains(r#"name="found" required"#), "{html}");
        for name in ["wanted", "source", "note"] {
            assert!(
                html.contains(&format!(r#"name="{name}""#)),
                "{name}: {html}"
            );
        }
        assert!(!html.contains(r#"name="view""#), "{html}");
        assert!(!html.contains("受け付けました"), "{html}");

        let view = DetailView {
            show_translation: true,
            reported: true,
            ..DetailView::default()
        };
        let html = detail_page(&detail(), &Notes::default(), view, &Page::default());
        let reports = &html[html.find(r#"id="reports""#).unwrap()..];
        assert_eq!(
            reports
                .matches(r#"<input type="hidden" name="view" value="translation">"#)
                .count(),
            2,
            "both report forms return to the translation: {html}"
        );
        assert!(html.contains("指摘を受け付けました"), "{html}");
    }

    /// 訳語以外の指摘は種類を選んで内容を書く。これも畳んでおく。
    #[test]
    fn detail_page_offers_a_folded_report_of_other_kinds() {
        let html = detail_page(
            &detail(),
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(
                r#"<details class="report" id="other-report"><summary>その他の指摘</summary><form method="post" action="/articles/7/report">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<select name="kind"><option value="translation">和訳の誤り</option><option value="digest">要約の誤り</option><option value="topic">トピック</option><option value="body">本文の取得漏れ</option><option value="other">その他</option></select>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<textarea class="wide" name="note" rows="3" required></textarea>"#),
            "{html}"
        );
    }

    /// 要約も和訳も無ければ、指摘する訳が無い。ほかの指摘はできる。
    #[test]
    fn detail_page_without_japanese_has_no_term_report() {
        let mut d = detail();
        d.digests.clear();
        let html = detail_page(
            &d,
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(!html.contains("term-report"), "{html}");
        // 本文の取得漏れなどは、要約が無くても指摘できる
        assert!(html.contains(r#"id="other-report""#), "{html}");
    }

    /// 詳細では、その記事への指摘と対応状況を小さく並べる。
    #[test]
    fn detail_page_lists_past_reports_with_their_status() {
        let html = detail_page(
            &detail(),
            &Notes {
                reports: &[term_report(1, ReportStatus::Rejected)],
                ..Notes::default()
            },
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(r#"<p class="meta">給油停止 → 燃料取替停止（却下）</p>"#),
            "{html}"
        );
        let html = detail_page(
            &detail(),
            &Notes {
                reports: &[other_report(2, ReportKind::Body)],
                ..Notes::default()
            },
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(r#"<p class="meta">本文の取得漏れ：数値が&lt;違う&gt;（受付中）</p>"#),
            "{html}"
        );
    }

    fn comment(id: i64, body: &str, visibility: Visibility, mine: bool) -> Comment {
        Comment {
            id,
            body: body.into(),
            visibility,
            mine,
            created_at: "2026-09-27T00:00:00.000Z".into(),
            updated_at: "2026-09-27T00:00:00.000Z".into(),
        }
    }

    /// コメントを書く欄は畳んでおき、指摘の欄より前に置く。既定は非公開（チェックを外したまま）。
    #[test]
    fn detail_page_offers_a_folded_comment_form() {
        let view = DetailView {
            show_translation: true,
            ..DetailView::default()
        };
        let html = detail_page(&detail(), &Notes::default(), view, &Page::default());
        assert!(
            html.contains(
                r#"<details class="comment-add"><summary>コメントを書く</summary><form method="post" action="/articles/7/comments"><input type="hidden" name="view" value="translation">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<textarea class="wide" name="body" rows="3" required></textarea>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<label><input type="checkbox" name="public" value="1"> 公開する</label>"#
            ),
            "{html}"
        );
        let comments = html.find(r#"id="comments""#).unwrap();
        let reports = html.find(r#"id="reports""#).unwrap();
        assert!(comments < reports, "{html}");
    }

    /// コメントは改行を保って出し、公開・非公開を示す。直せるのは自分のコメントだけ。
    #[test]
    fn detail_page_lists_comments_and_lets_authors_edit_them() {
        let mut mine = comment(1, "一行目\n<二行目>", Visibility::Private, true);
        mine.updated_at = "2026-09-27T01:00:00.000Z".into();
        let theirs = comment(2, "共有します", Visibility::Public, false);
        let notes = Notes {
            comments: &[mine, theirs],
            ..Notes::default()
        };
        let html = detail_page(&detail(), &notes, DetailView::default(), &Page::default());
        assert!(html.contains("<p>一行目<br>&lt;二行目&gt;</p>"), "{html}");
        assert!(
            html.contains(
                r#"<p class="meta">🔒 非公開 ・2026-09-27 09:00（更新 2026-09-27 10:00）</p>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<p class="meta">公開 ・2026-09-27 09:00</p>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<form method="post" action="/comments/1">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"required>一行目
&lt;二行目&gt;</textarea><label><input type="checkbox" name="public" value="1"> 公開する</label>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<form method="post" action="/comments/1/delete""#),
            "{html}"
        );
        assert!(!html.contains(r#"action="/comments/2"#), "{html}");
    }
}
