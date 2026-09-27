//! 画面の HTML。I/O を持たない関数だけにして、テストしやすくする。
//! JavaScript は一覧のスワイプ（`SWIPE_SCRIPT`）と検索の期間のカレンダー（`CALENDAR_SCRIPT`）に
//! だけ使い、無くても読める。

use crate::db::{
    ArticleDetail, Comment, ListItem, Report, ReportFilter, ReportKind, ReportStatus, TopicUsage,
    Visibility, Warning,
};
use crate::search::Params;

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

fn kind_label(kind: ReportKind) -> &'static str {
    match kind {
        ReportKind::Term => "訳語",
        ReportKind::Translation => "和訳の誤り",
        ReportKind::Digest => "要約の誤り",
        ReportKind::Topic => "トピック",
        ReportKind::Body => "本文の取得漏れ",
        ReportKind::Other => "その他",
    }
}

fn status_label(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Pending => "受付中",
        ReportStatus::Added => "追加済",
        ReportStatus::Existing => "登録済",
        ReportStatus::Done => "対応済",
        ReportStatus::Rejected => "却下",
    }
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

/// 検索画面。`results` が None なら（条件が無いときは）フォームだけを出す。
/// トピックの選択肢は要約に付いている語だけを、軸ごとに付いている数の多い順に並べる。
pub fn search_page(
    params: &Params,
    results: Option<&[ListItem]>,
    vocabulary: &[TopicUsage],
    error: Option<&str>,
    page: &Page,
) -> String {
    let mut body = String::from("<h1>検索</h1><p class=\"meta\"><a href=\"/\">一覧へ</a></p>");
    if let Some(error) = error {
        body.push_str(&format!("<div class=\"warn\">{}</div>", escape(error)));
    }
    body.push_str(&search_form(params, vocabulary, page));
    body.push_str(CALENDAR_SCRIPT);
    if let Some(items) = results {
        if items.is_empty() {
            body.push_str("<p class=\"meta\">該当する記事はありません</p>");
        } else {
            body.push_str(&format!("<h2>{} 件</h2>", items.len()));
            body.extend(items.iter().map(|i| card(i, false, page)));
        }
    }
    layout("検索", page, &body)
}

