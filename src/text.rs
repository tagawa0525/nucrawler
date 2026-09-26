//! HTML 断片からプレーンテキストへの変換。

/// HTML のバイト列を文字列にする。文字コードは Content-Type の charset、BOM、
/// 先頭 1024 バイト内の `<meta charset>` の順に探し、見つからなければ UTF-8 とみなす。
/// 不正なバイト列は置換文字にする。
pub fn decode_html(_bytes: &[u8], _content_type: Option<&str>) -> String {
    todo!()
}

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

    fn fixture(name: &str) -> Vec<u8> {
        crate::testutil::fixture(name)
    }

    #[test]
    fn decodes_utf8_by_default() {
        assert_eq!(decode_html("日本語".as_bytes(), None), "日本語");
    }

    #[test]
    fn decodes_charset_from_content_type() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("原子力");
        assert_eq!(
            decode_html(&bytes, Some("text/html; charset=Shift_JIS")),
            "原子力"
        );
    }

    #[test]
    fn decodes_charset_from_meta_tag() {
        let html = decode_html(&fixture("article_sjis.html"), Some("text/html"));
        assert!(html.contains("女川原子力発電所2号機"), "{html}");
    }

    #[test]
    fn bom_wins_over_meta() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("<meta charset=\"shift_jis\">日本".as_bytes());
        assert!(decode_html(&bytes, None).ends_with("日本"));
    }

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
