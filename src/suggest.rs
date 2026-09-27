//! `profile suggest` の表示：現行から案への差分（Rust で計算したもの）と、LLM が挙げた根拠。

use crate::profile::Profile;
use crate::prompt::suggest::Suggestion;

/// 差分が無ければ `None`（案を書き出す必要が無い）。
pub fn render(current: &Profile, suggestion: &Suggestion, out: &str) -> Option<String> {
    todo!("{current:?} {suggestion:?} {out}")
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

    #[test]
    fn nothing_to_write_without_changes() {
        let suggestion = Suggestion {
            profile: profile(0.9),
            reasons: vec![],
        };
        assert_eq!(render(&profile(0.9), &suggestion, "new.toml"), None);
    }
}
