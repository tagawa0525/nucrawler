//! 語彙の整理（tidy）の依頼内容：system prompt、語彙を並べたプロンプト、出力の JSON Schema、
//! 応答の検証。要約で LLM が提案して増えた語の表記揺れを、既存の語へ統合させる。
//! LLM の呼び出しやステージの進行はここでは扱わない。

use crate::db::{TopicMerge, TopicUsage};

#[derive(Debug, thiserror::Error)]
pub enum TidyError {
    #[error("tidy output does not match the schema: {0}")]
    Malformed(String),
}

pub fn system_prompt() -> &'static str {
    ""
}

/// 語彙を 1 語 1 行で並べたプロンプト。
pub fn build_prompt(_usage: &[TopicUsage]) -> String {
    String::new()
}

/// 出力の JSON Schema。
pub fn schema(_usage: &[TopicUsage]) -> serde_json::Value {
    serde_json::json!({})
}

/// 応答から統合を取り出す。規則に合わない統合は採らずに捨てる（ほかの統合は残す）。
pub fn parse(
    _output: &serde_json::Value,
    _usage: &[TopicUsage],
) -> Result<Vec<TopicMerge>, TidyError> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topics::Facet;

    fn usage() -> Vec<TopicUsage> {
        [
            ("新設・建設", Facet::Field, None, 12),
            ("燃料", Facet::Field, None, 30),
            ("PWR", Facet::Reactor, None, 8),
            ("新設炉", Facet::Field, Some("2026-09-28T01:00:00.000Z"), 2),
            (
                "新規建設",
                Facet::Field,
                Some("2026-09-30T01:00:00.000Z"),
                1,
            ),
            (
                "データセンター需要",
                Facet::Field,
                Some("2026-10-01T01:00:00.000Z"),
                3,
            ),
        ]
        .into_iter()
        .map(|(name, facet, added_at, uses)| TopicUsage {
            name: name.into(),
            facet,
            added_at: added_at.map(String::from),
            uses,
        })
        .collect()
    }

    fn merge(from: &str, into: &str) -> TopicMerge {
        TopicMerge {
            from: from.into(),
            into: into.into(),
        }
    }

    fn output(merges: &[(&str, &str)]) -> serde_json::Value {
        serde_json::json!({"merges": merges
            .iter()
            .map(|(from, into)| serde_json::json!({"from": from, "into": into, "reason": "同じ意味"}))
            .collect::<Vec<_>>()})
    }

    #[test]
    fn system_prompt_explains_the_rules() {
        let s = system_prompt();
        for word in ["統合", "from", "into", "追加"] {
            assert!(s.contains(word), "{word}: {s}");
        }
    }

    /// 語ごとに軸・使われている要約の数・LLM が足した日を示し、初期の語と見分けられるようにする。
    #[test]
    fn prompt_lists_topics_with_usage_and_origin() {
        let p = build_prompt(&usage());
        assert!(p.contains("- 燃料（分野、要約 30 件）"), "{p}");
        assert!(
            p.contains("- 新設炉（分野、要約 2 件、2026-09-28 に追加）"),
            "{p}"
        );
        assert!(p.contains("- PWR（炉型、要約 8 件）"), "{p}");
    }

    /// 統合元は LLM が足した語だけ、統合先は語彙のどれでもよい。
    #[test]
    fn schema_limits_from_to_added_topics() {
        let s = schema(&usage());
        let item = &s["properties"]["merges"]["items"];
        assert_eq!(
            item["properties"]["from"]["enum"],
            serde_json::json!(["新設炉", "新規建設", "データセンター需要"])
        );
        assert_eq!(
            item["properties"]["into"]["enum"],
            serde_json::json!([
                "新設・建設",
                "燃料",
                "PWR",
                "新設炉",
                "新規建設",
                "データセンター需要"
            ])
        );
        assert_eq!(
            item["required"],
            serde_json::json!(["from", "into", "reason"])
        );
        assert_eq!(item["additionalProperties"], false);
        assert_eq!(s["required"], serde_json::json!(["merges"]));
        assert_eq!(s["additionalProperties"], false);
    }

    #[test]
    fn parse_returns_valid_merges() {
        let merges = parse(
            &output(&[("新設炉", "新設・建設"), ("新規建設", "新設・建設")]),
            &usage(),
        )
        .unwrap();
        assert_eq!(
            merges,
            [
                merge("新設炉", "新設・建設"),
                merge("新規建設", "新設・建設")
            ]
        );
        assert!(parse(&output(&[]), &usage()).unwrap().is_empty());
    }

    #[test]
    fn parse_drops_merges_breaking_the_rules() {
        let cases: &[(&[(&str, &str)], &[(&str, &str)])] = &[
            // 初期の語は統合元にしない
            (&[("燃料", "新設・建設")], &[]),
            // 語彙に無い
            (&[("無い語", "燃料")], &[]),
            (&[("新設炉", "無い語")], &[]),
            // 自分自身
            (&[("新設炉", "新設炉")], &[]),
            // 同じ語を二度統合するなら最初の 1 つだけ
            (
                &[("新設炉", "新設・建設"), ("新設炉", "燃料")],
                &[("新設炉", "新設・建設")],
            ),
            // 統合先がほかの統合で消えるもの（連鎖）は採らない
            (
                &[("新設炉", "新規建設"), ("新規建設", "新設・建設")],
                &[("新規建設", "新設・建設")],
            ),
        ];
        for (given, expected) in cases {
            let kept = parse(&output(given), &usage()).unwrap();
            let expected: Vec<TopicMerge> = expected.iter().map(|(f, i)| merge(f, i)).collect();
            assert_eq!(kept, expected, "{given:?}");
        }
    }

    #[test]
    fn parse_rejects_malformed_output() {
        for bad in [
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"merges": "x"}),
            serde_json::json!({"merges": [], "extra": 1}),
            serde_json::json!({"merges": [{"from": "新設炉"}]}),
        ] {
            assert!(parse(&bad, &usage()).is_err(), "{bad}");
        }
    }
}