fn search_form(p: &Params, vocabulary: &[TopicUsage], page: &Page) -> String {
    let text = |name: &str, value: &str, extra: &str| {
        format!("<input name=\"{name}\" value=\"{}\"{extra}>", escape(value))
    };
    let checkbox = |name: &str, value: &str, checked: bool, label: &str| {
        format!(
            "<label><input type=\"checkbox\" name=\"{name}\" value=\"{}\"{}> {}</label> ",
            escape(value),
            if checked { " checked" } else { "" },
            escape(label)
        )
    };
    let select = |name: &str, current: &str, options: &[(&str, &str)]| {
        let options: String = options
            .iter()
            .map(|(value, label)| {
                let selected = if *value == current { " selected" } else { "" };
                format!("<option value=\"{value}\"{selected}>{label}</option>")
            })
            .collect();
        format!("<select name=\"{name}\">{options}</select>")
    };
    // 要約に付いている語だけを、軸ごとに付いている数の多い順に（選んだ語は数によらず出す）
    let mut topics = String::new();
    for facet in crate::topics::Facet::ALL {
        let mut words: Vec<&TopicUsage> = vocabulary
            .iter()
            .filter(|u| u.facet == facet && (u.uses > 0 || p.topics.contains(&u.name)))
            .collect();
        words.sort_by_key(|u| std::cmp::Reverse(u.uses));
        if words.is_empty() {
            continue;
        }
        topics.push_str(&format!("<div class=\"meta\">{}</div>", facet.as_str()));
        for u in words {
            let label = format!("{} ({})", u.name, u.uses);
            topics.push_str(&checkbox(
                "topic",
                &u.name,
                p.topics.contains(&u.name),
                &label,
            ));
        }
    }
    // 語彙に無い語（統合した語の別名など）も、選んでいれば残す
    let others: Vec<&String> = p
        .topics
        .iter()
        .filter(|t| vocabulary.iter().all(|u| &u.name != *t))
        .collect();
    if !others.is_empty() {
        topics.push_str("<div class=\"meta\">その他</div>");
        for t in others {
            topics.push_str(&checkbox("topic", t, true, t));
        }
    }
    let sources: String = page
        .labels
        .iter()
        .map(|(id, label)| checkbox("source", id, p.sources.contains(id), label))
        .collect();
    let open = |any: bool| if any { " open" } else { "" };
    format!(
        "<form method=\"get\" action=\"/search\">\
         <p>{q}</p>\
         <p>期間 {since}{since_cal} 〜 {until}{until_cal}</p>\
         <details{topics_open}><summary>トピック</summary>{topics}</details>\
         <details{sources_open}><summary>ソース</summary>{sources}</details>\
         <p>言語 {lang} 並び {sort}</p>\
         <p>{translated}{liked}{unread}{bookmarked}最低点 {min_score}</p>\
         <p><button type=\"submit\">検索</button></p></form>",
        q = text(
            "q",
            &p.q,
            " class=\"wide\" type=\"search\" placeholder=\"語（空白で区切るとすべてを含む）\""
        ),
        since = text("since", &p.since, " size=\"10\" placeholder=\"2026-09\""),
        until = text("until", &p.until, " size=\"10\" placeholder=\"2026-09-30\""),
        since_cal = calendar("since", "開始日"),
        until_cal = calendar("until", "終了日"),
        topics_open = open(!p.topics.is_empty()),
        sources_open = open(!p.sources.is_empty()),
        lang = select(
            "lang",
            &p.lang,
            &[("", "すべて"), ("en", "英語"), ("ja", "日本語")]
        ),
        sort = select(
            "sort",
            &p.sort,
            &[("newest", "新しい順"), ("score", "点数順")]
        ),
        translated = checkbox("translated", "1", p.translated, "和訳あり"),
        liked = checkbox("liked", "1", p.liked, "👍"),
        unread = checkbox("unread", "1", p.unread, "未読"),
        bookmarked = checkbox("bookmarked", "1", p.bookmarked, "🔖"),
        min_score = text(
            "min_score",
            &p.min_score,
            " type=\"number\" min=\"0\" max=\"100\" size=\"3\""
        ),
    )
}

/// 期間の欄の横の 📅。日付の入力を透明にして絵文字に重ね、押すとカレンダーが開く
/// （`CALENDAR_SCRIPT`）。名前を持たないので送られず、選んだ日付は `name` の欄へ入る。
/// キーボードでも操作できるよう、日付の入力はフォーカスでき、読み上げの名前を持つ。
fn calendar(name: &str, label: &str) -> String {
    format!(
        "<label class=\"cal\" title=\"カレンダー\">📅<input type=\"date\" data-for=\"{name}\" \
         aria-label=\"{label}をカレンダーで選ぶ\"></label>"
    )
}

/// 📅 のカレンダーで選んだ日付を、隣の期間の欄に入れる。欄は月だけの指定もできるよう文字の
/// 入力のまま残す。欄が日付ならその日から、そうでなければ（月だけや空なら）今日から開く。
const CALENDAR_SCRIPT: &str = concat!(
    "<script>\n",
    include_str!("assets/calendar.js"),
    "</script>"
);

/// 一覧のカードを左右にスワイプして振り分ける（右でブックマーク、左で見ない）。
/// 振り分けたカードは隠し、しばらく「元に戻す」を出す。縦のスクロールはブラウザに任せ
/// （`touch-action: pan-y`）、画面の端から始まる操作はブラウザの「戻る」に譲る。
/// キーボードでは j/k・↓/↑ でカードを選び、l/→ と h/← で振り分け、u で取り消す。
const SWIPE_SCRIPT: &str = concat!("<script>\n", include_str!("assets/swipe.js"), "</script>");

/// 記事のカード。`swipe` なら一覧の振り分けの対象にする（`SWIPE_SCRIPT`）。
fn card(i: &ListItem, swipe: bool, page: &Page) -> String {
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
         <div class=\"meta\">{source} ・{at}{liked}{bookmarked}{lock}{translation}</div>{summary}</div>",
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
    )
}

