//! トピックの語彙：要約に付けるタグの一覧。LLM には語彙の中から選ばせ、表記の揺れを防ぐ。
//! DB が正本で、`topics export` で書き出した TOML を編集して `topics import` で取り込む。

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum TopicsError {
    #[error("failed to parse topics")]
    Parse(#[from] toml::de::Error),
    #[error("invalid topics: {0}")]
    Invalid(String),
}

/// 語彙の軸。発電所名などの固有名は語彙に入れず、全文検索で引く。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Facet {
    #[serde(rename = "分野")]
    Field,
    #[serde(rename = "炉型")]
    Reactor,
    #[serde(rename = "地域")]
    Region,
    #[serde(rename = "組織")]
    Organization,
}

impl Facet {
    pub const ALL: [Facet; 4] = [
        Facet::Field,
        Facet::Reactor,
        Facet::Region,
        Facet::Organization,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Facet::Field => "分野",
            Facet::Reactor => "炉型",
            Facet::Region => "地域",
            Facet::Organization => "組織",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Topic {
    pub name: String,
    pub facet: Facet,
}

/// 語彙ファイルの 1 語。`added_at` は要約が提案して語彙に加えた時刻（LLM が足した語）で、
/// 週 1 回の整理で統合されうる。行から消して取り込めば、人が決めた語になり統合されなくなる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub facet: Facet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vocabulary {
    #[serde(rename = "topic", default)]
    topics: Vec<Entry>,
}

/// TOML を読み、値を検証する（1 件以上、名前は空でなく重複しない）。
pub fn parse(text: &str) -> Result<Vec<Entry>, TopicsError> {
    let Vocabulary { topics } = toml::from_str(text)?;
    if topics.is_empty() {
        return Err(TopicsError::Invalid(
            "at least one topic is required".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for t in &topics {
        if t.name.trim().is_empty() {
            return Err(TopicsError::Invalid("topic name must not be empty".into()));
        }
        if !seen.insert(t.name.as_str()) {
            return Err(TopicsError::Invalid(format!(
                "duplicate topic {:?}",
                t.name
            )));
        }
        if let Some(at) = &t.added_at
            && chrono::DateTime::parse_from_rfc3339(at).is_err()
        {
            return Err(TopicsError::Invalid(format!(
                "added_at of {:?} must be an RFC 3339 time, got {at:?}",
                t.name
            )));
        }
    }
    Ok(topics)
}

pub fn to_toml(topics: &[Entry]) -> String {
    toml::to_string(&Vocabulary {
        topics: topics.to_vec(),
    })
    .expect("topics are plain data")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic(name: &str, facet: Facet) -> Entry {
        Entry {
            name: name.into(),
            facet,
            added_at: None,
        }
    }

    #[test]
    fn parses_topics_in_file_order() {
        let text = r#"
[[topic]]
name = "規制・審査"
facet = "分野"

[[topic]]
name = "PWR"
facet = "炉型"
"#;
        assert_eq!(
            parse(text).unwrap(),
            [
                topic("規制・審査", Facet::Field),
                topic("PWR", Facet::Reactor)
            ]
        );
    }

    #[test]
    fn rejects_invalid_topics() {
        let one = "[[topic]]\nname = \"燃料\"\nfacet = \"分野\"\n";
        for (text, expected) in [
            ("", "at least one"),
            (&format!("{one}{one}")[..], "duplicate"),
            ("[[topic]]\nname = \" \"\nfacet = \"分野\"\n", "empty"),
        ] {
            let err = parse(text).unwrap_err();
            assert!(
                matches!(&err, TopicsError::Invalid(m) if m.contains(expected)),
                "{text:?}: {err}"
            );
        }
        for text in [
            "[[topic]]\nname = \"燃料\"\nfacet = \"話題\"\n",
            "[[topic]]\nname = \"燃料\"\n",
            "[[topic]]\nname = \"燃料\"\nfacet = \"分野\"\nnote = \"x\"\n",
        ] {
            assert!(
                matches!(parse(text), Err(TopicsError::Parse(_))),
                "{text:?}"
            );
        }
    }

    /// LLM が足した語は追加した時刻を持ち、書き出して取り込み直しても LLM が足した語のまま。
    #[test]
    fn parses_added_at_of_proposed_topics() {
        let text = "[[topic]]\nname = \"新設炉\"\nfacet = \"分野\"\nadded_at = \"2026-09-28T01:00:00.000Z\"\n";
        let parsed = parse(text).unwrap();
        assert_eq!(
            parsed[0].added_at.as_deref(),
            Some("2026-09-28T01:00:00.000Z")
        );
        assert_eq!(parse(&to_toml(&parsed)).unwrap(), parsed);
        let bad = "[[topic]]\nname = \"新設炉\"\nfacet = \"分野\"\nadded_at = \"yesterday\"\n";
        assert!(
            matches!(parse(bad), Err(TopicsError::Invalid(m)) if m.contains("added_at")),
            "{bad}"
        );
    }

    #[test]
    fn export_round_trips() {
        let topics = [
            topic("燃料", Facet::Field),
            topic("BWR", Facet::Reactor),
            topic("米国", Facet::Region),
            topic("IAEA", Facet::Organization),
        ];
        assert_eq!(parse(&to_toml(&topics)).unwrap(), topics);
    }
}
