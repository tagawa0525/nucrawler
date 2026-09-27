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

/// DB にある訳語集の 1 項目と、変えた時刻。`term_changed_at` は訳・略語・メモを変えた時刻、
/// `sources_added_at` は `term.sources` と同じ並びで各原語を加えた時刻（初期値の語はどれも None）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub term: Term,
    pub term_changed_at: Option<String>,
    pub sources_added_at: Vec<Option<String>>,
}

impl Entry {
    /// 訳語か原語を最後に変えた時刻（初期値のままなら None）。
    pub fn changed_at(&self) -> Option<&str> {
        self.sources_added_at
            .iter()
            .flatten()
            .map(String::as_str)
            .chain(self.term_changed_at.as_deref())
            .max()
    }
}

/// 記事に当たった訳語と、その時点。`glossary_at` は当たった訳語のうち最も新しく変えた時刻で、
/// 訳・略語・メモを変えた時刻と、記事に出てきた原語を加えた時刻から決める（無ければ None）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relevant {
    pub terms: Vec<Term>,
    pub glossary_at: Option<String>,
}

/// 原語のどれかが `text` に出てくる語だけを残し、その時点を求める。
pub fn relevant(entries: &[Entry], text: &str) -> Relevant {
    let text = normalize_spaces(text);
    let folded = text.to_ascii_lowercase();
    let mut terms = Vec::new();
    let mut glossary_at: Option<&str> = None;
    for entry in entries {
        let matched: Vec<Option<&str>> = entry
            .term
            .sources
            .iter()
            .zip(&entry.sources_added_at)
            .filter(|(source, _)| mentions(&text, &folded, source))
            .map(|(_, added_at)| added_at.as_deref())
            .collect();
        if matched.is_empty() {
            continue;
        }
        // 記事に出てこない原語を加えた時刻は、この記事の時点に含めない
        let at = matched
            .into_iter()
            .flatten()
            .chain(entry.term_changed_at.as_deref())
            .max();
        glossary_at = glossary_at.max(at);
        terms.push(entry.term.clone());
    }
    Relevant {
        terms,
        glossary_at: glossary_at.map(str::to_string),
    }
}

/// 改行や連続した空白を 1 つの空白にする（原文の折り返しで語が分かれても当てるため）。
fn normalize_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `source` が語として出てくるか。大文字だけの略語は大文字のときだけ、ほかは大文字小文字を問わず当てる。
/// 語の途中には当てず、複数形（s / es）は当てる。`folded` は `text` を ASCII で小文字にしたもの。
fn mentions(text: &str, folded: &str, source: &str) -> bool {
    let source = normalize_spaces(source);
    let abbreviation = source.chars().any(|c| c.is_ascii_uppercase())
        && !source.chars().any(|c| c.is_ascii_lowercase());
    let (haystack, needle) = if abbreviation {
        (text, source)
    } else {
        (folded, source.to_ascii_lowercase())
    };
    let is_word = |c: char| c.is_ascii_alphanumeric();
    haystack.match_indices(&needle).any(|(at, _)| {
        let rest = &haystack[at + needle.len()..];
        let rest = ["es", "s"]
            .iter()
            .find_map(|plural| rest.strip_prefix(plural))
            .unwrap_or(rest);
        !haystack[..at].chars().next_back().is_some_and(is_word)
            && !rest.chars().next().is_some_and(is_word)
    })
}

/// 要約と和訳で共有する表記と用語の決まり。
pub fn prompt_section(terms: &[Term]) -> String {
    let mut out = String::from(
        "# 表記\n- 数値・日付・固有名詞は原文のとおりに書き、記事に無いことは推測で補わない。\n",
    );
    if terms.is_empty() {
        return out;
    }
    out.push_str(
        "- 用語は次の訳に統一する。「/」で区切った原語はどれも同じ訳にする。\
         訳に略語が付いている語は、初出を「訳語（略語）」と書き、以降は略語だけでもよい：\n",
    );
    for term in terms {
        out.push_str(&format!(
            "  - {} → {}",
            term.sources.join(" / "),
            term.target
        ));
        if let Some(abbr) = &term.abbr {
            out.push_str(&format!("（{abbr}）"));
        }
        if let Some(note) = &term.note {
            out.push_str(&format!(" ※{note}"));
        }
        out.push('\n');
    }
    out
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

    /// 変えた時刻を持たない（初期値の）項目。
    fn entry(term: Term) -> Entry {
        let sources_added_at = vec![None; term.sources.len()];
        Entry {
            id: 1,
            term,
            term_changed_at: None,
            sources_added_at,
        }
    }

    fn sources_found(sources: &[&str], text: &str) -> bool {
        !relevant(&[entry(term(sources, "訳", None, None))], text)
            .terms
            .is_empty()
    }

    #[test]
    fn keeps_only_terms_whose_source_appears() {
        let entries = [
            entry(term(&["scram"], "スクラム", None, None)),
            entry(term(&["spent fuel", "used fuel"], "使用済燃料", None, None)),
        ];
        let kept = relevant(&entries, "Used fuel is stored on site.");
        assert_eq!(kept.terms.len(), 1);
        assert_eq!(kept.terms[0].target, "使用済燃料");
        assert_eq!(kept.glossary_at, None);
    }

    /// 時点は当たった訳語の変えた時刻と、記事に出てきた原語を加えた時刻のうち最も新しいもの。
    /// 記事に出てこない原語を加えても、その記事の時点は変わらない。
    #[test]
    fn glossary_at_is_the_latest_change_among_what_matched() {
        let mut fuel = entry(term(&["spent fuel", "used fuel"], "使用済燃料", None, None));
        fuel.sources_added_at = vec![None, Some("2026-09-27T02:00:00.000Z".into())];
        let mut scram = entry(term(&["scram"], "スクラム", None, None));
        scram.term_changed_at = Some("2026-09-27T01:00:00.000Z".into());
        let entries = [fuel, scram];
        let at = |text| relevant(&entries, text).glossary_at;
        assert_eq!(at("spent fuel"), None);
        assert_eq!(at("used fuel").as_deref(), Some("2026-09-27T02:00:00.000Z"));
        assert_eq!(
            at("spent fuel and a scram").as_deref(),
            Some("2026-09-27T01:00:00.000Z")
        );
        assert_eq!(
            at("used fuel and a scram").as_deref(),
            Some("2026-09-27T02:00:00.000Z")
        );
        assert_eq!(at("nothing"), None);
        assert_eq!(entries[0].changed_at(), Some("2026-09-27T02:00:00.000Z"));
        assert_eq!(entries[1].changed_at(), Some("2026-09-27T01:00:00.000Z"));
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
