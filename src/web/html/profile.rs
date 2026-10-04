//! 興味プロファイルの画面（計画 016）。

use super::*;

use crate::db::{
    ProfileOrigin, ProfileSuggestion, ProfileVersion, SuggestionStatus, SuggestionTrigger,
    VersionStats,
};
use crate::profile::{Change, Profile};

/// 興味プロファイルの画面の材料。
#[derive(Debug, Clone, Copy)]
pub struct ProfileView<'a> {
    /// 版（新しい順。先頭が今のプロファイル）
    pub versions: &'a [ProfileVersion],
    /// 利用者の案（新しい順）
    pub suggestions: &'a [ProfileSuggestion],
    /// 案を自動で当てるか
    pub auto_apply: bool,
    /// 見直しを頼んで、まだ案ができていない
    pub requested: bool,
}

/// 今のプロファイル、更新案（自動で当てるかの切り替え・今すぐ作る・待っている案の採用と見送り）、
/// 版の履歴（新しい順。今でない版には「この版に戻す」）。
pub fn profile_page(view: ProfileView, page: &Page) -> String {
    let versions = view.versions;
    let mut body = String::from(
        "<p class=\"meta\"><a href=\"/settings\">← 設定</a></p><h1>興味プロファイル</h1>",
    );
    let Some(current) = versions.first() else {
        body.push_str("<p>プロファイルはまだありません。</p>");
        return layout("興味プロファイル", page, &body);
    };
    body.push_str("<h2>今のプロファイル</h2>");
    body.push_str(&profile_contents(&current.profile));
    body.push_str(&suggestion_section(view, &current.profile));
    body.push_str(
        "<h2>履歴</h2>\
         <p class=\"meta\">一致率は、その版が使われていた間に付けた評価で、評価の高い記事ほど点数が高い組の割合\
         （0.5 が当て推量。案の根拠にした記事は除く）。件数が少ないうちは揺れます。前の版に戻すと、\
         その版の採点がそのまま使われます。</p>",
    );
    for (i, v) in versions.iter().enumerate() {
        let changes = match versions.get(i + 1) {
            Some(older) => crate::profile::diff(&older.profile, &v.profile)
                .iter()
                .map(|c| format!("<li>{}</li>", escape(&change_text(c))))
                .collect::<String>(),
            None => "<li>最初の版</li>".to_string(),
        };
        let state = if v.retired_at.is_none() {
            "今の版".to_string()
        } else {
            format!(
                "<form method=\"post\" action=\"/settings/profile/versions/{}/revert\">\
                 <button>この版に戻す</button></form>",
                v.id
            )
        };
        let concordance = v
            .stats
            .concordance
            .map(|c| format!("・一致率 {c:.2}"))
            .unwrap_or_default();
        body.push_str(&format!(
            "<details class=\"term\"><summary><b>#{} {}</b> {}\
             <span class=\"meta\">評価 {} 件{concordance}</span></summary>\
             <ul>{changes}</ul>{state}</details>",
            v.id,
            crate::jst::format_local(&v.created_at),
            origin_label(v.origin),
            v.stats.rated,
        ));
    }
    layout("興味プロファイル", page, &body)
}

/// 更新案の節。
fn suggestion_section(view: ProfileView, current: &Profile) -> String {
    let checked = if view.auto_apply { " checked" } else { "" };
    let mut out = format!(
        "<h2>更新案</h2>\
         <p class=\"meta\">評価が 10 件増えるたびに、評価を根拠に LLM が案を作り、今のプロファイルと同じ評価で\
         一致率を比べます。</p>\
         <form method=\"post\" action=\"/settings/profile/auto-apply\">\
         <label><input type=\"checkbox\" name=\"auto\" value=\"on\"{checked}> \
         案の一致率が十分に高ければ、自動で当てる（当てても履歴から戻せます）</label> \
         <button>保存</button></form>"
    );
    if view.requested {
        out.push_str("<p>案を作っています（15 分ごとの処理で作ります）。</p>");
    } else {
        out.push_str(
            "<form method=\"post\" action=\"/settings/profile/review\">\
             <button>今すぐ案を作る</button></form>",
        );
    }
    let pending = view
        .suggestions
        .iter()
        .find(|s| s.status == SuggestionStatus::Pending);
    match (pending, view.suggestions.first()) {
        (Some(s), _) => out.push_str(&pending_suggestion(s, current)),
        (None, Some(last)) => out.push_str(&format!(
            "<p class=\"meta\">前回の見直し：{}（{}・{}）</p>",
            crate::jst::format_local(&last.created_at),
            trigger_label(last.trigger),
            status_label(last.status),
        )),
        (None, None) => {}
    }
    out
}

