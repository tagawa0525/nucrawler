//! 訳語集：要約と和訳で訳を揃える語の一覧。DB が正本。
//! 1 つの訳語に原語を複数結び付けられる（表記の揺れや略語をまとめて同じ訳にする）。
//! 訳語集が大きくなっても system prompt が膨らまないよう、記事に原語が出てくる語だけを載せる。

/// 訳語集の 1 項目。`sources` はどれも `target`（略語があれば「訳語（略語）」）に訳す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub sources: Vec<String>,
    pub target: String,
    pub abbr: Option<String>,
    pub note: Option<String>,
}

/// 原語のどれかが `text` に出てくる語だけを残す。
pub fn relevant(_terms: Vec<Term>, _text: &str) -> Vec<Term> {
    todo!()
}

/// 要約と和訳で共有する表記と用語の決まり。
pub fn prompt_section(_terms: &[Term]) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(sources: &[&str], target: &str, abbr: Option<&str>, note: Option<&str>) -> Term {
        Term {
            sources: sources.iter().map(|s| s.to_string()).collect(),
            target: target.into(),
            abbr: abbr.map(Into::into),
            note: note.map(Into::into),
        }
    }

    #[test]
    fn lists_every_source_of_a_term_with_its_abbreviation() {
        let s = prompt_section(&[term(
            &["accident tolerant fuel", "accident-tolerant fuel", "ATF"],
            "事故耐性燃料",
            Some("ATF"),
            None,
        )]);
        assert!(
            s.contains(
                "  - accident tolerant fuel / accident-tolerant fuel / ATF → 事故耐性燃料（ATF）\n"
            ),
            "{s}"
        );
        // 原語をまとめた意味と、略語の書き方を指示する
        assert!(s.contains("「/」"), "{s}");
        assert!(s.contains("初出"), "{s}");
    }

    #[test]
    fn appends_the_note_and_omits_a_missing_abbreviation() {
        let s = prompt_section(&[term(
            &["refueling outage"],
            "燃料取替停止",
            None,
            Some("定期検査のこと"),
        )]);
        assert!(
            s.contains("  - refueling outage → 燃料取替停止 ※定期検査のこと\n"),
            "{s}"
        );
    }

    #[test]
    fn keeps_the_general_rules_without_terms() {
        let s = prompt_section(&[]);
        assert!(s.starts_with("# 表記\n"), "{s}");
        assert!(s.contains("推測で補わない"), "{s}");
        assert!(!s.contains("統一する"), "{s}");
    }

    fn sources_found(sources: &[&str], text: &str) -> bool {
        !relevant(vec![term(sources, "訳", None, None)], text).is_empty()
    }

    #[test]
    fn keeps_only_terms_whose_source_appears() {
        let terms = vec![
            term(&["scram"], "スクラム", None, None),
            term(&["spent fuel", "used fuel"], "使用済燃料", None, None),
        ];
        let kept = relevant(terms, "Used fuel is stored on site.");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target, "使用済燃料");
    }

    /// 語は大文字小文字を問わないが、大文字だけの略語は大文字のときだけ当てる。
    #[test]
    fn matches_words_in_any_case_but_abbreviations_exactly() {
        assert!(sources_found(
            &["refueling outage"],
            "The Refueling Outage began."
        ));
        assert!(sources_found(&["ATF"], "Loading ATF rods."));
        assert!(!sources_found(&["ATF"], "the atf rods"));
    }

    /// 語の途中には当てず、複数形（s / es）は当てる。
    #[test]
    fn matches_whole_words_and_plurals() {
        assert!(!sources_found(&["scram"], "They scrambled."));
        assert!(!sources_found(&["PRA"], "PRACTICE"));
        assert!(sources_found(&["SMR"], "Two SMRs were ordered."));
        assert!(sources_found(&["scram"], "Three scrams occurred."));
        assert!(sources_found(&["power uprate"], "(power uprate)"));
    }

    /// 改行などの空白の違いは無視し、日本語の原語は文字列の一致で当てる。
    #[test]
    fn ignores_whitespace_differences_and_matches_japanese() {
        assert!(sources_found(
            &["small modular reactor"],
            "small  modular\nreactor"
        ));
        assert!(sources_found(
            &["原子力規制委員会"],
            "原子力規制委員会は審査を終えた。"
        ));
    }
}