/// 見出し。空だとリンクが押せなくなるので、和文の見出し、原題、URL の順に空でないものを使う。
pub(crate) fn display_title<'a>(title_ja: Option<&'a str>, i: &'a ListItem) -> &'a str {
    [title_ja.unwrap_or(""), &i.title]
        .into_iter()
        .find(|t| !t.trim().is_empty())
        .unwrap_or(&i.url)
}

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
            bookmarked: false,
            locked_by: vec![],
        }
    }

    fn usage(name: &str, facet: crate::topics::Facet, uses: i64) -> TopicUsage {
        TopicUsage {
            name: name.into(),
            facet,
            added_at: None,
            uses,
        }
    }

    #[test]
    fn search_page_keeps_the_conditions_in_the_form() {
        use crate::topics::Facet;
        let labels = SourceLabels::from([
            ("nra".to_string(), "原子力規制委員会".to_string()),
            ("wnn".to_string(), "WNN".to_string()),
        ]);
        let params = Params {
            q: "炉心 \"<b>\"".into(),
            since: "2026-09".into(),
            topics: vec!["PWR".into()],
            sources: vec!["nra".into()],
            lang: "ja".into(),
            unread: true,
            bookmarked: true,
            min_score: "60".into(),
            sort: "score".into(),
            ..Params::default()
        };
        let vocabulary = [
            usage("燃料", Facet::Field, 3),
            usage("規制・審査", Facet::Field, 9),
            usage("高経年化", Facet::Field, 0),
            usage("PWR", Facet::Reactor, 2),
        ];
        let html = search_page(
            &params,
            None,
            &vocabulary,
            None,
            &Page {
                labels: &labels,
                ..Page::default()
            },
        );
        assert!(
            html.contains(r#"<form method="get" action="/search">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="q" value="炉心 &quot;&lt;b&gt;&quot;""#),
            "{html}"
        );
        assert!(html.contains(r#"name="since" value="2026-09""#), "{html}");
        // 付いている数の多い順。要約に付いていない語は出さない
        let field = html.find("規制・審査").unwrap();
        assert!(field < html.find("燃料").unwrap(), "{html}");
        assert!(!html.contains("高経年化"), "{html}");
        assert!(
            html.contains(r#"name="topic" value="PWR" checked"#),
            "{html}"
        );
        assert!(html.contains(r#"name="topic" value="燃料">"#), "{html}");
        assert!(
            html.contains(r#"name="source" value="nra" checked"#),
            "{html}"
        );
        assert!(html.contains("原子力規制委員会"), "{html}");
        assert!(html.contains(r#"<option value="ja" selected>"#), "{html}");
        assert!(
            html.contains(r#"name="unread" value="1" checked"#),
            "{html}"
        );
        assert!(html.contains(r#"name="translated" value="1">"#), "{html}");
        assert!(
            html.contains(r#"name="bookmarked" value="1" checked"#),
            "{html}"
        );
        assert!(html.contains(r#"name="min_score" value="60""#), "{html}");
        assert!(
            html.contains(r#"<option value="score" selected>"#),
            "{html}"
        );
        assert!(
            !html.contains("件"),
            "no results section without results: {html}"
        );
    }

    /// 検索語の欄は画面の幅いっぱいに広げる。
    #[test]
    fn search_page_widens_the_query_field() {
        let html = search_page(&Params::default(), None, &[], None, &Page::default());
        let at = html.find(r#"<input name="q""#).expect(&html);
        let input = &html[at..at + html[at..].find('>').unwrap()];
        assert!(input.contains(r#"class="wide""#), "{input}");
        assert!(html.contains(".wide { width: 100%;"), "{html}");
    }

    /// 期間は文字でも 📅 のカレンダーでも入れられる。カレンダーは名前を持たず送られない。
    #[test]
    fn search_page_offers_a_calendar_for_the_period() {
        let params = Params {
            since: "2026-09".into(),
            ..Params::default()
        };
        let html = search_page(&params, None, &[], None, &Page::default());
        // 月だけの指定もできるよう、文字の欄は残す
        assert!(html.contains(r#"name="since" value="2026-09""#), "{html}");
        // キーボードでも操作できるよう、日付の入力はフォーカスでき、名前を持つ
        for (name, label) in [("since", "開始日"), ("until", "終了日")] {
            let cal = format!(
                r#"<label class="cal" title="カレンダー">📅<input type="date" data-for="{name}" aria-label="{label}をカレンダーで選ぶ"></label>"#
            );
            let text = html.find(&format!(r#"name="{name}""#)).expect(&html);
            let at = html.find(&cal).expect(&html);
            assert!(text < at, "the calendar follows the text field: {html}");
        }
        assert!(html.contains("showPicker"), "{html}");
    }

    /// 語彙に無い語（統合した語の別名など）で検索しても、フォームを送り直して条件が消えないようにする。
    #[test]
    fn search_page_keeps_topics_outside_the_vocabulary() {
        let params = Params {
            topics: vec!["新設炉".into()],
            ..Params::default()
        };
        let vocabulary = [usage("燃料", crate::topics::Facet::Field, 3)];
        let html = search_page(&params, Some(&[]), &vocabulary, None, &Page::default());
        assert!(
            html.contains(r#"name="topic" value="新設炉" checked"#),
            "{html}"
        );
    }

    #[test]
    fn search_page_shows_results_errors_and_empty_results() {
        let params = Params {
            q: "炉心".into(),
            ..Params::default()
        };
        let items = [
            item(1, "2026-09-27T00:00:00.000Z"),
            item(2, "2026-09-26T00:00:00.000Z"),
        ];
        let html = search_page(&params, Some(&items), &[], None, &Page::default());
        assert!(html.contains("2 件"), "{html}");
        assert!(
            html.contains("/articles/1") && html.contains("/articles/2"),
            "{html}"
        );
        let html = search_page(&params, Some(&[]), &[], None, &Page::default());
        assert!(html.contains("該当する記事はありません"), "{html}");
        let html = search_page(
            &params,
            None,
            &[],
            Some("since must be <YYYY-MM>"),
            &Page::default(),
        );
        assert!(html.contains("since must be &lt;YYYY-MM&gt;"), "{html}");
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

    #[test]
    fn settings_page_leads_to_the_glossary() {
        let html = settings_page(15, 3, &Page::default());
        assert!(html.contains(r#"href="/""#), "back to the list: {html}");
        assert!(html.contains(r#"<a href="/glossary">訳語集</a>"#), "{html}");
        assert!(html.contains("15 語"), "{html}");
        assert!(html.contains(r#"<a href="/reports">受付箱</a>"#), "{html}");
        assert!(html.contains("受付中 3 件"), "{html}");
    }

    fn term_report(id: i64, status: ReportStatus) -> Report {
        Report {
            id,
            article_id: 7,
            article_title: "見出し<A>".into(),
            kind: ReportKind::Term,
            found: Some("給油停止".into()),
            wanted: Some("燃料取替停止".into()),
            source: Some("refueling outage".into()),
            note: None,
            status,
            term: None,
            reply: None,
            reported_at: "2026-09-27T00:00:00.000Z".into(),
            resolved_at: None,
        }
    }

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

    fn other_report(id: i64, kind: ReportKind) -> Report {
        Report {
            kind,
            found: None,
            wanted: None,
            source: None,
            note: Some("数値が<違う>".into()),
            ..term_report(id, ReportStatus::Pending)
        }
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

    fn entry(
        id: i64,
        sources: &[&str],
        target: &str,
        abbr: Option<&str>,
    ) -> crate::glossary::Entry {
        crate::glossary::Entry {
            id,
            term: crate::glossary::Term {
                sources: sources.iter().map(|s| s.to_string()).collect(),
                target: target.into(),
                abbr: abbr.map(Into::into),
                note: Some("注<記>".into()),
            },
            term_changed_at: None,
            sources_added_at: vec![None; sources.len()],
        }
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
