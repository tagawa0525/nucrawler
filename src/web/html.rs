//! 画面の HTML。I/O を持たない関数だけにして、テストしやすくする。
//! JavaScript は一覧のスワイプ（`SWIPE_SCRIPT`）にだけ使い、無くても読める。

use crate::db::{ArticleDetail, ListItem, TopicUsage, Warning};
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
.bar { display: flex; gap: 0.5rem; margin: 0.3rem 0; }
.btn { font-size: 1.3rem; padding: 0.3rem 0.7rem; border-radius: 0.5rem; border: 2px solid #bbb;
  background: #fff; text-decoration: none; }
.btn.on { border-color: #2e7d32; background: #e3f1e4; }
.btn.off { border-color: #b3261e; background: #fbe7e6; }
.warn { background: #fff3cd; border-left: 4px solid #d39e00; padding: 0.5rem 0.75rem; margin: 0.4rem 0;
  font-size: 0.85rem; }
.actions form { display: inline; }
.actions button { font-size: 1.1rem; padding: 0.4rem 0.9rem; margin: 0.2rem; border-radius: 0.5rem;
  border: 1px solid #bbb; background: #fff; }
.actions button.on { background: #0b57a4; color: #fff; }
.versions a { margin-right: 0.6rem; font-size: 0.85rem; }
.card[data-id] { touch-action: pan-y; transition: transform 0.2s; }
.card[data-id]:focus { outline: 2px solid #0b57a4; outline-offset: 2px; }
.card[data-dir=bookmark] { box-shadow: inset 5px 0 #2e7d32; }
.card[data-dir=dismiss] { box-shadow: inset -5px 0 #b3261e; }
.toast { position: fixed; left: 50%; bottom: 1rem; transform: translateX(-50%); background: #1d1d1b;
  color: #fff; padding: 0.6rem 0.9rem; border-radius: 0.5rem; font-size: 0.9rem; }
.toast button { margin-left: 0.8rem; background: none; border: 0; color: #9cc3ff; font-size: 0.9rem; }
.translation p { line-height: 1.7; }
";

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
        "<nav class=\"bar\">{}{}{}{}</nav>",
        button("/search", "検索", "🔍", None),
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
         <p>期間 {since} 〜 {until}</p>\
         <details{topics_open}><summary>トピック</summary>{topics}</details>\
         <details{sources_open}><summary>ソース</summary>{sources}</details>\
         <p>言語 {lang} 並び {sort}</p>\
         <p>{translated}{liked}{unread}{bookmarked}最低点 {min_score}</p>\
         <p><button type=\"submit\">検索</button></p></form>",
        q = text(
            "q",
            &p.q,
            " type=\"search\" placeholder=\"語（空白で区切るとすべてを含む）\""
        ),
        since = text("since", &p.since, " size=\"10\" placeholder=\"2026-09\""),
        until = text("until", &p.until, " size=\"10\" placeholder=\"2026-09-30\""),
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

/// 一覧のカードを左右にスワイプして振り分ける（右でブックマーク、左で見ない）。
/// 振り分けたカードは隠し、しばらく「元に戻す」を出す。縦のスクロールはブラウザに任せ
/// （`touch-action: pan-y`）、画面の端から始まる操作はブラウザの「戻る」に譲る。
/// キーボードでは j/k・↓/↑ でカードを選び、l/→ と h/← で振り分け、u で取り消す。
const SWIPE_SCRIPT: &str = r#"<script>
(() => {
  const post = (url, kind) => fetch(url, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: "kind=" + kind,
    redirect: "manual",
  }).then((res) => res.ok || res.type === "opaqueredirect");
  const toast = document.createElement("div");
  toast.className = "toast";
  // 振り分けの結果をスクリーンリーダーにも伝える
  toast.setAttribute("role", "status");
  toast.hidden = true;
  document.body.append(toast);
  let timer, undoLast = null;
  const hideToast = () => { toast.hidden = true; undoLast = null; };
  const notify = (text, undo) => {
    clearTimeout(timer);
    toast.textContent = text;
    undoLast = undo || null;
    if (undo) {
      const button = document.createElement("button");
      button.textContent = "元に戻す";
      button.onclick = () => { hideToast(); undo(); };
      toast.append(button);
    }
    toast.hidden = false;
    timer = setTimeout(hideToast, 6000);
  };
  const reset = (card) => {
    card.style.transform = "";
    delete card.dataset.dir;
    delete card.dataset.busy;
  };
  // 送っている間は同じカードを振り分け直さない（行動が二重に記録される）
  const triage = async (card, kind) => {
    if (card.dataset.busy) return;
    card.dataset.busy = "1";
    const id = card.dataset.id;
    card.style.transform = `translateX(${kind === "bookmark" ? "" : "-"}110%)`;
    if (!(await post(`/articles/${id}/feedback`, kind).catch(() => false))) {
      reset(card);
      notify("記録できませんでした");
      return;
    }
    card.hidden = true;
    notify(kind === "bookmark" ? "🔖 ブックマークしました" : "見ない記事にしました", async () => {
      if (await post(`/articles/${id}/feedback/undo`, kind).catch(() => false)) {
        reset(card);
        card.hidden = false;
      } else {
        notify("取り消せませんでした");
      }
    });
  };
  const TRIAGE_KEYS = { l: "bookmark", ArrowRight: "bookmark", h: "dismiss", ArrowLeft: "dismiss" };
  const MOVE_KEYS = { j: 1, ArrowDown: 1, k: -1, ArrowUp: -1 };
  document.addEventListener("keydown", (e) => {
    if (e.altKey || e.ctrlKey || e.metaKey || e.target.closest("input, textarea, select")) return;
    const cards = [...document.querySelectorAll(".card[data-id]")]
      .filter((c) => !c.hidden && !c.dataset.busy);
    const current = e.target.closest(".card[data-id]");
    const at = cards.indexOf(current);
    if (e.key in MOVE_KEYS) {
      const next = cards[at < 0 ? 0 : Math.min(Math.max(at + MOVE_KEYS[e.key], 0), cards.length - 1)];
      if (next) { e.preventDefault(); next.focus(); }
    } else if (e.key in TRIAGE_KEYS && at >= 0) {
      e.preventDefault();
      // 振り分けたカードは隠れるので、隣のカードを選んでおく
      const next = cards[at + 1] || cards[at - 1];
      triage(current, TRIAGE_KEYS[e.key]);
      if (next) next.focus();
    } else if (e.key === "Enter" && e.target === current) {
      current.querySelector("a.title").click();
    } else if (e.key === "u" && undoLast) {
      e.preventDefault();
      const undo = undoLast;
      hideToast();
      undo();
    }
  });
  const EDGE = 24, START = 10, COMMIT = 0.35;
  for (const card of document.querySelectorAll(".card[data-id]")) {
    let x0 = null, y0 = 0, dx = 0, dragging = false, moved = false;
    card.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 || card.dataset.busy) return;
      if (e.clientX < EDGE || e.clientX > innerWidth - EDGE) return;
      x0 = e.clientX; y0 = e.clientY; dx = 0; dragging = false; moved = false;
    });
    card.addEventListener("pointermove", (e) => {
      if (x0 === null) return;
      dx = e.clientX - x0;
      if (!dragging) {
        if (Math.abs(e.clientY - y0) > Math.abs(dx)) { x0 = null; return; }
        if (Math.abs(dx) < START) return;
        dragging = moved = true;
        card.setPointerCapture(e.pointerId);
        card.style.transition = "none";
      }
      card.style.transform = `translateX(${dx}px)`;
      card.dataset.dir = dx > 0 ? "bookmark" : "dismiss";
    });
    const end = () => {
      const was = dragging;
      x0 = null; dragging = false;
      if (!was) return;
      card.style.transition = "";
      if (Math.abs(dx) > card.offsetWidth * COMMIT) {
        triage(card, dx > 0 ? "bookmark" : "dismiss");
      } else {
        reset(card);
      }
    };
    card.addEventListener("pointerup", end);
    // ドラッグ中の取り消しだけを戻す（送信中のカードの状態は消さない）
    card.addEventListener("pointercancel", () => {
      const was = dragging;
      x0 = null; dragging = false;
      if (!was) return;
      card.style.transition = "";
      reset(card);
    });
    // スワイプの指を離したときのクリックで、記事を開かない
    card.addEventListener("click", (e) => {
      if (moved) { e.preventDefault(); moved = false; }
    }, true);
  }
})();
</script>"#;

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
         <div class=\"meta\">{source} ・{at}{lock}{translation}</div>{summary}</div>",
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

/// 詳細画面の表示の選択。
#[derive(Debug, Clone, Copy, Default)]
pub struct DetailView {
    /// 表示する digest の版（無ければ最新）
    pub digest: Option<i64>,
    /// 全文和訳を表示する（`translation` があればその版、無ければ最新）
    pub show_translation: bool,
    pub translation: Option<i64>,
}

pub fn detail_page(d: &ArticleDetail, view: DetailView, page: &Page) -> String {
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
    layout(&title, page, &body)
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
            !html.contains(r#"data-id=""#) && !html.contains("<script>"),
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
                .filter(|h| !h.starts_with("/search"))
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
        let html = detail_page(&d, DetailView::default(), &Page::default());
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
