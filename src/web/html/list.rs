//! 記事の一覧とカード。

use super::*;

/// 一覧を「前回の訪問の後に届いた記事」と「それより前の記事」に分ける。既読で絞るのは一覧の問い合わせで
/// 済ませておく（`ListQuery::read`）。`boundary`（`Db::begin_visit` の区切り）が無ければ（初回）、
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

/// 既読で絞る（`Some(true)` は既読だけ、`Some(false)` は未読だけ、`None` は絞らない）。確認枠はその日に
/// 選んだ記事を出し直すので、選んだ後に既読にした記事などをここで絞る。
pub fn filter_read(items: Vec<ListItem>, read: Option<bool>) -> Vec<ListItem> {
    match read {
        None => items,
        // 同じ報道のグループのどれかを読んでいれば既読（`is_read`。一覧と同じ）
        Some(read) => items.into_iter().filter(|i| i.is_read() == read).collect(),
    }
}

/// 印で絞る切り替え（👁・🔖）の次の状態。絞らない → 印のある記事だけ → 印の無い記事だけ → 絞らない。
fn next_mark(mark: Option<bool>) -> Option<bool> {
    match mark {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    }
}

/// 印で絞る切り替えの値（URL と欄の条件）。1 はあり、0 はなし。
fn mark_value(on: bool) -> &'static str {
    if on { "1" } else { "0" }
}

/// 一覧の表示の選択。最低点は `min=N`（既定の最低点なら省く）、既読は `read=1`（既読だけ）・`read=0`（未読だけ）・
/// `read=any`（絞らない）、評価は `rating=hide-low`（★1〜2 を隠す）・`rating=N`（★N 以上）・`rating=0`（評価の無い
/// 記事だけ）・`rating=any`（絞らない）、ブックマークは `bookmarked=1`・`bookmarked=0` で持つ（この表示の既定なら省く）。
/// 評価した記事（`rating=N`・`rating=0`）・ブックマーク中だけ（`bookmarked=1`）で絞るときは、一覧の代わりに全期間から
/// 該当する記事を出す（絞り込み）。一覧の既定は未読だけ・★1〜2 を隠す、絞り込みは集めた記事を見返すので既読・評価で
/// 絞らない。
#[derive(Clone, Copy)]
pub struct ListView {
    /// 表示する最低点。0 なら評価 1〜2・未採点・軽水炉と無関係の記事も出す（すべて）。`None` なら推薦点で絞らない
    /// （最低点なし。評価 1〜2・軽水炉と無関係の記事は隠す）
    pub min: Option<u8>,
    /// 一覧の既定の最低点。プロファイルがあれば設定の `web.min_score`、無ければ採点が無いので最低点なし（`None`）
    pub default_min: Option<u8>,
    /// 既読で絞る（`Some(true)` は既読だけ、`Some(false)` は未読だけ、`None` は絞らない）
    pub read: Option<bool>,
    /// 評価で絞る
    pub rating: RatingFilter,
    /// ブックマークで絞る（`Some(true)` はブックマーク中だけで絞り込みの画面、`Some(false)` はしていない記事だけ）
    pub bookmarked: Option<bool>,
}

impl Default for ListView {
    fn default() -> Self {
        let min = Some(crate::config::WebConfig::default().min_score);
        Self {
            min,
            default_min: min,
            read: Some(false),
            rating: RatingFilter::HideLow,
            bookmarked: None,
        }
    }
}

/// 最低点の選択肢の刻み（0〜90）
const MIN_STEP: u8 = 10;

impl ListView {
    /// 「すべて」の表示か（評価 1〜2・未採点・軽水炉と無関係の記事も出す）。
    pub fn shows_all(self) -> bool {
        self.min == Some(0)
    }

    /// 評価した記事・ブックマーク中だけで絞っているか（一覧の代わりに全期間から探す）。
    pub fn filtered(self) -> bool {
        matches!(
            self.rating,
            RatingFilter::AtLeast(_) | RatingFilter::Unrated
        ) || self.bookmarked == Some(true)
    }

    /// この表示での既読の既定。一覧は未読だけ、絞り込みは絞らない。
    pub fn read_default(self) -> Option<bool> {
        if self.filtered() { None } else { Some(false) }
    }

    /// この表示での評価の既定。一覧は ★1〜2 を隠す、絞り込みは絞らない。
    pub fn rating_default(self) -> RatingFilter {
        if self.filtered() {
            RatingFilter::Any
        } else {
            RatingFilter::HideLow
        }
    }

    /// この表示での最低点の既定。一覧は利用者の既定（`default_min`）、絞り込みは 0（点数で絞らない）。
    pub fn min_default(self) -> Option<u8> {
        if self.filtered() {
            Some(0)
        } else {
            self.default_min
        }
    }

    /// 絞り込みを変えた表示。一覧と絞り込みを行き来するときは、既読の表示と最低点を行き先の既定に戻す。
    /// 評価は、選び直したのでなく元の表示の既定のままなら、行き先の既定にする。
    fn with_filters(self, rating: RatingFilter, bookmarked: Option<bool>) -> Self {
        let mut next = Self {
            rating,
            bookmarked,
            ..self
        };
        if next.filtered() != self.filtered() {
            next.read = next.read_default();
            next.min = next.min_default();
            if rating == self.rating && rating == self.rating_default() {
                next.rating = next.rating_default();
            }
        }
        next
    }

