//! 画面の HTML。I/O を持たない関数だけにして、テストしやすくする。
//! JavaScript は一覧のスワイプ（`SWIPE_SCRIPT`）と検索の期間のカレンダー（`CALENDAR_SCRIPT`）に
//! だけ使い、無くても読める。

use crate::db::{
    ArticleDetail, Comment, ListItem, Report, ReportFilter, ReportKind, ReportStatus, TopicUsage,
    Visibility, Warning,
};

use crate::search::Params;

mod detail;
mod list;
mod reports;
mod search;
#[cfg(test)]
mod test_support;

pub use detail::*;
pub use list::*;
pub use reports::*;
pub use search::*;

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

const STYLE: &str = concat!("\n", include_str!("assets/style.css"));

/// ソースの ID から画面に出す名前へ（`Source::display_name`）。
pub type SourceLabels = std::collections::BTreeMap<String, String>;

/// どのページにも共通の表示の材料。
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    pub warnings: &'a [Warning],
    pub labels: &'a SourceLabels,
}

impl Default for Page<'_> {
    fn default() -> Self {
        static NONE: SourceLabels = SourceLabels::new();
        Self {
            warnings: &[],
            labels: &NONE,
        }
    }
}

impl Page<'_> {
    /// ソースの表示名（設定に無い ID はそのまま）。
    fn source<'s>(&'s self, id: &'s str) -> &'s str {
        self.labels.get(id).map_or(id, String::as_str)
    }
}

/// 全ページ共通の外枠（スマホ向けの 1 カラム、警告のバナー）。
pub fn layout(title: &str, page: &Page, body: &str) -> String {
    let banners: String = page
        .warnings
        .iter()
        .map(|w| warning_banner(w, page))
        .collect();
    format!(
        "<!DOCTYPE html>\n<html lang=\"ja\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{} - nucrawler</title><style>{STYLE}</style></head>\
         <body><main>{banners}{body}</main></body></html>\n",
        escape(title)
    )
}

fn warning_banner(w: &Warning, page: &Page) -> String {
    match w {
        Warning::SourceFailing {
            source_id,
            error,
            at,
        } => format!(
            "<div class=\"warn\">⚠ {} の取得に失敗しています（{}）：{}</div>",
            escape(page.source(source_id)),
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

/// 絵文字だけのボタン。名前は読み上げとツールチップに回す。
/// `state` があれば切り替えとして ON（緑）/ OFF（赤）を示す。`href` はエスケープ済みで渡す。
fn button(href: &str, name: &str, emoji: &str, state: Option<bool>) -> String {
    let (class, label) = match state {
        None => (String::new(), name.to_string()),
        Some(on) => {
            let (class, state) = if on { ("on", "ON") } else { ("off", "OFF") };
            (format!(" {class}"), format!("{name}：{state}"))
        }
    };
    format!(
        "<a class=\"btn{class}\" href=\"{href}\" aria-label=\"{label}\" title=\"{label}\">{emoji}</a>"
    )
}

/// 見出し。空だとリンクが押せなくなるので、和文の見出し、原題、URL の順に空でないものを使う。
pub(crate) fn display_title<'a>(title_ja: Option<&'a str>, i: &'a ListItem) -> &'a str {
    [title_ja.unwrap_or(""), &i.title]
        .into_iter()
        .find(|t| !t.trim().is_empty())
        .unwrap_or(&i.url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::html::test_support::*;

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
            ListView::default(),
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
        let html = detail_page(&d, &Notes::default(), DetailView::default(), &page);
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
            ListView::default(),
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
        let html = detail_page(
            &d,
            &Notes::default(),
            DetailView::default(),
            &Page::default(),
        );
        assert!(
            html.contains(&format!("<h1>{}</h1>", escape(&d.item.title))),
            "{html}"
        );
    }

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
