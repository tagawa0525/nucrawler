//! HTML 断片からプレーンテキストへの変換。

/// タグを除き、段落などのブロック要素は改行で区切る。script と style の中身は捨てる。
/// 行内の連続した空白は 1 つにまとめ、空行は除く。
pub fn html_to_text(html: &str) -> String {
    let fragment = scraper::Html::parse_fragment(html);
    let mut raw = String::new();
    for child in fragment.tree.root().children() {
        walk(child, &mut raw);
    }
    raw.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 改行で区切るブロック要素。
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];

fn walk(node: ego_tree::NodeRef<'_, scraper::Node>, out: &mut String) {
    match node.value() {
        // HTML ではソース中の改行も空白の一つ。行を分けるのはブロック要素だけ。
        scraper::Node::Text(t) => out.extend(
            t.chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c }),
        ),
        scraper::Node::Element(e) => {
            let name = e.name();
            if matches!(name, "script" | "style" | "noscript" | "template") {
                return;
            }
            let block = BLOCKS.contains(&name);
            if block {
                out.push('\n');
            }
            for child in node.children() {
                walk(child, out);
            }
            if block {
                out.push('\n');
            }
        }
        _ => {
            for child in node.children() {
                walk(child, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_and_splits_blocks() {
        assert_eq!(
            html_to_text("<p>Full text of the <b>event</b>.</p><p>Second</p>"),
            "Full text of the event.\nSecond"
        );
    }

    #[test]
    fn keeps_plain_text_and_collapses_whitespace() {
        assert_eq!(html_to_text("  plain \n\n  text  "), "plain text");
    }

    #[test]
    fn decodes_entities_and_line_breaks() {
        assert_eq!(html_to_text("A &amp; B<br>C&nbsp;D"), "A & B\nC D");
    }

    #[test]
    fn drops_script_and_style() {
        assert_eq!(
            html_to_text("<style>p{}</style><p>x</p><script>alert(1)</script>"),
            "x"
        );
    }

    #[test]
    fn list_items_become_lines() {
        assert_eq!(html_to_text("<ul><li>a</li><li>b</li></ul>"), "a\nb");
    }
}