    /// この表示の一覧の正規の URL。既定と同じ値は付けない（最低点・既読の表示は、この表示での既定と
    /// 違うときだけ）。
    pub fn url(self) -> String {
        // 最低点なしは、既定が最低点なしの表示でしか選べないので、いつも省ける
        let min = self.min.map(|m| format!("min={m}")).unwrap_or_default();
        let rating = format!("rating={}", rating_value(self.rating));
        let read = format!("read={}", self.read.map_or("any", mark_value));
        let bookmarked = format!("bookmarked={}", self.bookmarked.map_or("", mark_value));
        let query: Vec<&str> = [
            (
                self.min.is_some() && self.min != self.min_default(),
                min.as_str(),
            ),
            (self.rating != self.rating_default(), rating.as_str()),
            (self.read != self.read_default(), read.as_str()),
            (self.bookmarked.is_some(), bookmarked.as_str()),
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

/// 評価の条件の、URL と選択肢の値。
fn rating_value(rating: RatingFilter) -> String {
    match rating {
        RatingFilter::Any => "any".to_string(),
        RatingFilter::HideLow => "hide-low".to_string(),
        RatingFilter::AtLeast(r) => r.get().to_string(),
        RatingFilter::Unrated => "0".to_string(),
    }
}

pub fn list_page(new: &[ListItem], earlier: &[ListItem], view: ListView, page: &Page) -> String {
    list_page_with_explore(new, earlier, &[], view, HiddenCounts::default(), page)
}

/// 一覧の条件をそれぞれ 1 つだけ外したときに加わる記事の数（その条件で隠れている記事の数）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HiddenCounts {
    /// 最低点（「すべて」にすると加わる、点数が足りない・未採点の記事）
    pub min: usize,
    /// 👁（既読・未読で絞らないと加わる記事）
    pub read: usize,
    /// ★（★1〜2 を隠さないと加わる記事）
    pub rating: usize,
    /// 🔖（ブックマークなしだけで絞らないと加わる記事）
    pub bookmarked: usize,
}

/// 一覧で効いている条件（例：点数 50 以上・未読だけ・★1〜2 を隠す）。
fn list_conditions(view: ListView) -> Vec<String> {
    let min = view
        .min
        .filter(|m| *m > 0)
        .map(|m| format!("点数 {m} 以上"));
    let read = view
        .read
        .map(|read| if read { "既読だけ" } else { "未読だけ" }.to_string());
    let rating = match view.rating {
        RatingFilter::Any => None,
        RatingFilter::HideLow => Some("★1〜2 を隠す".to_string()),
        RatingFilter::AtLeast(r) => Some(format!("★{} 以上", r.get())),
        RatingFilter::Unrated => Some("未評価だけ".to_string()),
    };
    let bookmarked = view.bookmarked.map(|on| {
        if on {
            "ブックマーク中だけ"
        } else {
            "ブックマークなしだけ"
        }
        .to_string()
    });
    [min, read, rating, bookmarked]
        .into_iter()
        .flatten()
        .collect()
}

/// 条件で隠れている記事の数を、条件ごとに、その条件を外した表示へのリンクにして出す。何も隠れていなければ空。
fn hidden_note(view: ListView, hidden: HiddenCounts) -> String {
    let min = view
        .min
        .filter(|m| *m > 0)
        // 最低点を外すと、未採点（軽水炉と無関係で採点しない記事を含む）も加わる
        .map(|m| {
            (
                hidden.min,
                format!("点数 {m} 未満・未採点"),
                view.with_min(0),
            )
        });
    let read = view.read.map(|read| {
        let label = if read { "未読" } else { "既読" };
        (hidden.read, label.to_string(), view.with_read(None))
    });
    let rating = (view.rating == RatingFilter::HideLow).then(|| {
        (
            hidden.rating,
            "★1〜2".to_string(),
            view.with_rating(RatingFilter::Any),
        )
    });
    let bookmarked = (view.bookmarked == Some(false)).then(|| {
        (
            hidden.bookmarked,
            "ブックマーク中".to_string(),
            view.with_bookmarked(None),
        )
    });
    let links: Vec<String> = [min, read, rating, bookmarked]
        .into_iter()
        .flatten()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, label, target)| {
            format!("<a href=\"{}\">{label} {count} 件</a>", target.bar_href())
        })
        .collect();
    if links.is_empty() {
        return String::new();
    }
    format!(
        "<p class=\"meta\">条件で隠れている記事：{}</p>",
        links.join("・")
    )
}

/// 選択を選んだときに移る先。選択肢ごとに、その表示の正規の URL を `data-href` に持たせる
/// （一覧と絞り込みを行き来するときに、既読の表示と最低点を行き先の既定に戻すため）。
const JUMP: &str = "location.href=this.selectedOptions[0].dataset.href";

/// 上部のバー（点数・評価・既読・ブックマーク）が指す表示。一覧・絞り込み（`ListView`）と
/// 検索（`SearchView`）で、今の状態と、バーの部品を変えたときの行き先を与える。
pub(super) trait BarView: Clone {
    /// JavaScript が無いときに選択を送る先
    fn action(&self) -> &'static str;
    /// この表示の正規の URL
    fn bar_url(&self) -> String;
    /// 表示する最低点（0 は絞らない。`None` は最低点なし）
    fn min(&self) -> Option<u8>;
    /// 最低点の選択肢に加える値（設定の最低点）
    fn extra_min(&self) -> Option<u8>;
    /// 最低点なしを選べるなら、その表示（一覧の既定が最低点なしのとき）
    fn without_min(&self) -> Option<Self> {
        None
    }
    /// 評価の条件
    fn rating(&self) -> RatingFilter;
    /// 既読で絞る（`Some(true)` は既読だけ、`Some(false)` は未読だけ）
    fn read(&self) -> Option<bool>;
    /// ブックマークで絞る（`Some(true)` はブックマーク中だけ、`Some(false)` はしていない記事だけ）
    fn bookmarked(&self) -> Option<bool>;
    fn with_min(&self, min: u8) -> Self;
    fn with_rating(&self, rating: RatingFilter) -> Self;
    fn with_read(&self, read: Option<bool>) -> Self;
    fn with_bookmarked(&self, bookmarked: Option<bool>) -> Self;
    /// JavaScript が無いときに評価の選択と一緒に送る、今の条件（評価の選択で置き換わる欄は送らない）
    fn rating_inputs(&self) -> String {
        state_inputs(self, self.rating_replaces())
    }
    /// JavaScript が無いときに最低点の選択を送る欄の名前（選んだ値で置き換わるので、今の条件からは外す）
    fn min_name(&self) -> &'static str;
    /// 評価の選択（`rating`）で置き換わる、今の条件の欄の名前
    fn rating_replaces(&self) -> &'static [&'static str];
    /// この表示の URL（HTML の属性値としてエスケープ済み）
    fn bar_href(&self) -> String {
        escape(&self.bar_url())
    }
}

