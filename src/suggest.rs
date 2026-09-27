//! `profile suggest` の表示：現行から案への差分（Rust で計算したもの）と、LLM が挙げた根拠。

use std::fmt::Write as _;

use crate::profile::Profile;
use crate::prompt::suggest::Suggestion;

/// 差分が無ければ `None`（案を書き出す必要が無い）。
pub fn render(current: &Profile, suggestion: &Suggestion, out: &str) -> Option<String> {
    let changes = crate::profile::diff(current, &suggestion.profile);
    if changes.is_empty() {
        return None;
    }
    let mut text = String::from("changes:\n");
    for change in &changes {
        let _ = writeln!(text, "  {change}");
    }
    if !suggestion.reasons.is_empty() {
        text.push_str("reasons:\n");
        for r in &suggestion.reasons {
            let _ = writeln!(text, "  {}: {}", r.change, r.evidence);
        }
    }
    let _ = writeln!(
        text,
        "\nwrote {out}. compare it with `nucrawler eval --profile {out}`, \
         then `nucrawler profile import {out}` to adopt it"
    );
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Interest;
    use crate::prompt::suggest::Reason;

    fn profile(weight: f64) -> Profile {
        Profile {
            interests: vec![Interest {
                topic: "燃料".into(),
                weight,
                note: None,
            }],
            exclude: vec![],
        }
    }

    #[test]
    fn renders_changes_reasons_and_next_steps() {
        let suggestion = Suggestion {
            profile: profile(0.7),
            reasons: vec![Reason {
                change: "燃料の重みを下げた".into(),
                evidence: "不要 3 件（ATF の記事など）".into(),
            }],
        };
        let out = render(&profile(0.9), &suggestion, "new.toml").unwrap();
        assert_eq!(
            out,
            "changes:\n\
             \x20 weight 燃料: 0.9 → 0.7\n\
             reasons:\n\
             \x20 燃料の重みを下げた: 不要 3 件（ATF の記事など）\n\
             \n\
             wrote new.toml. compare it with `nucrawler eval --profile new.toml`, \
             then `nucrawler profile import new.toml` to adopt it\n"
        );
    }

    /// 次の手順はそのまま貼り付けて実行できるよう、パスをシェル向けに引用する。
    #[test]
    fn quotes_paths_for_the_shell() {
        let suggestion = Suggestion {
            profile: profile(0.7),
            reasons: vec![],
        };
        let out = render(&profile(0.9), &suggestion, "my profile's.toml").unwrap();
        assert!(
            out.contains("nucrawler eval --profile 'my profile'\\''s.toml'"),
            "{out}"
        );
        let plain = render(&profile(0.9), &suggestion, "dir/new-1.toml").unwrap();
        assert!(plain.contains("--profile dir/new-1.toml`"), "{plain}");
    }

    #[test]
    fn nothing_to_write_without_changes() {
        let suggestion = Suggestion {
            profile: profile(0.9),
            reasons: vec![],
        };
        assert_eq!(render(&profile(0.9), &suggestion, "new.toml"), None);
    }
}
