//! `eval`：明示的な反応を正解ラベルにして、採点のキーごとに点数が正例と負例をどれだけ分けているかを表示する。

use std::fmt::Write as _;

use crate::db::{EvalKey, Label, LabeledScore, SignalKind};

/// 正例・負例のどちらかがこれより少なければ、指標は参考値と注記する
const FEW_LABELS: usize = 5;

/// 正例と負例の組のうち、正例の点数が高い割合（同点は半分と数える）。どちらかが空なら `None`。
pub fn auc(positive: &[u8], negative: &[u8]) -> Option<f64> {
    if positive.is_empty() || negative.is_empty() {
        return None;
    }
    // 同点を半分と数えるため、2 倍で数える
    let twice: usize = positive
        .iter()
        .flat_map(|p| negative.iter().map(move |n| p.cmp(n)))
        .map(|o| match o {
            std::cmp::Ordering::Greater => 2,
            std::cmp::Ordering::Equal => 1,
            std::cmp::Ordering::Less => 0,
        })
        .sum();
    Some(twice as f64 / (2 * positive.len() * negative.len()) as f64)
}

/// 10 点刻みの点数帯の下限。100 点は 90 帯に入れる。
pub fn band(score: u8) -> u8 {
    (score.min(99) / 10) * 10
}

/// 評価の表示。`current` は現行のプロファイルの hash、`candidate` は候補のプロファイルの hash、
/// `version` は今の score のプロンプトの版。`all` が偽なら、今の版の現行と候補のキーだけを出す。
pub fn render(
    labels: &[Label],
    scores: &[LabeledScore],
    current: Option<&str>,
    candidate: Option<&str>,
    version: i64,
    all: bool,
) -> String {
    let current = current.map(|hash| (hash, version));
    let mut out = String::new();
    let count = |kind| labels.iter().filter(|l| l.kind == kind).count();
    let (up, bookmark) = (count(SignalKind::Up), count(SignalKind::Bookmark));
    let (down, dismiss) = (count(SignalKind::Down), count(SignalKind::Dismiss));
    let _ = writeln!(
        out,
        "labels: {} positive (up {up}, bookmark {bookmark}), {} negative (down {down}, dismiss {dismiss})",
        up + bookmark,
        down + dismiss
    );
    if up + bookmark < FEW_LABELS || down + dismiss < FEW_LABELS {
        let _ = writeln!(
            out,
            "note: fewer than {FEW_LABELS} positive or negative labels; treat the numbers as rough"
        );
    }
    let is_current = |k: &EvalKey| {
        current.is_some_and(|(hash, version)| k.profile_hash == hash && k.prompt_version == version)
    };
    // 候補は今の版のプロンプトで採点する。現行のプロファイルが無くても判定できるようにする
    let is_candidate = |k: &EvalKey| {
        k.prompt_version == version
            && candidate.is_some_and(|hash| k.profile_hash == hash)
            && !is_current(k)
    };
    let mut keys: Vec<&EvalKey> = scores.iter().map(|s| &s.key).collect();
    keys.sort_by_key(|k| (!is_current(k), !is_candidate(k), *k));
    keys.dedup();
    if !all {
        keys.retain(|k| is_current(k) || is_candidate(k));
        // 候補だけ採点済みでも、比べる相手が無いことを示す
        if !keys.iter().any(|k| is_current(k)) {
            out.push('\n');
            if current.is_none() {
                let _ = writeln!(
                    out,
                    "no profile; import one with `nucrawler profile import FILE`"
                );
            } else {
                let _ = writeln!(
                    out,
                    "no scores for the current profile and prompt version (see --all)"
                );
            }
        }
    }
    for key in keys {
        out.push('\n');
        let role = if is_current(key) {
            "  (current)"
        } else if is_candidate(key) {
            "  (candidate)"
        } else {
            ""
        };
        render_key(&mut out, key, role, labels, scores);
    }
    out
}

