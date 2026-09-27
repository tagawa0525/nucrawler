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
    r#"あなたは原子力分野のニュースに付けるトピック（タグ）の語彙を管理しています。
要約を作るときに LLM が語彙に無い語を追加していくので、表記の揺れや意味の重なりが生まれます。
語彙を見て、同じ意味の語を 1 つにまとめる統合を挙げてください。

# 入力
- 語彙の語を 1 行ずつ、軸（分野・炉型・地域・組織）、付いている要約の数、LLM が追加した日とともに示します。
- 追加した日の無い語は、人が決めた語です。

# 出力
- merges: 統合の一覧。無ければ空の配列
  - from: まとめて消す語。LLM が追加した語（追加した日のある語）だけ
  - into: 残す語。人が決めた語があればそちらを残す。LLM が追加した語同士なら、要約の数が多い方を残す
  - reason: 同じ意味だと判断した理由（1 文）

# 規則
- 言い換え、表記の違い、語順の違い、片方がもう片方の一部を言い換えただけのもの（例：「新設炉」と「新設・建設」）を統合する
- 意味の違う語、細かく分けておく価値のある語は統合しない。迷ったら統合しない
- 軸が違う語へは統合しない
"#
}

/// 語彙を 1 語 1 行で並べたプロンプト（例「- 新設炉（分野、要約 2 件、2026-09-28 に追加）」）。
pub fn build_prompt(usage: &[TopicUsage]) -> String {
    let mut out = String::from("次の語彙から、統合すべき語を挙げてください。\n\n");
    for u in usage {
        let added = u
            .added_at
            .as_deref()
            .map(|at| format!("、{} に追加", at.get(..10).unwrap_or(at)))
            .unwrap_or_default();
        out.push_str(&format!(
            "- {}（{}、要約 {} 件{added}）\n",
            u.name,
            u.facet.as_str(),
            u.uses
        ));
    }
    out
}

/// 出力の JSON Schema。統合元は LLM が足した語、統合先は語彙のどれか。
pub fn schema(usage: &[TopicUsage]) -> serde_json::Value {
    let added: Vec<&str> = usage
        .iter()
        .filter(|u| u.added_at.is_some())
        .map(|u| u.name.as_str())
        .collect();
    let all: Vec<&str> = usage.iter().map(|u| u.name.as_str()).collect();
    serde_json::json!({
        "type": "object",
        "properties": {
            "merges": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "from": {"type": "string", "enum": added},
                        "into": {"type": "string", "enum": all},
                        "reason": {"type": "string"},
                    },
                    "required": ["from", "into", "reason"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["merges"],
        "additionalProperties": false,
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    merges: Vec<Item>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    from: String,
    into: String,
    reason: String,
}

/// 応答から統合を取り出す。規則に合わない統合は採らずに捨てる（ほかの統合は残す）。
pub fn parse(
    output: &serde_json::Value,
    usage: &[TopicUsage],
) -> Result<Vec<TopicMerge>, TidyError> {
    let Output { merges } =
        serde_json::from_value(output.clone()).map_err(|e| TidyError::Malformed(e.to_string()))?;
    let find = |name: &str| usage.iter().find(|u| u.name == name);
    let mut kept: Vec<Item> = Vec::new();
    for m in merges {
        let reason = match (find(&m.from), find(&m.into)) {
            _ if m.from == m.into => Some("merges a topic into itself"),
            (None, _) | (_, None) => Some("names a topic not in the vocabulary"),
            (Some(from), _) if from.added_at.is_none() => Some("merges away a curated topic"),
            _ if kept.iter().any(|k| k.from == m.from) => Some("merges the same topic twice"),
            _ => None,
        };
        match reason {
            Some(reason) => {
                tracing::warn!(from = %m.from, into = %m.into, "ignoring merge that {reason}")
            }
            None => kept.push(m),
        }
    }
    // 統合先がほかの統合で消えると、付け替えた付与ごと失われるので採らない
    let froms: Vec<String> = kept.iter().map(|m| m.from.clone()).collect();
    Ok(kept
        .into_iter()
        .filter(|m| {
            let chained = froms.contains(&m.into);
            if chained {
                tracing::warn!(from = %m.from, into = %m.into, "ignoring merge into a topic merged away");
            } else {
                tracing::info!(from = %m.from, into = %m.into, reason = %m.reason, "topic merge proposed");
            }
            !chained
        })
        .map(|m| TopicMerge {
            from: m.from,
            into: m.into,
        })
        .collect())
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
        type Pairs<'a> = &'a [(&'a str, &'a str)];
        let cases: &[(Pairs, Pairs)] = &[
            // 初期の語は統合元にしない
            (&[("燃料", "新設・建設")], &[]),
            // 語彙に無い
            (&[("無い語", "燃料")], &[]),
            (&[("新設炉", "無い語")], &[]),
            // 自分自身
            (&[("新設炉", "新設炉")], &[]),
            // 軸が違う
            (&[("新設炉", "PWR")], &[]),
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
