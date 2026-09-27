//! 画面の HTML。I/O を持たない関数だけにして、テストしやすくする。JavaScript は使わない。

use crate::db::{ArticleDetail, ListItem, Warning};

/// HTML の特殊文字を実体参照にする。
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// 一覧を「前回の訪問の後に届いた記事」と「それより前の未読の記事」に分ける。
/// `boundary`（`Db::begin_visit` の区切り）が無ければ（初回）、すべてを前者にする。
pub fn split_sections(
    items: Vec<ListItem>,
    boundary: Option<&str>,
) -> (Vec<ListItem>, Vec<ListItem>) {
    let Some(boundary) = boundary else {
        return (items, Vec::new());
    };
    let (new, earlier): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|i| i.fetched_at.as_str() > boundary);
    (new, earlier.into_iter().filter(|i| !i.read).collect())
}

const STYLE: &str = "
body { font-family: system-ui, sans-serif; margin: 0; background: #f6f6f4; color: #1d1d1b; }
main { max-width: 42rem; margin: 0 auto; padding: 0.75rem; }
a { color: #0b57a4; }
h1 { font-size: 1.3rem; } h2 { font-size: 1.05rem; margin-top: 1.5rem; }
.card { background: #fff; border-radius: 0.6rem; padding: 0.8rem; margin: 0.6rem 0;
  box-shadow: 0 1px 2px rgba(0,0,0,.08); }
.card a.title { font-weight: 600; text-decoration: none; }
.meta { color: #666; font-size: 0.8rem; margin: 0.3rem 0; }
.score { display: inline-block; min-width: 2.2rem; text-align: center; border-radius: 0.4rem;
  background: #0b57a4; color: #fff; font-weight: 700; margin-right: 0.4rem; }
.read { opacity: 0.6; }
.warn { background: #fff3cd; border-left: 4px solid #d39e00; padding: 0.5rem 0.75rem; margin: 0.4rem 0;
  font-size: 0.85rem; }
.actions form { display: inline; }
.actions button { font-size: 1.1rem; padding: 0.4rem 0.9rem; margin: 0.2rem; border-radius: 0.5rem;
  border: 1px solid #bbb; background: #fff; }
.actions button.on { background: #0b57a4; color: #fff; }
.versions a { margin-right: 0.6rem; font-size: 0.85rem; }
.translation p { line-height: 1.7; }
";

/// 全ページ共通の外枠（スマホ向けの 1 カラム、警告のバナー）。
pub fn layout(title: &str, warnings: &[Warning], body: &str) -> String {
    let banners: String = warnings.iter().map(warning_banner).collect();
    format!(
        "<!DOCTYPE html>\n<html lang=\"ja\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{} - nucrawler</title><style>{STYLE}</style></head>\
         <body><main>{banners}{body}</main></body></html>\n",
        escape(title)
    )
}

fn warning_banner(w: &Warning) -> String {
    match w {
        Warning::SourceFailing {
            source_id,
            error,
            at,
        } => format!(
            "<div class=\"warn\">⚠ ソース {} の取得に失敗しています（{}）：{}</div>",
            escape(source_id),
            crate::jst::format_local(at),
            escape(error)
        ),
        // `LlmError::RateLimited` の表示。上限は失敗ではなく、枠が戻れば次の実行で再開する
        Warning::LlmFailed { error, at } if error.starts_with("usage limit reached") => format!(
            "<div class=\"warn\">⏸ 利用上限に達したため、要約・採点・和訳を止めています（{}）。枠が戻ると次の実行で再開します</div>",
            crate::jst::format_local(at),
        ),
        Warning::LlmFailed { error, at } => {
            // 認証切れは利用者にしか直せないので、対処を案内する
            let lower = error.to_lowercase();
            let hint = if lower.contains("logged in") || lower.contains("authenticat") {
                "（claude の認証が切れているようです。端末で claude を起動してログインしてください）"
            } else {
                ""
            };
            format!(
                "<div class=\"warn\">⚠ 要約・採点・和訳が失敗しています（{}）：{}{hint}</div>",
                crate::jst::format_local(at),
                escape(error)
            )
        }
    }
}

pub fn list_page(
    new: &[ListItem],
    earlier: &[ListItem],
    show_all: bool,
    warnings: &[Warning],
) -> String {
    let mut body = String::from("<h1>nucrawler</h1>");
    let toggle = if show_all {
        "<a href=\"/\">おすすめだけ表示</a>"
    } else {
        "<a href=\"/?all=1\">すべて表示（👎・低い点・未採点を含む）</a>"
    };
    body.push_str(&format!("<p class=\"meta\">{toggle}</p>"));
    body.push_str("<h2>前回から</h2>");
    if new.is_empty() {
        body.push_str("<p class=\"meta\">新しい記事はありません</p>");
    }
    body.extend(new.iter().map(card));
    if !earlier.is_empty() {
        body.push_str("<h2>過去の未読</h2>");
        body.extend(earlier.iter().map(card));
    }
    layout("一覧", warnings, &body)
}

fn card(i: &ListItem) -> String {
    let title = display_title(i.title_ja.as_deref(), i);
    let score = i
        .score
        .map_or_else(String::new, |s| format!("<span class=\"score\">{s}</span>"));
    let lock = if i.locked_by.is_empty() {
        String::new()
    } else {
        format!(" 🔒 {}限定", escape(&i.locked_by.join("・")))
    };
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
        "<div class=\"card{read}\">{score}<a class=\"title\" href=\"/articles/{id}\">{title}</a>\
         <div class=\"meta\">{source} ・{at}{lock}{translation}</div>{summary}</div>",
        read = if i.read { " read" } else { "" },
        id = i.article_id,
        title = escape(title),
        source = escape(&i.source_id),
        at = crate::jst::format_local(&i.at),
    )
}

/// 見出し。空だとリンクが押せなくなるので、和文の見出し、原題、URL の順に空でないものを使う。
fn display_title<'a>(title_ja: Option<&'a str>, i: &'a ListItem) -> &'a str {
    [title_ja.unwrap_or(""), &i.title]
        .into_iter()
        .find(|t| !t.trim().is_empty())
        .unwrap_or(&i.url)
}

/// 詳細画面の表示の選択。
#[derive(Debug, Clone, Copy, Default)]
pub struct DetailView {
    /// 表示する digest の版（無ければ最新）
    pub digest: Option<i64>,
    /// 全文和訳を表示する（`translation` があればその版、無ければ最新）
    pub show_translation: bool,
    pub translation: Option<i64>,
}

pub fn detail_page(d: &ArticleDetail, view: DetailView, warnings: &[Warning]) -> String {
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
        escape(&i.source_id),
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
            "<p><span class=\"score\">{score}</span>{}</p>",
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
    body.push_str(&feedback_forms(id, i.feedback));
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
    layout(&title, warnings, &body)
}

fn feedback_forms(id: i64, current: Option<crate::db::Feedback>) -> String {
    use crate::db::Feedback;
    let button = |value: &str, label: &str, on: bool| {
        format!(
            "<form method=\"post\" action=\"/articles/{id}/feedback\">\
             <button name=\"kind\" value=\"{value}\"{}>{label}</button></form>",
            if on { " class=\"on\"" } else { "" }
        )
    };
    format!(
        "<div class=\"actions\">{}{}</div>",
        button("up", "👍", current == Some(Feedback::Up)),
        button("down", "👎", current == Some(Feedback::Down))
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
    use crate::db::{ArtifactVersion, Feedback};

    fn item(id: i64, fetched_at: &str) -> ListItem {
        ListItem {
            article_id: id,
            source_id: "wnn".into(),
            url: format!("https://e.com/{id}"),
            title: format!("Title {id}"),
            lang: "en".into(),
            at: "2026-09-26T00:00:00.000Z".into(),
            fetched_at: fetched_at.into(),
            title_ja: Some(format!("見出し{id}")),
            summary_ja: Some("要約<b>".into()),
            lwr_relevant: Some(true),
            score: Some(80),
            reason: Some("理由".into()),
            read: false,
            feedback: None,
            has_translation: false,
            translation_requested: false,
            locked_by: vec![],
        }
    }

    #[test]
    fn escapes_html() {
        assert_eq!(
            escape(r#"<a href="x">&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn splits_new_and_earlier_unread() {
        let mut read = item(3, "2026-09-26T00:00:00.000Z");
        read.read = true;
        let items = vec![
            item(1, "2026-09-27T05:00:00.000Z"),
            item(2, "2026-09-26T00:00:00.000Z"),
            read,
        ];
        let (new, earlier) = split_sections(items.clone(), Some("2026-09-27T00:00:00.000Z"));
        assert_eq!(new.iter().map(|i| i.article_id).collect::<Vec<_>>(), [1]);
        // 前回より前の記事は、未読のものだけを残す
        assert_eq!(
            earlier.iter().map(|i| i.article_id).collect::<Vec<_>>(),
            [2]
        );
        let (new, earlier) = split_sections(items, None);
        assert_eq!(new.len(), 3);
        assert!(earlier.is_empty());
    }

    #[test]
    fn layout_is_mobile_friendly_and_shows_warnings() {
        let html = layout(
            "一覧",
            &Page {
                warnings: &[
                    Warning::SourceFailing {
                        source_id: "nei".into(),
                        error: "HTTP 403".into(),
                        at: "2026-09-27T00:00:00.000Z".into(),
                    },
                    Warning::LlmFailed {
                        error: "llm reported an error (error): Not logged in".into(),
                        at: "2026-09-27T01:00:00.000Z".into(),
                    },
                ],
                ..Page::default()
            },
            "<p>body</p>",
        );
        assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
        assert!(html.contains(r#"name="viewport""#));
        assert!(html.contains("<p>body</p>"));
        assert!(html.contains("nei") && html.contains("HTTP 403"));
        // 認証切れらしいときは、対処を案内する
        assert!(
            html.contains("claude") && html.contains("ログイン"),
            "{html}"
        );
    }

    /// 利用上限は失敗ではなく一時停止なので、失敗とは書かず再開の見込みを示す。
    #[test]
    fn usage_limit_is_shown_as_paused_not_failed() {
        let error = crate::llm::LlmError::RateLimited {
            resets_at: Some(1_790_000_000),
            rate_limit: None,
        }
        .to_string();
        let html = layout(
            "一覧",
            &Page {
                warnings: &[Warning::LlmFailed {
                    error,
                    at: "2026-09-27T01:00:00.000Z".into(),
                }],
                ..Page::default()
            },
            "",
        );
        assert!(html.contains("利用上限"), "{html}");
        assert!(!html.contains("失敗"), "{html}");
    }

    #[test]
    fn auth_hint_ignores_case() {
        let html = layout(
            "一覧",
            &Page {
                warnings: &[Warning::LlmFailed {
                    error: "llm process exited with exit status: 1: Authentication required".into(),
                    at: "2026-09-27T01:00:00.000Z".into(),
                }],
                ..Page::default()
            },
            "",
        );
        assert!(html.contains("ログイン"), "{html}");
    }

    /// ソースは ID ではなく表示名で出す（設定に無い ID はそのまま）。
    #[test]
    fn sources_are_shown_by_label() {
        let labels = SourceLabels::from([("kyuden".to_string(), "九電".to_string())]);
        let page = Page {
            warnings: &[Warning::SourceFailing {
                source_id: "kyuden".into(),
                error: "HTTP 500".into(),
                at: "2026-09-27T00:00:00.000Z".into(),
            }],
            labels: &labels,
        };
        let mut kyuden = item(1, "2026-09-27T05:00:00.000Z");
        kyuden.source_id = "kyuden".into();
        let html = list_page(
            &[kyuden, item(2, "2026-09-27T05:00:00.000Z")],
            &[],
            false,
            &page,
        );
        assert!(!html.contains("kyuden"), "{html}");
        assert_eq!(html.matches("九電").count(), 2, "card and warning: {html}");
        assert!(
            html.contains("wnn"),
            "unknown ids fall back to the id: {html}"
        );

        let mut d = detail();
        d.item.source_id = "kyuden".into();
        let html = detail_page(&d, DetailView::default(), &page);
        assert!(!html.contains("kyuden"), "{html}");
    }

    /// 見出しが空だとリンクが押せなくなるので、原題、それも空なら URL を出す。
    #[test]
    fn blank_titles_fall_back() {
        let mut blank_ja = item(1, "2026-09-26T00:00:00.000Z");
        blank_ja.title_ja = Some("  ".into());
        let mut blank_both = item(2, "2026-09-26T00:00:00.000Z");
        blank_both.title_ja = None;
        blank_both.title = String::new();
        let html = list_page(
            &[blank_ja, blank_both.clone()],
            &[],
            false,
            &Page::default(),
        );
        assert!(html.contains(">Title 1</a>"), "{html}");
        assert!(
            html.contains(&format!(">{}</a>", escape(&blank_both.url))),
            "{html}"
        );

        let mut d = detail();
        for v in &mut d.digests {
            v.payload["title_ja"] = serde_json::json!("");
        }
        let html = detail_page(&d, DetailView::default(), &Page::default());
        assert!(
            html.contains(&format!("<h1>{}</h1>", escape(&d.item.title))),
            "{html}"
        );
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
            false,
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

    fn detail() -> ArticleDetail {
        let digest = |id: i64, model: &str, title: &str| ArtifactVersion {
            id,
            backend: "claude-cli".into(),
            model: model.into(),
            prompt_version: 1,
            created_at: "2026-09-26T00:00:00.000Z".into(),
            payload: serde_json::json!({
                "title_ja": title, "summary_ja": "要約", "points_ja": ["要点A", "要点B"],
                "implications_ja": "示唆", "lwr_relevant": true, "topics": ["燃料"],
            }),
        };
        let mut i = item(7, "2026-09-26T00:00:00.000Z");
        i.feedback = Some(Feedback::Up);
        ArticleDetail {
            item: i,
            digests: vec![digest(11, "opus", "新版"), digest(10, "sonnet", "旧版")],
            translations: vec![],
            has_body: true,
        }
    }

    #[test]
    fn detail_page_shows_latest_digest_and_version_links() {
        let html = detail_page(&detail(), DetailView::default(), &Page::default());
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
        // 英語で本文があり和訳が無いので、依頼ボタンを出す
        assert!(
            html.contains(r#"action="/articles/7/translation-request""#),
            "{html}"
        );
    }

    #[test]
    fn detail_page_switches_digest_version() {
        let view = DetailView {
            digest: Some(10),
            ..DetailView::default()
        };
        let html = detail_page(&detail(), view, &Page::default());
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
        let html = detail_page(&d, DetailView::default(), &Page::default());
        assert!(
            html.contains(r#"href="/articles/7?view=translation""#),
            "{html}"
        );
        assert!(!html.contains("第一段落"));
        let view = DetailView {
            show_translation: true,
            ..DetailView::default()
        };
        let html = detail_page(&d, view, &Page::default());
        assert!(html.contains("<p>第一段落。</p>"), "{html}");
        assert!(html.contains("第二段落&lt;script&gt;"));
        assert!(!html.contains(r#"translation-request"#));
    }

    #[test]
    fn detail_page_shows_waiting_when_requested() {
        let mut d = detail();
        d.item.translation_requested = true;
        let html = detail_page(&d, DetailView::default(), &Page::default());
        assert!(html.contains("和訳待ち"));
        assert!(!html.contains(r#"action="/articles/7/translation-request""#));
    }
}