/// 1 つのキーの結果（`role` は現行・候補の印）：カバー率、AUC、反応より後に採点した件数、点数帯ごとの正例と負例。
fn render_key(
    out: &mut String,
    key: &EvalKey,
    role: &str,
    labels: &[Label],
    scores: &[LabeledScore],
) {
    let hash: String = key.profile_hash.chars().take(8).collect();
    let _ = writeln!(
        out,
        "profile {hash}  {}/{}  prompt v{}{role}",
        key.backend, key.model, key.prompt_version,
    );
    // ラベルと突き合わせた (点数, 正例か, 反応より後に採点したか)
    let matched: Vec<(u8, bool, bool)> = scores
        .iter()
        .filter(|s| &s.key == key)
        .filter_map(|s| {
            let label = labels.iter().find(|l| l.article_id == s.article_id)?;
            Some((s.score, label.positive(), s.scored_at > label.at))
        })
        .collect();
    let pick = |positive: bool| -> Vec<u8> {
        matched
            .iter()
            .filter(|m| m.1 == positive)
            .map(|m| m.0)
            .collect()
    };
    let auc = auc(&pick(true), &pick(false)).map_or_else(|| "-".to_string(), |a| format!("{a:.2}"));
    let _ = writeln!(
        out,
        "  scored {}/{}  AUC {auc}",
        matched.len(),
        labels.len()
    );
    let late = matched.iter().filter(|m| m.2).count();
    if key.prompt_version == 1 && late > 0 {
        // 版 1 の採点のプロンプトは直近の反応の見出しを含むので、反応の後の採点は甘くなりうる
        let _ = writeln!(
            out,
            "  note: {late} scored after the reaction; the article's own title may have been a signal, so AUC may be high"
        );
    }
    let _ = writeln!(out, "  {:<8}{:>5}{:>6}", "score", "pos", "neg");
    for b in (0..10).rev().map(|i| i * 10) {
        let in_band = |positive: bool| {
            matched
                .iter()
                .filter(|m| band(m.0) == b && m.1 == positive)
                .count()
        };
        let (pos, neg) = (in_band(true), in_band(false));
        if pos + neg == 0 {
            continue;
        }
        let label = if b == 90 {
            "90-100".to_string()
        } else {
            format!("{b}-{}", b + 9)
        };
        let _ = writeln!(out, "  {label:<8}{pos:>5}{neg:>6}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auc_is_the_share_of_correctly_ordered_pairs() {
        assert_eq!(auc(&[90, 80], &[10, 20]), Some(1.0));
        assert_eq!(auc(&[10], &[90]), Some(0.0));
        assert_eq!(auc(&[50], &[50]), Some(0.5));
        // 組は (90,50) 正、(90,70) 正、(40,50) 誤、(40,70) 誤
        assert_eq!(auc(&[90, 40], &[50, 70]), Some(0.5));
        assert_eq!(auc(&[80, 60], &[60]), Some(0.75));
        assert_eq!(auc(&[], &[10]), None);
        assert_eq!(auc(&[10], &[]), None);
    }

    #[test]
    fn bands_are_tens_with_100_in_the_top_band() {
        assert_eq!(band(0), 0);
        assert_eq!(band(49), 40);
        assert_eq!(band(50), 50);
        assert_eq!(band(90), 90);
        assert_eq!(band(100), 90);
    }

    fn label(article_id: i64, kind: SignalKind) -> Label {
        Label {
            article_id,
            kind,
            at: "2026-09-27T00:00:00.000Z".into(),
        }
    }

    fn key(profile_hash: &str, prompt_version: i64) -> EvalKey {
        EvalKey {
            profile_hash: profile_hash.into(),
            backend: "claude-cli".into(),
            model: "sonnet".into(),
            prompt_version,
        }
    }

    fn scored(key: &EvalKey, article_id: i64, score: u8, scored_at: &str) -> LabeledScore {
        LabeledScore {
            key: key.clone(),
            article_id,
            score,
            scored_at: scored_at.into(),
        }
    }

    #[test]
    fn renders_labels_and_the_current_key() {
        let labels = [
            label(1, SignalKind::Up),
            label(2, SignalKind::Bookmark),
            label(3, SignalKind::Dismiss),
        ];
        let current = key("0123456789abcdef", 1);
        let old = key("fedcba9876543210", 1);
        let before = "2026-09-26T00:00:00.000Z";
        let scores = [
            scored(&current, 1, 95, before),
            // 反応の後に採点し直した
            scored(&current, 2, 72, "2026-09-28T00:00:00.000Z"),
            scored(&current, 3, 40, before),
            scored(&old, 1, 10, before),
        ];
        let out = render(&labels, &scores, Some("0123456789abcdef"), None, 1, false);
        assert!(
            out.starts_with(
                "labels: 2 positive (up 1, bookmark 1), 1 negative (down 0, dismiss 1)\n"
            ),
            "{out}"
        );
        assert!(out.contains("fewer than 5"), "{out}");
        assert!(
            out.contains("profile 01234567  claude-cli/sonnet  prompt v1  (current)"),
            "{out}"
        );
        assert!(out.contains("scored 3/3  AUC 1.00"), "{out}");
        assert!(out.contains("1 scored after the reaction"), "{out}");
        assert!(out.contains("90-100      1     0"), "{out}");
        assert!(out.contains("70-79       1     0"), "{out}");
        assert!(out.contains("40-49       0     1"), "{out}");
        // 既定では現行のキーだけ
        assert!(!out.contains("fedcba98"), "{out}");
        let all = render(&labels, &scores, Some("0123456789abcdef"), None, 1, true);
        assert!(
            all.contains("profile fedcba98  claude-cli/sonnet  prompt v1\n"),
            "{all}"
        );
        assert!(all.contains("scored 1/3  AUC -"), "{all}");
    }

    /// 反応の見出しをプロンプトに入れていたのは版 1 だけなので、それ以降の版には注記しない。
    #[test]
    fn notes_late_scores_only_for_prompt_v1() {
        let labels = [label(1, SignalKind::Up), label(2, SignalKind::Dismiss)];
        let v2 = key("h", 2);
        let after = "2026-09-28T00:00:00.000Z";
        let scores = [scored(&v2, 1, 80, after), scored(&v2, 2, 20, after)];
        let out = render(&labels, &scores, Some("h"), None, 2, false);
        assert!(out.contains("scored 2/2  AUC 1.00"), "{out}");
        assert!(!out.contains("after the reaction"), "{out}");
    }

    #[test]
    fn shows_the_candidate_next_to_the_current_key() {
        let labels = [label(1, SignalKind::Up), label(2, SignalKind::Dismiss)];
        let current = key("aaaaaaaaaaaa", 2);
        let candidate = key("bbbbbbbbbbbb", 2);
        let other = key("cccccccccccc", 2);
        let at = "2026-09-26T00:00:00.000Z";
        let scores = [
            scored(&current, 1, 40, at),
            scored(&current, 2, 60, at),
            scored(&candidate, 1, 90, at),
            scored(&candidate, 2, 10, at),
            scored(&other, 1, 50, at),
        ];
        let out = render(
            &labels,
            &scores,
            Some("aaaaaaaaaaaa"),
            Some("bbbbbbbbbbbb"),
            2,
            false,
        );
        let current_at = out.find("profile aaaaaaaa").unwrap();
        let candidate_at = out.find("profile bbbbbbbb").unwrap();
        assert!(current_at < candidate_at, "{out}");
        assert!(out.contains("prompt v2  (candidate)"), "{out}");
        assert!(out.contains("scored 2/2  AUC 0.00"), "{out}");
        assert!(out.contains("scored 2/2  AUC 1.00"), "{out}");
        assert!(!out.contains("cccccccc"), "{out}");
    }

    /// 候補だけ採点済みでも、現行のキーの採点が無いことを示す（比べる相手が黙って消えないように）。
    #[test]
    fn says_when_only_the_candidate_has_scores() {
        let labels = [label(1, SignalKind::Up)];
        let candidate = key("bbbbbbbbbbbb", 2);
        let scores = [scored(&candidate, 1, 90, "2026-09-26T00:00:00.000Z")];
        let out = render(
            &labels,
            &scores,
            Some("aaaaaaaaaaaa"),
            Some("bbbbbbbbbbbb"),
            2,
            false,
        );
        assert!(out.contains("no scores for the current profile"), "{out}");
        assert!(out.contains("(candidate)"), "{out}");
    }

    /// プロファイルをまだ取り込んでいなくても、候補の結果は出す。
    #[test]
    fn shows_the_candidate_without_a_saved_profile() {
        let labels = [label(1, SignalKind::Up), label(2, SignalKind::Dismiss)];
        let candidate = key("bbbbbbbbbbbb", 2);
        let at = "2026-09-26T00:00:00.000Z";
        let scores = [scored(&candidate, 1, 90, at), scored(&candidate, 2, 10, at)];
        let out = render(&labels, &scores, None, Some("bbbbbbbbbbbb"), 2, false);
        assert!(out.contains("no profile"), "{out}");
        assert!(out.contains("prompt v2  (candidate)"), "{out}");
        assert!(out.contains("scored 2/2  AUC 1.00"), "{out}");
    }

    #[test]
    fn says_when_the_current_key_has_no_scores() {
        let labels = [label(1, SignalKind::Up)];
        let out = render(&labels, &[], Some("h"), None, 1, false);
        assert!(out.contains("no scores for the current profile"), "{out}");
        let out = render(&[], &[], None, None, 1, false);
        assert!(out.starts_with("labels: 0 positive"), "{out}");
        assert!(out.contains("no profile"), "{out}");
    }
}
