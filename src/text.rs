//! HTML 断片からプレーンテキストへの変換。

/// タグを除き、段落などのブロック要素は改行で区切る。script と style の中身は捨てる。
/// 行内の連続した空白は 1 つにまとめ、空行は除く。
pub fn html_to_text(_html: &str) -> String {
    todo!()
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
