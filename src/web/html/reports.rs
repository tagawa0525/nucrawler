//! 指摘の一覧。

use super::*;

/// 受付箱。状況と種類で絞り、各指摘の対応のフォームは開いたときだけ出す。
pub fn reports_page(
    reports: &[Report],
    counts: &[(ReportStatus, i64)],
    filter: &ReportFilter,
    terms: &[crate::glossary::Entry],
    page: &Page,
) -> String {
    let link = |to: ReportFilter, label: &str| {
        let class = if to == *filter { " class=\"on\"" } else { "" };
        format!(
            "<a{class} href=\"{}\">{label}</a>",
            escape(&reports_href(&to))
        )
    };
    let mut statuses: Vec<String> = counts
        .iter()
        .map(|&(status, n)| {
            let to = ReportFilter {
                status: Some(status),
                ..*filter
            };
            link(to, &format!("{} {n}", status_label(status)))
        })
        .collect();
    statuses.push(link(
        ReportFilter {
            status: None,
            ..*filter
        },
        "すべて",
    ));
    let mut kinds = vec![link(
        ReportFilter {
            kind: None,
            ..*filter
        },
        "すべての種類",
    )];
    kinds.extend(ReportKind::ALL.into_iter().map(|kind| {
        link(
            ReportFilter {
                kind: Some(kind),
                ..*filter
            },
            kind_label(kind),
        )
    }));
    let back = format!(
        "<input type=\"hidden\" name=\"back_status\" value=\"{}\">{}",
        filter.status.map_or("all", ReportStatus::as_str),
        filter
            .kind
            .map(|k| format!(
                "<input type=\"hidden\" name=\"back_kind\" value=\"{}\">",
                k.as_str()
            ))
            .unwrap_or_default()
    );
    let mut sorted_terms: Vec<&crate::glossary::Entry> = terms.iter().collect();
    sorted_terms.sort_by(|a, b| a.term.target.cmp(&b.term.target));
    let mut body = format!(
        "<p class=\"meta\"><a href=\"/settings\">← 設定</a></p><h1>受付箱</h1>\
         <nav class=\"filters\">{}</nav><nav class=\"filters\">{}</nav>",
        statuses.join(" "),
        kinds.join(" ")
    );
    if reports.is_empty() {
        body.push_str("<p class=\"meta\">指摘はありません</p>");
    }
    for r in reports {
        let headline = match r.kind {
            ReportKind::Term => format!(
                "<b>{}</b>{}",
                escape(r.found.as_deref().unwrap_or_default()),
                r.wanted
                    .as_ref()
                    .map(|w| format!(" → {}", escape(w)))
                    .unwrap_or_default()
            ),
            kind => format!(
                "<b>{}</b> {}",
                kind_label(kind),
                escape(r.note.as_deref().unwrap_or_default())
            ),
        };
        let mut detail = Vec::new();
        if let Some(source) = &r.source {
            detail.push(format!("原語 {}", escape(source)));
        }
        if r.kind == ReportKind::Term
            && let Some(note) = &r.note
        {
            detail.push(escape(note));
        }
        let mut handling = vec![
            format!(
                "<a href=\"/articles/{}\">{}</a>",
                r.article_id,
                escape(&r.article_title)
            ),
            format!("受付 {}", crate::jst::format_local(&r.reported_at)),
        ];
        if let Some(at) = &r.resolved_at {
            handling.push(format!(
                "{} {}",
                status_label(r.status),
                crate::jst::format_local(at)
            ));
        }
        if let Some((id, target)) = &r.term {
            handling.push(format!(
                "<a href=\"/glossary#term-{id}\">{}</a>",
                escape(target)
            ));
        }
        if let Some(reply) = &r.reply {
            handling.push(escape(reply));
        }
        let status_options: String = ReportStatus::for_kind(r.kind)
            .iter()
            .map(|&status| {
                format!(
                    "<option value=\"{}\"{}>{}</option>",
                    status.as_str(),
                    if status == r.status { " selected" } else { "" },
                    status_label(status)
                )
            })
            .collect();
        // 訳語を結び付けるのは訳語の指摘だけ
        let term_select = if r.kind == ReportKind::Term {
            let chosen = r.term.as_ref().map(|(id, _)| *id);
            let options: String = sorted_terms
                .iter()
                .map(|e| {
                    format!(
                        "<option value=\"{}\"{}>{}</option>",
                        e.id,
                        if chosen == Some(e.id) {
                            " selected"
                        } else {
                            ""
                        },
                        escape(&e.term.target)
                    )
                })
                .collect();
            format!(
                "<label>訳語（任意）<select name=\"term_id\"><option value=\"\">なし</option>{options}</select></label>"
            )
        } else {
            String::new()
        };
        body.push_str(&format!(
            "<div class=\"inbox\" id=\"report-{id}\"><p>{headline}</p>{detail}\
             <p class=\"meta\">{handling}</p>\
             <details><summary>対応</summary><form method=\"post\" action=\"/reports/{id}\">{back}\
             <label>状況<select name=\"status\">{status_options}</select></label>{term_select}\
             <label>ひとこと（任意）<input class=\"wide\" name=\"reply\" value=\"{reply}\"></label>\
             <button>保存</button></form></details></div>",
            id = r.id,
            detail = if detail.is_empty() {
                String::new()
            } else {
                format!("<p class=\"meta\">{}</p>", detail.join(" ・"))
            },
            handling = handling.join(" ・"),
            reply = escape(r.reply.as_deref().unwrap_or_default()),
        ));
    }
    layout("受付箱", page, &body)
}