impl BarView for ListView {
    fn action(&self) -> &'static str {
        "/"
    }
    fn bar_url(&self) -> String {
        ListView::url(*self)
    }
    fn min(&self) -> Option<u8> {
        self.min
    }
    fn extra_min(&self) -> Option<u8> {
        self.default_min
    }
    fn without_min(&self) -> Option<Self> {
        self.min_default()
            .is_none()
            .then_some(ListView { min: None, ..*self })
    }
    fn rating(&self) -> RatingFilter {
        self.rating
    }
    fn read(&self) -> Option<bool> {
        self.read
    }
    fn bookmarked(&self) -> Option<bool> {
        self.bookmarked
    }
    fn with_min(&self, min: u8) -> Self {
        ListView {
            min: Some(min),
            ..*self
        }
    }
    fn with_rating(&self, rating: RatingFilter) -> Self {
        self.with_filters(rating, self.bookmarked)
    }
    fn with_read(&self, read: Option<bool>) -> Self {
        ListView { read, ..*self }
    }
    fn with_bookmarked(&self, bookmarked: Option<bool>) -> Self {
        self.with_filters(self.rating, bookmarked)
    }
    /// 行き先が一覧か絞り込みかは選んだ評価で決まるので、今の条件と、どちらの画面から送ったか（`from`）を送る。
    /// 一覧と絞り込みを行き来したときに最低点と既読を行き先の既定にするのは、受け取った側で行う
    /// （JavaScript のときの行き先 `with_filters` と同じ）
    fn rating_inputs(&self) -> String {
        let from = if self.filtered() { "filtered" } else { "list" };
        format!(
            "{}<input type=\"hidden\" name=\"from\" value=\"{from}\">",
            state_inputs(self, self.rating_replaces())
        )
    }
    fn min_name(&self) -> &'static str {
        "min"
    }
    fn rating_replaces(&self) -> &'static [&'static str] {
        &["rating"]
    }
}

/// JavaScript が無いときに選択と一緒に送る、今の表示のほかの条件（正規の URL の、`except` 以外の欄）。
fn state_inputs(view: &impl BarView, except: &[&str]) -> String {
    let url = view.bar_url();
    let query = url.split_once('?').map_or("", |(_, query)| query);
    url::form_urlencoded::parse(query.as_bytes())
        .filter(|(key, _)| !except.contains(&key.as_ref()))
        .map(|(key, value)| {
            format!(
                "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
                escape(&key),
                escape(&value)
            )
        })
        .collect()
}

/// 選択肢。今の選択なら `selected`。
fn option(value: &str, target: &impl BarView, selected: bool, label: &str) -> String {
    format!(
        "<option value=\"{value}\" data-href=\"{}\"{}>{label}</option>",
        target.bar_href(),
        if selected { " selected" } else { "" },
    )
}

/// 開いた一覧と閉じた選択で書き分ける選択肢。開いた一覧では `label`、閉じた選択では `closed` と書く（`BAR_SCRIPT`）。
fn relabeled_option(
    value: &str,
    target: &impl BarView,
    selected: bool,
    label: &str,
    closed: &str,
) -> String {
    format!(
        "<option value=\"{value}\" data-href=\"{}\" data-closed=\"{closed}\"{}>{label}</option>",
        target.bar_href(),
        if selected { " selected" } else { "" },
    )
}

/// 「絞らない」の選択肢。開いた一覧では「-」、閉じた選択では `closed`（00・★）と書く。
fn blank_option(value: &str, target: &impl BarView, selected: bool, closed: &str) -> String {
    relabeled_option(value, target, selected, "-", closed)
}

/// 上部のバーの選択肢を、閉じているときは短く（「絞らない」は 00・★、★1〜2 を隠すは ★3+☆）、開いた一覧では
/// 意味が分かるように（「-」・「★1〜2 を隠す」）書き分ける。JavaScript が無ければ開いた一覧の書き方のまま。
pub(super) const BAR_SCRIPT: &str =
    concat!("<script>\n", include_str!("assets/bar.js"), "</script>");

/// 表示する最低点の選択。0〜90 の 10 刻みと設定・今の最低点から選び、選ぶとすぐ表示を切り替える
/// （JavaScript が無ければ「表示」のボタンで）。0 は点数で絞らない（すべて。開いた一覧では「-」、閉じた選択では 00）で、
/// それ以外のあいだは緑にする。一覧の既定が最低点なし（プロファイルが無い）なら、先頭に最低点なし（「--」）を置く。
fn min_select(view: &impl BarView) -> String {
    let name = view.min_name();
    let mut values: Vec<u8> = (0..100).step_by(MIN_STEP.into()).collect();
    values.extend(view.extra_min());
    values.extend(view.min());
    values.sort_unstable();
    values.dedup();
    // 空の値は、JavaScript が無いときに送っても既定（最低点なし）になる
    let unfloored = view
        .without_min()
        .map(|target| option("", &target, view.min().is_none(), "--"));
    let options: String = unfloored
        .into_iter()
        .chain(values.into_iter().map(|v| {
            let target = view.with_min(v);
            let selected = view.min() == Some(v);
            if v == 0 {
                // 絞らない
                blank_option("0", &target, selected, "00")
            } else {
                // 桁をそろえる（1 桁は 0 を付ける）
                option(&v.to_string(), &target, selected, &format!("{v:02}"))
            }
        }))
        .collect();
    format!(
        "<form class=\"min{}\" method=\"get\" action=\"{}\"><select name=\"{name}\" aria-label=\"表示する最低点\" \
         title=\"表示する最低点\" onchange=\"{JUMP}\">{options}</select>{}\
         <noscript><button>表示</button></noscript></form>",
        if view.min().is_some_and(|m| m > 0) {
            " on"
        } else {
            ""
        },
        view.action(),
        state_inputs(view, &[name]),
    )
}

