//! robots.txt（RFC 9309）の解釈。必要な範囲だけを実装する：
//! user-agent のグループ選択、allow/disallow の最長一致（同じ長さなら allow）、`*` と `$`。

/// 1 つのホストの robots.txt から、自分の UA に適用される規則。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    allow: bool,
    pattern: String,
}

impl Rules {
    /// すべて許可（robots.txt が 4xx のとき）。
    pub fn allow_all() -> Self {
        Self { rules: Vec::new() }
    }

    /// すべて拒否（robots.txt が 5xx や通信エラーのとき）。
    pub fn disallow_all() -> Self {
        Self {
            rules: vec![Rule {
                allow: false,
                pattern: "/".into(),
            }],
        }
    }

    /// `product` は UA の製品名（例 "nucrawler"）。大文字小文字は区別しない。
    pub fn parse(_txt: &str, _product: &str) -> Self {
        todo!()
    }

    /// `path` はパスとクエリ（例 "/news/1?x=2"）。
    pub fn allows(&self, _path: &str) -> bool {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_allows_everything() {
        assert!(Rules::parse("", "nucrawler").allows("/any"));
    }

    #[test]
    fn star_group_applies_when_no_specific_group() {
        let r = Rules::parse("User-agent: *\nDisallow: /private/\n", "nucrawler");
        assert!(!r.allows("/private/a"));
        assert!(r.allows("/public/a"));
    }

    #[test]
    fn specific_group_overrides_star_group() {
        let txt = "User-agent: *\nDisallow: /\n\nUser-agent: NuCrawler\nDisallow: /tmp/\n";
        let r = Rules::parse(txt, "nucrawler");
        assert!(r.allows("/news/1"));
        assert!(!r.allows("/tmp/x"));
    }

    #[test]
    fn groups_for_same_agent_are_merged_and_multiple_agents_share_rules() {
        let txt = "User-agent: other\nUser-agent: nucrawler\nDisallow: /a\n\
                   User-agent: nucrawler\nDisallow: /b\n";
        let r = Rules::parse(txt, "nucrawler");
        assert!(!r.allows("/a") && !r.allows("/b"));
        assert!(r.allows("/c"));
    }

    #[test]
    fn longest_match_wins_and_allow_wins_ties() {
        let txt =
            "User-agent: *\nDisallow: /news/\nAllow: /news/public/\nAllow: /x\nDisallow: /x\n";
        let r = Rules::parse(txt, "nucrawler");
        assert!(!r.allows("/news/secret"));
        assert!(r.allows("/news/public/1"));
        assert!(r.allows("/x"));
    }

    #[test]
    fn wildcards_and_end_anchor() {
        let txt = "User-agent: *\nDisallow: /*.pdf$\nDisallow: /search*q=\n";
        let r = Rules::parse(txt, "nucrawler");
        assert!(!r.allows("/docs/a.pdf"));
        assert!(r.allows("/docs/a.pdf?download=1"));
        assert!(!r.allows("/search?lang=ja&q=x"));
        assert!(r.allows("/search?lang=ja"));
    }

    #[test]
    fn empty_disallow_allows_and_comments_are_ignored() {
        let txt = "# comment\nUser-agent: * # all\nDisallow:\n";
        assert!(Rules::parse(txt, "nucrawler").allows("/a"));
    }

    #[test]
    fn rules_before_any_user_agent_are_ignored() {
        let r = Rules::parse("Disallow: /\nUser-agent: *\nAllow: /\n", "nucrawler");
        assert!(r.allows("/a"));
    }

    #[test]
    fn percent_encoding_is_compared_case_insensitively() {
        let r = Rules::parse("User-agent: *\nDisallow: /%E6%97%A5\n", "nucrawler");
        assert!(!r.allows("/%e6%97%a5/x"));
    }

    #[test]
    fn allow_all_and_disallow_all() {
        assert!(Rules::allow_all().allows("/a"));
        assert!(!Rules::disallow_all().allows("/a"));
    }
}
