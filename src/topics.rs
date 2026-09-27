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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vocabulary {
    #[serde(rename = "topic", default)]
    topics: Vec<Topic>,
}

/// TOML を読み、値を検証する（1 件以上、名前は空でなく重複しない）。
pub fn parse(_text: &str) -> Result<Vec<Topic>, TopicsError> {
    Ok(Vec::new())
}

pub fn to_toml(_topics: &[Topic]) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic(name: &str, facet: Facet) -> Topic {
        Topic {
            name: name.into(),
            facet,
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
