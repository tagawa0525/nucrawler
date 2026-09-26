//! robots.txt（RFC 9309）の解釈。必要な範囲だけを実装する：
//! user-agent のグループ選択、allow/disallow の最長一致（同じ長さなら allow）、`*` と `$`。

/// 1 つのホストの robots.txt から、自分の UA に適用される規則。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    rules: Vec<Rule>,
}

#[derive(Default)]
struct Group {
    agents: Vec<String>,
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
    pub fn parse(txt: &str, product: &str) -> Self {
        let mut groups: Vec<Group> = Vec::new();
        // 直前の行が user-agent なら、続く user-agent は同じグループに加える。
        let mut in_agent_lines = false;
        for line in txt.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if !in_agent_lines {
                        groups.push(Group::default());
                    }
                    in_agent_lines = true;
                    if let Some(g) = groups.last_mut() {
                        g.agents.push(value.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    in_agent_lines = false;
                    // 最初の user-agent より前の規則と、空の値（何も指定しない）は無視する。
                    if let Some(g) = groups.last_mut()
                        && !value.is_empty()
                    {
                        g.rules.push(Rule {
                            allow: key == "allow",
                            pattern: normalize(value),
                        });
                    }
                }
                _ => in_agent_lines = false,
            }
        }
        let product = product.to_ascii_lowercase();
        let pick = |agent: &str| -> Vec<Rule> {
            groups
                .iter()
                .filter(|g| g.agents.iter().any(|a| a == agent))
                .flat_map(|g| g.rules.iter().cloned())
                .collect()
        };
        let specific = pick(&product);
        let rules = if groups.iter().any(|g| g.agents.contains(&product)) {
            specific
        } else {
            pick("*")
        };
        Self { rules }
    }

    /// `path` はパスとクエリ（例 "/news/1?x=2"）。
    pub fn allows(&self, path: &str) -> bool {
        let path = normalize(path);
        self.rules
            .iter()
            .filter(|r| matches(&r.pattern, &path))
            // 最も長く一致した規則を採る。同じ長さなら allow を優先する。
            .max_by_key(|r| (r.pattern.len(), r.allow))
            .is_none_or(|r| r.allow)
    }
}

/// パーセントエンコードの 16 進数字を大文字に揃える（%e6 と %E6 を同じとみなす）。
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '%' {
            for h in chars.by_ref().take(2) {
                out.push(h.to_ascii_uppercase());
            }
        }
    }
    out
}

/// `*` は任意の文字列、末尾の `$` はパスの終わりに一致する。それ以外は前方一致。
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pattern.split('*').collect();
    let Some(rest) = path.strip_prefix(parts[0]) else {
        return false;
    };
    let mut rest = rest;
    let Some((last, middle)) = parts[1..].split_last() else {
        return !anchored || rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    if anchored {
        rest.ends_with(last)
    } else {
        rest.contains(last)
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
