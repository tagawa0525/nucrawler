//! 画面の HTML。I/O を持たない関数だけにして、テストしやすくする。
//! JavaScript は一覧の印の付け外し（`MARKS_SCRIPT`）、上部のバーの選択の「絞らない」の書き分け
//! （`BAR_SCRIPT`）と検索の期間のカレンダー（`CALENDAR_SCRIPT`）にだけ使い、無くても読める。

use crate::db::{
    ArticleDetail, Comment, ListItem, Rating, Report, ReportFilter, ReportKind, ReportStatus,
    TopicUsage, Visibility, Warning,
};

use crate::search::Params;

mod detail;
mod list;
mod reports;
mod search;
mod settings;
#[cfg(test)]
mod test_support;

pub use detail::*;
pub use list::*;
pub use reports::*;
pub use search::*;
pub use settings::*;

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

const STYLE: &str = concat!("\n", include_str!("assets/style.css"));

/// ソースの ID から画面に出す名前へ（`Source::display_name`）。
pub type SourceLabels = std::collections::BTreeMap<String, String>;

/// どのページにも共通の表示の材料。
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    pub warnings: &'a [Warning],
    pub labels: &'a SourceLabels,
    /// 利用者の一覧の既定の最低点（プロファイルが無ければ最低点なし）。一覧のほかの画面の上部のバーが、一覧の既定を指すのに使う
    pub default_min: Option<u8>,
}