/// 評価で絞る選択。最低点の数字と見分けられるよう ★ で示す。「-」（閉じた選択では数字の無い ★）は絞らない、
/// 「★1〜2 を隠す」（閉じた選択では ★3+☆）は関心が無いと評価した記事を隠して未評価は残す（一覧の既定）、
/// ★1〜★5 は評価した記事のうちその評価以上で、最低点と同じく小さい順で「以上」の印は付けない（★4 は ★4 以上）、
/// 最後の白抜きの「☆」は評価の無い記事だけ。絞っているあいだは緑にする。
/// 選ぶとすぐ表示を切り替える（JavaScript が無ければ「表示」のボタンで、`rating_inputs` の条件も送る）。
fn rating_select(view: &impl BarView) -> String {
    let rated = (Rating::MIN..=Rating::MAX)
        .filter_map(Rating::new)
        .map(|r| (RatingFilter::AtLeast(r), format!("★{}", r.get())))
        .chain([(RatingFilter::Unrated, "☆".to_string())]);
    let choice = |rating: RatingFilter| (view.with_rating(rating), view.rating() == rating);
    let (target, selected) = choice(RatingFilter::Any);
    let blank = blank_option("any", &target, selected, "★");
    let (target, selected) = choice(RatingFilter::HideLow);
    let hide_low = relabeled_option("hide-low", &target, selected, "★1〜2 を隠す", "★3+☆");
    let options: String = [blank, hide_low]
        .into_iter()
        .chain(rated.map(|(rating, label)| {
            let (target, selected) = choice(rating);
            option(&rating_value(rating), &target, selected, &label)
        }))
        .collect();
    let inputs = view.rating_inputs();
    format!(
        "<form class=\"stars{}\" method=\"get\" action=\"{}\"><select name=\"rating\" aria-label=\"評価で絞る\" \
         title=\"評価で絞る\" onchange=\"{JUMP}\">{options}</select>{inputs}\
         <noscript><button>表示</button></noscript></form>",
        if view.rating() == RatingFilter::Any {
            ""
        } else {
            " on"
        },
        view.action(),
    )
}

/// 上部のバー。検索・点数・評価・既読・ブックマーク・設定の順で、カードの下の印と同じ並びにする。
/// 一覧・絞り込みの画面では先頭を 🔍（検索）に、ほかの画面では 🏠（一覧へ戻る）にする。
pub(super) fn bar(view: &impl BarView, home: bool) -> String {
    let read = mark_button(
        &view.with_read(next_mark(view.read())).bar_href(),
        "既読",
        "👁",
        view.read(),
        ["絞らない", "既読だけ", "未読だけ"],
    );
    let bookmark = mark_button(
        &view
            .with_bookmarked(next_mark(view.bookmarked()))
            .bar_href(),
        "ブックマーク",
        "🔖",
        view.bookmarked(),
        ["絞らない", "ブックマーク中だけ", "ブックマークなしだけ"],
    );
    format!(
        "<nav class=\"bar\">{}{}{}{read}{bookmark}{}</nav>{BAR_SCRIPT}",
        if home {
            button("/", "ホーム", "🏠", "")
        } else {
            button("/search", "検索", "🔍", "")
        },
        min_select(view),
        rating_select(view),
        button("/settings", "設定", "⚙️", ""),
    )
}

/// 印で絞る切り替え（👁・🔖）。絞らない（白）・印のある記事だけ（緑）・印の無い記事だけ（赤と斜線）を示し、
/// 押すと次の状態へ移る（`href`）。名前には今の状態と、押したときの次の状態を出す。
/// `states` は 絞らない・印のある記事だけ・印の無い記事だけ の言い方。
fn mark_button(
    href: &str,
    name: &str,
    emoji: &str,
    mark: Option<bool>,
    states: [&str; 3],
) -> String {
    let state = |mark: Option<bool>| match mark {
        None => states[0],
        Some(true) => states[1],
        Some(false) => states[2],
    };
    let class = match mark {
        None => "",
        Some(true) => "on",
        Some(false) => "not",
    };
    let label = format!(
        "{name}：{}（押すと{}）",
        state(mark),
        state(next_mark(mark))
    );
    button(href, &label, emoji, class)
}

/// 欄（`.sections`）に持たせる、印で絞る条件（`data-read`・`data-bookmarked`。1 はあり、0 はなし）と評価の条件
/// （`data-hide-low`・`data-min-rating`・`data-unrated`）。印を付け外しして条件から外れたカードは、その場で隠す
/// （`MARKS_SCRIPT`）。
fn mark_conditions(view: ListView) -> String {
    let marks: String = [("read", view.read), ("bookmarked", view.bookmarked)]
        .into_iter()
        .filter_map(|(name, mark)| mark.map(|on| format!(" data-{name}=\"{}\"", mark_value(on))))
        .collect();
    let rating = match view.rating {
        RatingFilter::Any => String::new(),
        RatingFilter::HideLow => " data-hide-low=\"1\"".to_string(),
        RatingFilter::AtLeast(r) => format!(" data-min-rating=\"{}\"", r.get()),
        RatingFilter::Unrated => " data-unrated=\"1\"".to_string(),
    };
    marks + &rating
}

