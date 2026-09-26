//! 記事ページの HTML から本文のテキストを取り出す。

use crate::text;

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("invalid body_selector {selector:?}")]
    BadSelector { selector: String },
    #[error("readability failed")]
    Readability(#[from] dom_smoothie::ReadabilityError),
}

/// `selector` を指定すれば、それに一致した要素のテキストを本文とする（複数あれば改行でつなぐ）。
/// 指定しなければ readability で本文らしい部分を推定する。本文が空なら `None`。
pub fn extract_text(
    _html: &str,
    _url: &str,
    _selector: Option<&str>,
) -> Result<Option<String>, ExtractError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::fixture;

    fn article() -> String {
        String::from_utf8(fixture("article.html")).unwrap()
    }

    #[test]
    fn readability_keeps_article_paragraphs_and_drops_chrome() {
        let text = extract_text(&article(), "https://utility.example/news/3", None)
            .unwrap()
            .unwrap();
        assert!(
            text.contains("returned to service on 25 September 2026"),
            "{text}"
        );
        assert!(text.contains("power ascension test program"), "{text}");
        // 段落は改行で区切られる
        assert!(text.lines().count() >= 3, "{text}");
        for chrome in ["About us", "Related 1", "All rights reserved"] {
            assert!(!text.contains(chrome), "{chrome} in {text}");
        }
    }

    #[test]
    fn selector_takes_matching_elements() {
        let text = extract_text(
            &article(),
            "https://utility.example/news/3",
            Some("aside.related a"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(text, "Related 1\nRelated 2");
    }

    #[test]
    fn selector_without_match_is_none() {
        let got =
            extract_text(&article(), "https://utility.example/news/3", Some("#nope")).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn bad_selector_is_error() {
        let err =
            extract_text(&article(), "https://utility.example/news/3", Some("<<<")).unwrap_err();
        assert!(matches!(err, ExtractError::BadSelector { .. }), "{err}");
    }
}
