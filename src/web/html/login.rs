//! ログイン画面。

use super::*;

/// ログイン画面。`next` はログイン後に戻る画面（サーバーが自分のホストを指すかを確かめてから使う）。
/// 失敗したときの文言は、ID が無い・パスワードが違う・待ち時間中のどれでも同じ（ID の有無を漏らさないため）。
pub fn login_page(next: Option<&str>, failed: bool, labels: &SourceLabels) -> String {
    let message = if failed {
        "<p class=\"warn\">ログイン ID かパスワードが違うか、失敗が続いたため今はログインできません。</p>"
    } else {
        ""
    };
    let next = next
        .map(|n| {
            format!(
                "<input type=\"hidden\" name=\"next\" value=\"{}\">",
                escape(n)
            )
        })
        .unwrap_or_default();
    let body = format!(
        "<h1>ログイン</h1>{message}\
         <form method=\"post\" action=\"/login\">{next}\
         <p><label>ログイン ID<br><input name=\"login\" type=\"email\" autocomplete=\"username\" required></label></p>\
         <p><label>パスワード<br><input name=\"password\" type=\"password\" autocomplete=\"current-password\" required></label></p>\
         <p><button>ログイン</button></p></form>\
         <p class=\"meta\">パスワードを忘れたら、管理者にリセットを頼んでください。</p>"
    );
    let page = Page {
        warnings: &[],
        labels,
        default_min: None,
    };
    layout("ログイン", &page, &body)
}