impl Default for Page<'_> {
    fn default() -> Self {
        static NONE: SourceLabels = SourceLabels::new();
        Self {
            warnings: &[],
            labels: &NONE,
            default_min: Some(crate::config::WebConfig::default().min_score),
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
/// 新着の途絶えは急ぎでないので、本文を押し下げないよう一番下に出す。
pub fn layout(title: &str, page: &Page, body: &str) -> String {
    let (bottom, top): (Vec<_>, Vec<_>) = page
        .warnings
        .iter()
        .partition(|w| matches!(w, Warning::SourceStale { .. }));
    let banners =
        |ws: Vec<&Warning>| -> String { ws.into_iter().map(|w| warning_banner(w, page)).collect() };
    format!(
        "<!DOCTYPE html>\n<html lang=\"ja\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{} - nucrawler</title><style>{STYLE}</style></head>\
         <body><main>{}{body}{}</main></body></html>\n",
        escape(title),
        banners(top),
        banners(bottom),
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
        // 取得は成功しているので、一覧の selector やフィードの形が変わった疑いがある
        Warning::SourceEmpty { source_id, at } => format!(
            "<div class=\"warn\">⚠ {} の一覧が 0 件でした（{}）</div>",
            escape(page.source(source_id)),
            crate::jst::format_local(at),
        ),
        Warning::SourceDropped {
            source_id,
            total,
            median,
            at,
        } => format!(
            "<div class=\"warn\">⚠ {} の取得件数が減っています（{}）：{total} 件（直近の中央値 {median} 件）</div>",
            escape(page.source(source_id)),
            crate::jst::format_local(at),
        ),
        Warning::SourceStale {
            source_id,
            idle_days,
            typical_gap_days,
        } => format!(
            "<div class=\"warn\">⚠ {} の新着が {idle_days} 日ありません（普段は {typical_gap_days} 日おき）</div>",
            escape(page.source(source_id)),
        ),
        // `LlmError::RateLimited` の表示。上限は失敗ではなく、枠が戻れば次の実行で再開する
        Warning::LlmFailed { error, at, .. } if error.starts_with("usage limit reached") => {
            format!(
                "<div class=\"warn\">⏸ 利用上限に達したため、要約・採点・和訳を止めています（{}）。枠が戻ると次の実行で再開します</div>",
                crate::jst::format_local(at),
            )
        }
        Warning::LlmFailed { error, backend, at } => {
            // 認証切れは利用者にしか直せないので、失敗したバックエンドに合わせて対処を案内する
            let lower = error.to_lowercase();
            let hint = if !(lower.contains("logged in") || lower.contains("authenticat")) {
                ""
            } else if backend == crate::llm::copilot_cli::BACKEND {
                "（copilot の認証が切れているようです。端末で copilot login を実行してログインしてください）"
            } else {
                "（claude の認証が切れているようです。端末で claude を起動してログインしてください）"
            };
            format!(
                "<div class=\"warn\">⚠ 要約・採点・和訳が失敗しています（{}）：{}{hint}</div>",
                crate::jst::format_local(at),
                escape(error)
            )
        }
    }
}

/// 絵文字だけのボタン。名前は読み上げとツールチップに回す。`class` は `btn` に足すクラス（空でもよい）。
/// `href` はエスケープ済みで渡す。
fn button(href: &str, name: &str, emoji: &str, class: &str) -> String {
    let class = if class.is_empty() {
        String::new()
    } else {
        format!(" {class}")
    };
    format!(
        "<a class=\"btn{class}\" href=\"{href}\" aria-label=\"{name}\" title=\"{name}\">{emoji}</a>"
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
                        backend: "claude-cli".into(),
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

    #[test]
    fn shows_empty_and_dropped_source_warnings() {
        let html = layout(
            "一覧",
            &Page {
                warnings: &[
                    Warning::SourceEmpty {
                        source_id: "hepco".into(),
                        at: "2026-09-27T00:00:00.000Z".into(),
                    },
                    Warning::SourceDropped {
                        source_id: "fepc".into(),
                        total: 5,
                        median: 30,
                        at: "2026-09-27T01:00:00.000Z".into(),
                    },
                ],
                ..Page::default()
            },
            "",
        );
        assert!(
            html.contains("⚠ hepco の一覧が 0 件でした（2026-09-27 09:00）"),
            "{html}"
        );
        assert!(
            html.contains(
                "⚠ fepc の取得件数が減っています（2026-09-27 10:00）：5 件（直近の中央値 30 件）"
            ),
            "{html}"
        );
    }

    /// 新着の途絶えは急ぎでないので、本文の後（ページの一番下）に出す。ほかの警告は上のまま。
    #[test]
    fn shows_stale_source_warning_below_body() {
        let html = layout(
            "一覧",
            &Page {
                warnings: &[
                    Warning::SourceFailing {
                        source_id: "nei".into(),
                        error: "HTTP 403".into(),
                        at: "2026-09-27T00:00:00.000Z".into(),
                    },
                    Warning::SourceStale {
                        source_id: "nrc-news".into(),
                        idle_days: 13,
                        typical_gap_days: 2,
                    },
                ],
                ..Page::default()
            },
            "<p>body</p>",
        );
        let stale = html
            .find("⚠ nrc-news の新着が 13 日ありません（普段は 2 日おき）")
            .expect(&html);
        let failing = html.find("HTTP 403").expect(&html);
        let body = html.find("<p>body</p>").expect(&html);
        assert!(failing < body && body < stale, "{html}");
    }

    /// 利用上限は失敗ではなく一時停止なので、失敗とは書かず再開の見込みを示す。
    #[test]
    fn usage_limit_is_shown_as_paused_not_failed() {
        let error = crate::llm::LlmError::RateLimited {
            resets_at: Some(1_790_000_000),
        }
        .to_string();
        let html = layout(
            "一覧",
            &Page {
                warnings: &[Warning::LlmFailed {
                    error,
                    backend: "claude-cli".into(),
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
                    backend: "claude-cli".into(),
                    at: "2026-09-27T01:00:00.000Z".into(),
                }],
                ..Page::default()
            },
            "",
        );
        assert!(html.contains("ログイン"), "{html}");
    }

    /// 認証切れの対処は、失敗した呼び出しのバックエンドに合わせて案内する。
    #[test]
    fn auth_hint_follows_the_backend() {
        let html = layout(
            "一覧",
            &Page {
                warnings: &[Warning::LlmFailed {
                    error: "llm process exited with exit status: 1: Not logged in".into(),
                    backend: "copilot-cli".into(),
                    at: "2026-09-27T01:00:00.000Z".into(),
                }],
                ..Page::default()
            },
            "",
        );
        assert!(html.contains("copilot login"), "{html}");
        assert!(!html.contains("claude"), "{html}");
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
            ..Page::default()
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
}
