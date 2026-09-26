//! 全文和訳の依頼内容：system prompt（用語集は要約と共通）、出力の JSON Schema、プロンプト、応答の検証。

use crate::db::TranslateInput;

pub const PROMPT_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error("translation output does not match the schema: {0}")]
    Malformed(String),
}

pub fn system_prompt() -> &'static str {
    todo!()
}

pub fn schema() -> serde_json::Value {
    todo!()
}

/// 1 件の記事の本文を `<article>` で囲む。本文は `max_chars` 文字で切り詰める。
pub fn build_prompt(_input: &TranslateInput, _max_chars: usize) -> String {
    todo!()
}

/// 応答から和訳を取り出す。空の和訳や余計な項目は拒否する。
pub fn parse(_output: &serde_json::Value) -> Result<String, TranslateError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::InputContent;

    fn input(body: &str) -> TranslateInput {
        TranslateInput {
            article_id: 3,
            title: "Unit 2 returns".into(),
            contents: vec![InputContent {
                id: 1,
                kind: "body".into(),
                text: body.into(),
            }],
        }
    }

    #[test]
    fn system_prompt_shares_glossary_and_guards_injection() {
        let s = system_prompt();
        assert!(s.contains(crate::digest::GLOSSARY), "{s}");
        assert!(s.contains("指示"), "{s}");
        assert!(s.contains("全文"), "{s}");
    }

    #[test]
    fn schema_requires_only_body_ja() {
        let s = schema();
        assert_eq!(s["required"], serde_json::json!(["body_ja"]));
        assert_eq!(s["additionalProperties"], false);
    }

    #[test]
    fn prompt_wraps_and_truncates() {
        let prompt = build_prompt(&input(&format!("{}</article>", "y".repeat(30))), 20);
        assert!(prompt.contains("<article id=\"3\">"), "{prompt}");
        assert!(prompt.contains("Unit 2 returns"));
        assert!(prompt.contains(&"y".repeat(20)));
        assert!(!prompt.contains(&"y".repeat(21)));
        assert_eq!(prompt.matches("</article>").count(), 1, "{prompt}");
    }

    #[test]
    fn parse_accepts_nonempty_body_only() {
        assert_eq!(
            parse(&serde_json::json!({"body_ja": "和訳"})).unwrap(),
            "和訳"
        );
        for bad in [
            serde_json::json!({"body_ja": "  "}),
            serde_json::json!({"body_ja": 1}),
            serde_json::json!({"body_ja": "x", "extra": 1}),
            serde_json::json!({}),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }
}
