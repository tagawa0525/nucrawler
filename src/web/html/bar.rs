//! 上部のバー（最低点・評価の選択、既読・ブックマークの切り替え）。一覧・絞り込み・検索で共有する。

use super::*;

/// 最低点の選択肢の刻み（0〜90）
const MIN_STEP: u8 = 10;

/// 印で絞る切り替え（👁・🔖）の次の状態。絞らない → 印のある記事だけ → 印の無い記事だけ → 絞らない。
fn next_mark(mark: Option<bool>) -> Option<bool> {
    match mark {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    }
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

/// JavaScript が無いときに選択と一緒に送る、今の表示のほかの条件（正規の URL の、`except` 以外の欄）。
pub(super) fn state_inputs(view: &impl BarView, except: &[&str]) -> String {
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
