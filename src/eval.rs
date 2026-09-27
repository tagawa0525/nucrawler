//! `eval`：明示的な反応を正解ラベルにして、採点のキーごとに点数が正例と負例をどれだけ分けているかを表示する。

use crate::db::{Label, LabeledScore};

/// 正例・負例のどちらかがこれより少なければ、指標は参考値と注記する
const FEW_LABELS: usize = 5;

/// 正例と負例の組のうち、正例の点数が高い割合（同点は半分と数える）。どちらかが空なら `None`。
pub fn auc(positive: &[u8], negative: &[u8]) -> Option<f64> {
    todo!("{positive:?} {negative:?} {FEW_LABELS}")
}

/// 10 点刻みの点数帯の下限。100 点は 90 帯に入れる。
pub fn band(score: u8) -> u8 {
    todo!("{score}")
}

/// 評価の表示。`current` は現行のプロファイルの hash と score のプロンプトの版で、`all` が偽なら
/// そのキーだけを出す。
pub fn render(
    labels: &[Label],
    scores: &[LabeledScore],
    current: Option<(&str, i64)>,
    all: bool,
) -> String {
    todo!("{labels:?} {scores:?} {current:?} {all}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{EvalKey, SignalKind};

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
        let out = render(&labels, &scores, Some(("0123456789abcdef", 1)), false);
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
        let all = render(&labels, &scores, Some(("0123456789abcdef", 1)), true);
        assert!(
            all.contains("profile fedcba98  claude-cli/sonnet  prompt v1\n"),
            "{all}"
        );
        assert!(all.contains("scored 1/3  AUC -"), "{all}");
    }

    #[test]
    fn says_when_the_current_key_has_no_scores() {
        let labels = [label(1, SignalKind::Up)];
        let out = render(&labels, &[], Some(("h", 1)), false);
        assert!(out.contains("no scores for the current profile"), "{out}");
        let out = render(&[], &[], None, false);
        assert!(out.starts_with("labels: 0 positive"), "{out}");
        assert!(out.contains("no profile"), "{out}");
    }
}