/// 待っている案：今との違い・根拠・今と案の一致率、採用と見送り。
fn pending_suggestion(s: &ProfileSuggestion, current: &Profile) -> String {
    let changes: String = crate::profile::diff(current, &s.profile)
        .iter()
        .map(|c| format!("<li>{}</li>", escape(&change_text(c))))
        .collect();
    let reasons: String = s
        .reasons
        .iter()
        .map(|r| {
            format!(
                "<li>{}：<span class=\"meta\">{}</span></li>",
                escape(&r.change),
                escape(&r.evidence)
            )
        })
        .collect();
    format!(
        "<h3>待っている案 <span class=\"meta\">{}・{}</span></h3>\
         <p>一致率：今 {} → 案 {}<br><span class=\"meta\">案の値は、案の根拠にした評価で測るので甘めに出ます。\
         </span></p>\
         <ul>{changes}</ul><p>根拠</p><ul>{reasons}</ul>\
         <form method=\"post\" action=\"/settings/profile/suggestions/{id}/apply\">\
         <button>採用する</button></form>\
         <form method=\"post\" action=\"/settings/profile/suggestions/{id}/dismiss\">\
         <button>見送る</button></form>",
        crate::jst::format_local(&s.created_at),
        trigger_label(s.trigger),
        stats_text(s.current),
        stats_text(s.candidate),
        id = s.id,
    )
}

fn stats_text(stats: VersionStats) -> String {
    match stats.concordance {
        Some(c) => format!("{c:.2}（評価 {} 件）", stats.rated),
        None => format!("-（評価 {} 件）", stats.rated),
    }
}

fn trigger_label(trigger: SuggestionTrigger) -> &'static str {
    match trigger {
        SuggestionTrigger::Auto => "評価が増えたので作成",
        SuggestionTrigger::Manual => "頼まれて作成",
    }
}

fn status_label(status: SuggestionStatus) -> &'static str {
    match status {
        SuggestionStatus::Pending => "待っている",
        SuggestionStatus::Applied => "当てた",
        SuggestionStatus::Dismissed => "見送った",
        SuggestionStatus::Superseded => "新しい案に置き換えた",
        SuggestionStatus::Unchanged => "変える根拠なし",
    }
}

/// 関心分野（重みと補足）と推薦しない話題。
fn profile_contents(profile: &Profile) -> String {
    let interests: String = profile
        .interests
        .iter()
        .map(|i| {
            let note = i
                .note
                .as_deref()
                .map(|n| format!("<br><span class=\"meta\">{}</span>", escape(n)))
                .unwrap_or_default();
            format!(
                "<li><b>{}</b>（重み {}）{note}</li>",
                escape(&i.topic),
                i.weight
            )
        })
        .collect();
    let excludes = if profile.exclude.is_empty() {
        "なし".to_string()
    } else {
        escape(&profile.exclude.join("、"))
    };
    format!("<h3>関心分野</h3><ul>{interests}</ul><h3>推薦しない話題</h3><p>{excludes}</p>")
}

fn origin_label(origin: ProfileOrigin) -> &'static str {
    match origin {
        ProfileOrigin::Import => "取り込み",
        ProfileOrigin::Suggest => "案を採用",
        ProfileOrigin::Auto => "自動で適用",
        ProfileOrigin::Revert => "前の版に戻した",
    }
}

/// 1 つ前の版からの変更の説明。
fn change_text(change: &Change) -> String {
    match change {
        Change::Added {
            topic,
            weight,
            note,
        } => match note {
            Some(note) => format!("{topic}を追加（重み {weight}、{note}）"),
            None => format!("{topic}を追加（重み {weight}）"),
        },
        Change::Removed { topic } => format!("{topic}を削除"),
        Change::Weight { topic, from, to } => format!("{topic}の重み {from} → {to}"),
        Change::Note { topic, to, .. } => {
            format!("{topic}の補足を「{}」に", to.as_deref().unwrap_or(""))
        }
        Change::ExcludeAdded(e) => format!("推薦しない話題に{e}を追加"),
        Change::ExcludeRemoved(e) => format!("推薦しない話題から{e}を削除"),
    }
}
