//! HTML 断片からプレーンテキストへの変換。

/// HTML のバイト列を文字列にする。文字コードは BOM、Content-Type の charset、
/// 先頭 1024 バイト内の `<meta>` の宣言の順に決め、どれも使えなければ UTF-8 とみなす。
/// 解釈できないラベルは無いものとして次の候補を見る。不正なバイト列は置換文字にする。
pub fn decode_html(bytes: &[u8], content_type: Option<&str>) -> String {
    let label_encoding = |label: String| encoding_rs::Encoding::for_label(label.trim().as_bytes());
    let encoding = encoding_rs::Encoding::for_bom(bytes)
        .map(|(bom, _)| bom)
        .or_else(|| {
            content_type
                .and_then(charset_param)
                .and_then(label_encoding)
        })
        .or_else(|| meta_charset(&bytes[..bytes.len().min(1024)]))
        .unwrap_or(encoding_rs::UTF_8);
    // decode は BOM があればそれに従い、BOM 自体は取り除く。
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// "text/html; charset=Shift_JIS" から "Shift_JIS" を取り出す。
fn charset_param(content_type: &str) -> Option<String> {
    content_type.split(';').find_map(|param| {
        let (k, v) = param.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches(['"', '\'']).to_string())
    })
}

/// 先頭部分を HTML として解析し、`<meta charset>` か
/// `<meta http-equiv="Content-Type" content="...; charset=...">` の宣言を読む。
/// 解釈できないラベルの宣言は飛ばして、後ろの宣言を見る。
/// コメントや script の中の文字列は要素ではないので拾わない。
/// 文字コードが分かる前なので、ASCII 互換とみなして UTF-8（不正バイトは置換）で読む。
fn meta_charset(head: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let doc = scraper::Html::parse_document(&String::from_utf8_lossy(head));
    let meta = scraper::Selector::parse("meta").expect("valid selector");
    doc.select(&meta).find_map(|el| {
        let attr = |name| el.value().attr(name);
        let label = attr("charset").map(String::from).or_else(|| {
            attr("http-equiv")
                .filter(|v| v.eq_ignore_ascii_case("content-type"))
                .and(attr("content"))
                .and_then(charset_param)
        })?;
        encoding_rs::Encoding::for_label(label.trim().as_bytes())
    })
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

    fn sjis(html: &str) -> Vec<u8> {
        encoding_rs::SHIFT_JIS.encode(html).0.into_owned()
    }

    #[test]
    fn unknown_header_charset_falls_back_to_meta() {
        let bytes = sjis("<meta charset=\"Shift_JIS\"><p>原子力</p>");
        let html = decode_html(&bytes, Some("text/html; charset=x-unknown"));
        assert!(html.contains("原子力"), "{html}");
    }

    #[test]
    fn ignores_charset_text_outside_meta_tags() {
        let bytes = sjis(
            "<!-- charset=utf-8 --><script>var s = \"charset=utf-8\";</script>\
             <meta charset=\"shift_jis\"><p>原子力</p>",
        );
        let html = decode_html(&bytes, None);
        assert!(html.contains("原子力"), "{html}");
    }

    #[test]
    fn meta_charset_allows_spaces_around_equals() {
        let bytes = sjis("<meta charset = \"shift_jis\"><p>原子力</p>");
        assert!(decode_html(&bytes, None).contains("原子力"));
    }

    #[test]
    fn skips_unknown_meta_charset_for_a_later_valid_one() {
        let bytes = sjis("<meta charset=\"x-unknown\"><meta charset=\"shift_jis\"><p>原子力</p>");
        assert!(decode_html(&bytes, None).contains("原子力"));
    }

    /// 不正なバイト列があっても失敗せず、置換文字にして残りを読む。
    #[test]
    fn malformed_bytes_become_replacement_chars() {
        let mut bytes = b"<p>ok ".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE, 0x80]);
        bytes.extend_from_slice(" end</p>".as_bytes());
        let html = decode_html(&bytes, Some("text/html; charset=utf-8"));
        assert!(html.contains("ok \u{FFFD}"), "{html:?}");
        assert!(html.ends_with("end</p>"), "{html:?}");
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
