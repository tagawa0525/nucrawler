//! 興味プロファイルの画面（計画 016）。

use super::*;

use crate::db::{ProfileOrigin, ProfileVersion};
use crate::profile::{Change, Profile};

/// 今のプロファイル（`versions` の先頭）と版の履歴（新しい順）。今でない版には「この版に戻す」を出す。
pub fn profile_page(versions: &[ProfileVersion], page: &Page) -> String {
    let mut body = String::from(
        "<p class=\"meta\"><a href=\"/settings\">← 設定</a></p><h1>興味プロファイル</h1>",
    );
    let Some(current) = versions.first() else {
        body.push_str("<p>プロファイルはまだありません。</p>");
        return layout("興味プロファイル", page, &body);
    };
    body.push_str("<h2>今のプロファイル</h2>");
    body.push_str(&profile_contents(&current.profile));
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