/// 絞り込みの見出し。全期間から探していることと、何で絞ったかを出す（例：ブックマーク中・★4 以上（全期間））。
fn filtered_heading(view: ListView) -> String {
    let rating = match view.rating {
        RatingFilter::Any => None,
        RatingFilter::HideLow => Some("★1〜2 を隠す".to_string()),
        RatingFilter::AtLeast(r) => Some(format!("★{} 以上", r.get())),
        RatingFilter::Unrated => Some("未評価".to_string()),
    };
    let parts: Vec<String> = (view.bookmarked == Some(true))
        .then(|| "ブックマーク中".to_string())
        .into_iter()
        .chain(rating)
        .collect();
    format!("{}（全期間）", parts.join("・"))
}

/// 一覧のほかの画面（検索・詳細）の上部のバー。一覧の既定の表示を指し、先頭は 🏠。
pub(super) fn home_bar(page: &Page) -> String {
    let view = ListView {
        min: page.default_min,
        default_min: page.default_min,
        ..ListView::default()
    };
    bar(&view, true)
}

/// 評価・ブックマークで絞った記事。上部のバーは一覧と同じで、検索のフォームは出さない。
pub fn filtered_page(items: &[ListItem], view: ListView, page: &Page) -> String {
    let mut body = bar(&view, false);
    // 0 件でも、何で絞ったかと全期間であることは出す
    body.push_str(&format!(
        "<h2>{}<span class=\"count\">{} 件</span></h2>",
        filtered_heading(view),
        items.len(),
    ));
    if items.is_empty() {
        body.push_str("<p class=\"meta\">該当する記事はありません</p>");
    } else {
        // 欄に絞り込みの条件を持たせ、条件から外れたカードをその場で隠す（`MARKS_SCRIPT`）
        body.push_str(&format!(
            "<div class=\"sections\"{}>",
            mark_conditions(view),
        ));
        body.extend(items.iter().map(|i| card(i, true, page)));
        body.push_str("</div>");
    }
    body.push_str(MARKS_SCRIPT);
    layout("一覧", page, &body)
}