/// 受付箱の絞り込みの URL（エスケープ前）。受付中が既定なので状況は省き、すべては `status=all`。
pub(crate) fn reports_href(filter: &ReportFilter) -> String {
    let mut query = Vec::new();
    match filter.status {
        Some(ReportStatus::Pending) => {}
        Some(status) => query.push(format!("status={}", status.as_str())),
        None => query.push("status=all".into()),
    }
    if let Some(kind) = filter.kind {
        query.push(format!("kind={}", kind.as_str()));
    }
    if query.is_empty() {
        "/reports".into()
    } else {
        format!("/reports?{}", query.join("&"))
    }
}

pub(super) fn kind_label(kind: ReportKind) -> &'static str {
    match kind {
        ReportKind::Term => "訳語",
        ReportKind::Translation => "和訳の誤り",
        ReportKind::Digest => "要約の誤り",
        ReportKind::Topic => "トピック",
        ReportKind::Body => "本文の取得漏れ",
        ReportKind::Other => "その他",
    }
}

pub(super) fn status_label(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Pending => "受付中",
        ReportStatus::Added => "追加済",
        ReportStatus::Existing => "登録済",
        ReportStatus::Done => "対応済",
        ReportStatus::Rejected => "却下",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::html::test_support::*;

    /// 受付箱は状況ごとの件数で絞り、指摘・記事・受付日時を出す。対応は畳んでおく。
    #[test]
    fn reports_page_lists_reports_with_filters_and_folded_forms() {
        let mut added = term_report(2, ReportStatus::Added);
        added.term = Some((1, "燃料取替停止（定期検査）".into()));
        added.reply = Some("原語を追加".into());
        added.resolved_at = Some("2026-09-27T03:00:00.000Z".into());
        let counts = [
            (ReportStatus::Pending, 1),
            (ReportStatus::Added, 1),
            (ReportStatus::Existing, 0),
            (ReportStatus::Done, 0),
            (ReportStatus::Rejected, 0),
        ];
        let terms = [entry(
            1,
            &["refueling outage"],
            "燃料取替停止（定期検査）",
            None,
        )];
        let html = reports_page(
            &[term_report(1, ReportStatus::Pending), added],
            &counts,
            &ReportFilter::default(),
            &terms,
            &Page::default(),
        );
        assert!(
            html.contains(r#"href="/settings""#),
            "back to settings: {html}"
        );
        for link in [
            r#"<a href="/reports">受付中 1</a>"#,
            r#"<a href="/reports?status=added">追加済 1</a>"#,
            r#"<a href="/reports?status=existing">登録済 0</a>"#,
            r#"<a href="/reports?status=done">対応済 0</a>"#,
            r#"<a href="/reports?status=rejected">却下 0</a>"#,
            r#"<a class="on" href="/reports?status=all">すべて</a>"#,
            r#"<a class="on" href="/reports?status=all">すべての種類</a>"#,
            r#"<a href="/reports?status=all&amp;kind=term">訳語</a>"#,
            r#"<a href="/reports?status=all&amp;kind=digest">要約の誤り</a>"#,
        ] {
            assert!(html.contains(link), "{link}: {html}");
        }
        assert!(html.contains("<b>給油停止</b> → 燃料取替停止"), "{html}");
        assert!(html.contains("原語 refueling outage"), "{html}");
        assert!(
            html.contains(r#"<a href="/articles/7">見出し&lt;A&gt;</a>"#),
            "{html}"
        );
        assert!(html.contains("受付 2026-09-27 09:00"), "{html}");
        assert!(html.contains("追加済 2026-09-27 12:00"), "{html}");
        assert!(
            html.contains(r#"<a href="/glossary#term-1">燃料取替停止（定期検査）</a>"#),
            "{html}"
        );
        assert!(html.contains("原語を追加"), "{html}");
        assert!(
            html.contains(
                r#"<details><summary>対応</summary><form method="post" action="/reports/1">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="back_status" value="all">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="added" selected>追加済</option>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<option value="1" selected>燃料取替停止（定期検査）</option>"#),
            "{html}"
        );
    }

    /// 訳語以外の指摘は種類と内容を出し、対応は受付中・対応済・却下から選ぶ（訳語は結び付けない）。
    /// 絞り込みは状況と種類を互いに引き継ぐ。
    #[test]
    fn reports_page_shows_other_kinds_and_keeps_both_filters() {
        let counts: Vec<_> = ReportStatus::ALL.into_iter().map(|s| (s, 0)).collect();
        let filter = ReportFilter {
            status: Some(ReportStatus::Pending),
            kind: Some(ReportKind::Digest),
            ..ReportFilter::default()
        };
        let html = reports_page(
            &[other_report(3, ReportKind::Digest)],
            &counts,
            &filter,
            &[],
            &Page::default(),
        );
        assert!(
            html.contains("<b>要約の誤り</b> 数値が&lt;違う&gt;"),
            "{html}"
        );
        let form = &html[html.find(r#"action="/reports/3""#).unwrap()..];
        assert!(
            form.contains(
                r#"<select name="status"><option value="pending" selected>受付中</option><option value="done">対応済</option><option value="rejected">却下</option></select>"#
            ),
            "{form}"
        );
        assert!(!form.contains(r#"name="term_id""#), "{form}");
        assert!(
            form.contains(r#"<input type="hidden" name="back_kind" value="digest">"#),
            "{form}"
        );
        for link in [
            r#"<a class="on" href="/reports?kind=digest">受付中 0</a>"#,
            r#"<a href="/reports?status=all&amp;kind=digest">すべて</a>"#,
            r#"<a href="/reports">すべての種類</a>"#,
            r#"<a class="on" href="/reports?kind=digest">要約の誤り</a>"#,
        ] {
            assert!(html.contains(link), "{link}: {html}");
        }
    }
}
