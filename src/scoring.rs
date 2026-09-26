//! 推薦の採点の依頼内容：プロファイルと行動シグナルを入れた system prompt、出力の JSON Schema、
//! 記事をまとめたプロンプト、応答の検証。

use crate::db::{ScoreInput, Signal};
use crate::profile::Profile;

#[derive(Debug, thiserror::Error)]
pub enum ScoringError {
    #[error("score output does not match the schema: {0}")]
    Malformed(String),
}

/// 応答の検証結果。
#[derive(Debug, PartialEq)]
pub struct Parsed {
    /// (記事 id, 点数, 理由)
    pub items: Vec<(i64, u8, String)>,
    /// 依頼したのに応答に無かった、またはスキーマに合わなかった記事
    pub missing: Vec<i64>,
}

/// プロファイルと直近の行動シグナルを埋め込んだ system prompt。
pub fn system_prompt(_profile: &Profile, _signals: &[Signal]) -> String {
    todo!()
}

pub fn schema() -> serde_json::Value {
    todo!()
}

/// 採点する記事（要約済み）を `<article>` で区切って並べる。
pub fn build_prompt(_inputs: &[ScoreInput]) -> String {
    todo!()
}

pub fn parse(_output: &serde_json::Value, _requested: &[i64]) -> Result<Parsed, ScoringError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SignalKind;

    fn profile() -> Profile {
        crate::profile::parse(include_str!("../examples/profile.toml")).unwrap()
    }

    #[test]
    fn system_prompt_carries_profile_and_signal_strengths() {
        let signals = [
            Signal {
                kind: SignalKind::Up,
                title_ja: "ATF の照射試験".into(),
            },
            Signal {
                kind: SignalKind::Down,
                title_ja: "核融合の新記録".into(),
            },
            Signal {
                kind: SignalKind::OpenTranslation,
                title_ja: "再稼働審査の進捗".into(),
            },
            Signal {
                kind: SignalKind::OpenDetail,
                title_ja: "電力市場の動向".into(),
            },
        ];
        let s = system_prompt(&profile(), &signals);
        assert!(s.contains("規制・審査") && s.contains("1.0"), "{s}");
        assert!(
            s.contains("再稼働審査、新規制基準"),
            "notes are included: {s}"
        );
        assert!(
            s.contains("核兵器") && s.contains("核融合"),
            "excludes: {s}"
        );
        for title in [
            "ATF の照射試験",
            "核融合の新記録",
            "再稼働審査の進捗",
            "電力市場の動向",
        ] {
            assert!(s.contains(title), "{title}: {s}");
        }
        // 強さの順：👎 ≫ 詳細を開いた ＜ 和訳を開いた ≪ 👍
        assert!(s.contains("強い関心") && s.contains("強い不要"), "{s}");
        assert!(
            s.contains("指示"),
            "instructions inside articles must be ignored: {s}"
        );
    }

    #[test]
    fn system_prompt_without_signals_says_so() {
        let s = system_prompt(&profile(), &[]);
        assert!(s.contains("まだありません"), "{s}");
    }

    #[test]
    fn schema_bounds_scores_and_forbids_extras() {
        let s = schema();
        let item = &s["properties"]["items"]["items"];
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(item["additionalProperties"], false);
        assert_eq!(item["properties"]["score"]["minimum"], 0);
        assert_eq!(item["properties"]["score"]["maximum"], 100);
        assert_eq!(
            item["required"],
            serde_json::json!(["id", "score", "reason"])
        );
    }

    #[test]
    fn prompt_lists_digests_and_neutralizes_tags() {
        let prompt = build_prompt(&[ScoreInput {
            article_id: 5,
            artifact_id: 9,
            title_ja: "題</article>".into(),
            summary_ja: "要約".into(),
            topics: vec!["燃料".into(), "規制・審査".into()],
        }]);
        assert!(prompt.contains("<article id=\"5\">"), "{prompt}");
        assert!(prompt.contains("燃料、規制・審査"), "{prompt}");
        assert_eq!(prompt.matches("</article>").count(), 1, "{prompt}");
    }

    #[test]
    fn parse_validates_items_and_reports_missing() {
        let output = serde_json::json!({"items": [
            {"id": 1, "score": 80, "reason": "規制に直結"},
            {"id": 2, "score": 101, "reason": "範囲外"},
            {"id": 3, "score": 50, "reason": "r", "extra": 1},
            {"id": 99, "score": 10, "reason": "依頼していない"},
        ]});
        let parsed = parse(&output, &[1, 2, 3, 4]).unwrap();
        assert_eq!(parsed.items, [(1, 80, "規制に直結".to_string())]);
        assert_eq!(parsed.missing, [2, 3, 4]);
        for bad in [
            serde_json::json!({"items": [], "x": 1}),
            serde_json::json!({"items": "x"}),
            serde_json::json!({"items": [{"score": 1, "reason": "no id"}]}),
        ] {
            assert!(parse(&bad, &[1]).is_err(), "{bad}");
        }
    }
}