/// 一覧に、閾値未満から無作為に選んだ確認枠（`explore`）と、条件で隠れている記事の数（`hidden`）を添える。
pub fn list_page_with_explore(
    new: &[ListItem],
    earlier: &[ListItem],
    explore: &[ListItem],
    view: ListView,
    hidden: HiddenCounts,
    page: &Page,
) -> String {
    let mut body = bar(&view, false);
    // 欄に印で絞る条件を持たせ、条件から外れたカードをその場で隠す（`MARKS_SCRIPT`）
    body.push_str(&format!(
        "<div class=\"sections\"{}>",
        mark_conditions(view)
    ));
    body.push_str("<h2>前回から</h2>");
    if new.is_empty() && earlier.is_empty() {
        // 1 件も出ないときは、何が記事を隠しているかが分かるように、効いている条件を出す
        let conditions = list_conditions(view);
        if conditions.is_empty() {
            body.push_str("<p class=\"meta\">記事はありません</p>");
        } else {
            body.push_str(&format!(
                "<p class=\"meta\">{} に合う記事はありません</p>",
                conditions.join("・")
            ));
        }
    } else if new.is_empty() {
        body.push_str("<p class=\"meta\">新しい記事はありません</p>");
    }
    body.extend(new.iter().map(|i| card(i, true, page)));
    if !earlier.is_empty() {
        body.push_str(match view.read {
            None => "<h2>過去の記事</h2>",
            Some(true) => "<h2>過去の既読</h2>",
            Some(false) => "<h2>過去の未読</h2>",
        });
        body.extend(earlier.iter().map(|i| card(i, true, page)));
    }
    body.push_str(&hidden_note(view, hidden));
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
    match i.llm_score.filter(|llm| *llm != score) {
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
    use crate::db::{Rating, RatingFilter};
    use crate::web::html::test_support::*;

    fn at_least(rating: u8) -> RatingFilter {
        RatingFilter::AtLeast(Rating::new(rating).unwrap())
    }

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
        assert_eq!(ids(&filter_read(items.clone(), Some(false))), [1, 2]);
        assert_eq!(ids(&filter_read(items.clone(), None)), [1, 2, 3, 4, 5]);
        // 初回（区切りが無い）は、すべて前回からの欄
        let (new, earlier) = split_sections(items, None);
        assert_eq!(ids(&new), [1, 2, 3, 4, 5]);
        assert!(earlier.is_empty());
    }

    /// カードの 👁 は、同じ報道のグループのどれかを読んでいれば付いた状態で出す（一覧の既読の絞り込みと同じ）。
    #[test]
    fn card_shows_the_story_as_read() {
        let mut story_read = item(1, "2026-09-27T05:00:00.000Z");
        story_read.story_read = true;
        let html = list_page(
            &[],
            &[story_read],
            ListView {
                read: None,
                ..ListView::default()
            },
            &Page::default(),
        );
        assert!(
            html.contains(r#"<div class="card read" data-id="1" tabindex="0">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<form method="post" action="/articles/1/read"><button name="on" value="0" aria-pressed="true" aria-label="既読" title="既読" class="on">👁</button></form>"#),
            "{html}"
        );
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
            at(r#"aria-label="既読："#),
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
            html.contains(r#"<div class="sections" data-read="0" data-hide-low="1">"#),
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
            html.contains("want(f.read")
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
            read: None,
            ..ListView::default()
        };
        let html = list_page(
            &[item(1, "2026-09-27T05:00:00.000Z")],
            &[],
            shown,
            &Page::default(),
        );
        assert!(
            html.contains(r#"<div class="sections" data-hide-low="1">"#),
            "{html}"
        );
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
            min: Some(0),
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
                r#"<a class="btn not" href="/?min=0&amp;read=any" aria-label="既読：未読だけ（押すと絞らない）" title="既読：未読だけ（押すと絞らない）">👁</a>"#
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
            let at = html.find("👁</a>").unwrap();
            let start = html[..at].rfind("href=\"").unwrap() + 6;
            html[start..start + html[start..].find('"').unwrap()].to_string()
        };
        // 未読だけ（一覧の既定）→ 絞らない → 既読だけ
        assert_eq!(eye(Some(50), Some(false)), "/?read=any");
        assert_eq!(eye(Some(50), None), "/?read=1");
        assert_eq!(eye(Some(30), Some(false)), "/?min=30&amp;read=any");
        assert_eq!(eye(Some(30), None), "/?min=30&amp;read=1");
        let read = ListView {
            read: None,
            ..ListView::default()
        };
        let html = list_page(&[], &[], read, &Page::default());
        assert!(
            html.contains(r#"<input type="hidden" name="read" value="any">"#),
            "{html}"
        );
        let odd = ListView {
            min: Some(55),
            default_min: Some(55),
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

    /// 評価の選択は、★1〜2 を隠す（一覧の既定）・評価（★1〜5）以上・評価の無い記事だけから選び、🔖 はブックマークだけに、
    /// 一覧の上部のバーで絞る（検索画面へは移らない）。評価の選択は、最低点の数字と見分けられるよう ★ で示す。
    /// 並びは最低点と同じく小さい順で、最低点と同じく「以上」の印（↑）は付けない。絞っているときは切り替えの ON と
    /// 同じ緑にする。★1〜2 を隠す選択肢は、開いた一覧では意味を書き、閉じた選択では短く ★3+☆ と書く。
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
                r#"<form class="stars on" method="get" action="/"><select name="rating" aria-label="評価で絞る" title="評価で絞る" onchange="location.href=this.selectedOptions[0].dataset.href"><option value="any" data-href="/?rating=any" data-closed="★">-</option><option value="hide-low" data-href="/" data-closed="★3+☆" selected>★1〜2 を隠す</option><option value="1" data-href="/?rating=1">★1</option><option value="2" data-href="/?rating=2">★2</option><option value="3" data-href="/?rating=3">★3</option><option value="4" data-href="/?rating=4">★4</option><option value="5" data-href="/?rating=5">★5</option><option value="0" data-href="/?rating=0">☆</option></select>"#
            ),
            "{html}"
        );
        // ★1〜2 を付けたカードは、その場でも隠す（`MARKS_SCRIPT`）
        assert!(
            html.contains(r#"<div class="sections" data-read="0" data-hide-low="1">"#),
            "{html}"
        );
        assert!(MARKS_SCRIPT.contains("f.hideLow"), "{MARKS_SCRIPT}");
        assert!(
            html.contains(
                r#"<a class="btn" href="/?bookmarked=1" aria-label="ブックマーク：絞らない（押すとブックマーク中だけ）" title="ブックマーク：絞らない（押すとブックマーク中だけ）">🔖</a>"#
            ),
            "{html}"
        );
        // 絞らない一覧では欄に評価の条件を持たせない
        let any = ListView {
            rating: RatingFilter::Any,
            ..ListView::default()
        };
        let html = list_page(&[], &[], any, &Page::default());
        assert!(
            html.contains(r#"<form class="stars" method="get" action="/">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<option value="hide-low" data-href="/" data-closed="★3+☆">★1〜2 を隠す</option>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="sections" data-read="0">"#),
            "{html}"
        );
    }

    /// 絞り込んだ画面は、上部のバーと該当する記事だけを出す。検索のフォームも、効かない最低点と 👁 も出さない。
    /// 絞り込みはもう一方の状態を引き継ぎ、👍 を選び直すか 🔖 を外すと一覧に戻る。
    #[test]
    fn filtered_page_shows_the_bar_and_the_matches() {
        // 絞り込んだ画面の既定は、既読も出す
        let view = ListView {
            min: Some(0),
            rating: at_least(4),
            read: None,
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
                r#"<a class="btn" href="/?rating=4&amp;read=1" aria-label="既読：絞らない（押すと既読だけ）" title="既読：絞らない（押すと既読だけ）">"#
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
                r#"<a class="btn" href="/?rating=4&amp;bookmarked=1" aria-label="ブックマーク：絞らない（押すとブックマーク中だけ）" title="ブックマーク：絞らない（押すとブックマーク中だけ）">🔖</a>"#
            ),
            "{html}"
        );
        // 全期間から探した画面だと見出しで分かるようにする。条件から外れたカードはその場で隠して「元に戻す」を出し、
        // 件数も合わせる（`MARKS_SCRIPT`）
        assert!(
            html.contains(
                r#"<h2>★4 以上（全期間）<span class="count">1 件</span></h2><div class="sections" data-min-rating="4">"#
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
            min: Some(0),
            rating: at_least(4),
            bookmarked: Some(true),
            read: None,
            ..ListView::default()
        };
        let html = filtered_page(
            &[item(2, "2026-09-27T05:00:00.000Z")],
            view,
            &Page::default(),
        );
        assert!(
            html.contains(r#"<h2>ブックマーク中・★4 以上（全期間）<span class="count">1 件</span></h2><div class="sections" data-bookmarked="1" data-min-rating="4">"#),
            "{html}"
        );
        let html = filtered_page(&[], view, &Page::default());
        assert!(
            html.contains(r#"<input type="hidden" name="bookmarked" value="1">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="btn on" href="/?rating=4&amp;bookmarked=0" aria-label="ブックマーク：ブックマーク中だけ（押すとブックマークなしだけ）""#
            ),
            "{html}"
        );
        assert!(html.contains("該当する記事はありません"), "{html}");
        // 0 件でも、何で絞ったかと全期間であることは見出しに出す
        assert!(
            html.contains(
                r#"<h2>ブックマーク中・★4 以上（全期間）<span class="count">0 件</span></h2>"#
            ),
            "{html}"
        );
        let view = ListView {
            min: Some(0),
            bookmarked: Some(true),
            read: None,
            ..ListView::default()
        };
        let html = filtered_page(&[], view, &Page::default());
        assert!(
            html.contains(r#"href="/?bookmarked=0" aria-label="ブックマーク：ブックマーク中だけ（押すとブックマークなしだけ）""#),
            "{html}"
        );

        // 👁 を OFF にした絞り込みは、既読を隠し（その場でも隠す）、絞り込みを変えても OFF を引き継ぐ
        let view = ListView {
            min: Some(0),
            rating: at_least(4),
            ..ListView::default()
        };
        let html = filtered_page(
            &[item(3, "2026-09-27T05:00:00.000Z")],
            view,
            &Page::default(),
        );
        assert!(
            html.contains(r#"href="/?rating=4" aria-label="既読：未読だけ（押すと絞らない）""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="sections" data-read="0" data-min-rating="4">"#),
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
            min: Some(0),
            bookmarked: Some(true),
            ..ListView::default()
        };
        let html = filtered_page(&[], bookmark_off, &Page::default());
        assert!(
            html.contains(r#"href="/?bookmarked=0" aria-label="ブックマーク：ブックマーク中だけ（押すとブックマークなしだけ）""#),
            "{html}"
        );
        let from_list = ListView {
            read: None,
            ..ListView::default()
        };
        let html = list_page(&[], &[], from_list, &Page::default());
        assert!(
            html.contains(r#"href="/?bookmarked=1" aria-label="ブックマーク：絞らない（押すとブックマーク中だけ）""#),
            "{html}"
        );
        // JavaScript が無いときの評価の選択は、今の条件と、どちらの画面から送ったかを送る。行き先が一覧か絞り込みかは
        // 選んだ評価で決まるので、行き来したときに 👁・最低点を行き先の既定に戻すのは受け取った側で行う
        let stars = html.split(r#"<form class="stars"#).nth(1).unwrap();
        let stars = stars.split("</form>").next().unwrap();
        assert!(
            stars.contains(r#"<input type="hidden" name="read" value="any">"#)
                && stars.contains(r#"<input type="hidden" name="from" value="list">"#),
            "{stars}"
        );
    }

    /// JavaScript が無いとき、一覧の評価の選択はブックマークなしだけの条件も送る（JavaScript のときの行き先と同じく
    /// 引き継ぐ）。一覧の既定の最低点と 👁 は送らない。
    #[test]
    fn no_js_rating_keeps_the_bookmark_condition() {
        let view = ListView {
            bookmarked: Some(false),
            ..ListView::default()
        };
        let html = list_page(&[], &[], view, &Page::default());
        let stars = html.split(r#"<form class="stars"#).nth(1).unwrap();
        let stars = stars.split("</form>").next().unwrap();
        assert!(
            stars.contains(r#"<input type="hidden" name="bookmarked" value="0">"#),
            "{stars}"
        );
        assert!(
            !stars.contains(r#"name="read""#) && !stars.contains(r#"name="min""#),
            "{stars}"
        );
        assert!(
            stars.contains(r#"data-href="/?rating=4&amp;bookmarked=0""#),
            "{stars}"
        );
        let filtered = ListView {
            min: Some(0),
            read: None,
            rating: at_least(4),
            ..ListView::default()
        };
        let html = filtered_page(&[], filtered, &Page::default());
        assert!(
            html.contains(r#"<input type="hidden" name="from" value="filtered">"#),
            "{html}"
        );
    }

    /// 👁 と 🔖 は押すたびに 絞らない（白）→ 印のある記事だけ（緑）→ 印の無い記事だけ（赤・斜線）と切り替える。
    /// 一覧の 👁 の既定は未読だけ（赤）、絞り込みの既定は絞らない（白）。今の状態と押したときの次を名前に出す。
    #[test]
    fn bar_marks_cycle_through_three_states() {
        assert!(STYLE.contains(".btn.not {"), "{STYLE}");
        let eye = |read: Option<bool>| {
            let html = list_page(
                &[],
                &[],
                ListView {
                    read,
                    ..ListView::default()
                },
                &Page::default(),
            );
            let at = html.find("👁</a>").expect(&html);
            let start = html[..at].rfind("<a ").unwrap();
            html[start..at].to_string()
        };
        assert_eq!(
            eye(Some(false)),
            r#"<a class="btn not" href="/?read=any" aria-label="既読：未読だけ（押すと絞らない）" title="既読：未読だけ（押すと絞らない）">"#
        );
        assert!(
            eye(None).starts_with(
                r#"<a class="btn" href="/?read=1" aria-label="既読：絞らない（押すと既読だけ）""#
            ),
            "{}",
            eye(None)
        );
        assert!(
            eye(Some(true)).starts_with(
                r#"<a class="btn on" href="/" aria-label="既読：既読だけ（押すと未読だけ）""#
            ),
            "{}",
            eye(Some(true))
        );
        let bookmark = |view: ListView| {
            let html = filtered_page(&[], view, &Page::default());
            let at = html.find("🔖</a>").expect(&html);
            let start = html[..at].rfind("<a ").unwrap();
            html[start..at].to_string()
        };
        // ブックマーク中だけ（絞り込み）→ ブックマークなしだけ（一覧の既定へ戻る）
        let only = ListView {
            min: Some(0),
            read: None,
            bookmarked: Some(true),
            ..ListView::default()
        };
        assert!(
            bookmark(only).starts_with(r#"<a class="btn on" href="/?bookmarked=0" aria-label="ブックマーク：ブックマーク中だけ（押すとブックマークなしだけ）""#),
            "{}",
            bookmark(only)
        );
        let html = list_page(
            &[],
            &[],
            ListView {
                bookmarked: Some(false),
                ..ListView::default()
            },
            &Page::default(),
        );
        assert!(
            html.contains(r#"<a class="btn not" href="/" aria-label="ブックマーク：ブックマークなしだけ（押すと絞らない）""#),
            "{html}"
        );
        // 欄の条件は印ごとに、あり（1）・なし（0）で持つ
        assert!(
            html.contains(
                r#"<div class="sections" data-read="0" data-bookmarked="0" data-hide-low="1">"#
            ),
            "{html}"
        );
        assert!(
            MARKS_SCRIPT.contains("want(f.read") && MARKS_SCRIPT.contains("want(f.bookmarked"),
            "{MARKS_SCRIPT}"
        );
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
                min: Some(0),
                rating: at_least(4),
                read: None,
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
            min: Some(0),
            read: None,
            rating: RatingFilter::Unrated,
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
        let scored = ListView {
            min: Some(60),
            ..unrated
        };
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
        // 一覧へ戻ると、最低点は設定の最低点に戻る。選んだ評価の条件はそのまま
        assert!(
            html.contains(
                r#"<option value="any" data-href="/?rating=any" data-closed="★">-</option>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<option value="hide-low" data-href="/" data-closed="★3+☆">★1〜2 を隠す</option>"#
            ),
            "{html}"
        );
        // 一覧から絞り込みへ移ると、最低点は 00 になる
        let list = ListView {
            min: Some(30),
            ..ListView::default()
        };
        let html = list_page(&[], &[], list, &Page::default());
        assert!(
            html.contains(r#"<option value="4" data-href="/?rating=4">★4</option>"#),
            "{html}"
        );
    }

    /// 一覧に出ない記事があるときは、どの条件で何件隠れているかを、その条件を外した表示へのリンクにして出す。
    /// 最低点を外すと、点数の足りない記事のほかに未採点（軽水炉と無関係で採点しない記事を含む）も加わる。
    /// 1 件も出ないときは、「新しい記事はありません」の代わりに効いている条件を出す（何が記事を隠しているか分かるように）。
    #[test]
    fn list_page_explains_what_the_conditions_hide() {
        let view = ListView {
            min: Some(50),
            default_min: Some(50),
            ..ListView::default()
        };
        let hidden = HiddenCounts {
            min: 32,
            read: 12,
            rating: 3,
            bookmarked: 0,
        };
        let html = list_page_with_explore(&[], &[], &[], view, hidden, &Page::default());
        assert!(
            html.contains(
                r#"<p class="meta">点数 50 以上・未読だけ・★1〜2 を隠す に合う記事はありません</p>"#
            ),
            "{html}"
        );
        assert!(!html.contains("新しい記事はありません"), "{html}");
        assert!(
            html.contains(
                r#"<p class="meta">条件で隠れている記事：<a href="/?min=0">点数 50 未満・未採点 32 件</a>・<a href="/?read=any">既読 12 件</a>・<a href="/?rating=any">★1〜2 3 件</a></p>"#
            ),
            "{html}"
        );
        // 記事が出ていれば、欄の見出しはそのままで、隠れている件数だけを後に添える
        let html = list_page_with_explore(
            &[],
            &[item(1, "2026-09-26T00:00:00.000Z")],
            &[],
            view,
            HiddenCounts {
                read: 2,
                ..HiddenCounts::default()
            },
            &Page::default(),
        );
        assert!(html.contains("新しい記事はありません"), "{html}");
        let cards = html.find(r#"data-id="1""#).unwrap();
        let note = html.find("条件で隠れている記事").unwrap();
        assert!(cards < note, "{html}");
        assert!(
            html.contains(r#"<a href="/?read=any">既読 2 件</a></p>"#),
            "{html}"
        );
        // 既読だけ・ブックマークなしだけで隠れているものも、外した表示へのリンクにする
        let view = ListView {
            min: Some(50),
            default_min: Some(50),
            read: Some(true),
            rating: RatingFilter::Any,
            bookmarked: Some(false),
        };
        let html = list_page_with_explore(
            &[],
            &[],
            &[],
            view,
            HiddenCounts {
                read: 5,
                bookmarked: 1,
                ..HiddenCounts::default()
            },
            &Page::default(),
        );
        assert!(
            html.contains("点数 50 以上・既読だけ・ブックマークなしだけ に合う記事はありません"),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/?rating=any&amp;read=any&amp;bookmarked=0">未読 5 件</a>・<a href="/?rating=any&amp;read=1">ブックマーク中 1 件</a>"#),
            "{html}"
        );
        // 何も隠れていなければ出さない
        let html = list_page(&[], &[], ListView::default(), &Page::default());
        assert!(!html.contains("条件で隠れている記事"), "{html}");
    }

    #[test]
    fn list_page_names_the_earlier_section_by_whether_read_is_shown() {
        let mut read = item(2, "2026-09-26T00:00:00.000Z");
        read.read_at = Some("2026-09-26T12:00:00.000Z".into());
        let earlier = [read];
        let html = list_page(&[], &earlier, ListView::default(), &Page::default());
        assert!(html.contains("<h2>過去の未読</h2>"), "{html}");
        let view = ListView {
            read: None,
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
            HiddenCounts::default(),
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
