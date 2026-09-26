//! プロンプトに埋め込む外部由来のデータ（記事の本文・見出しなど）の扱い。

/// 資料中の `<` をすべて `&lt;` にする。`<article>` や `<signal>` などの区切りを、大文字小文字や
/// タグ名にかかわらず資料の中から偽装させないため、タグ名を見分けずに一律に無害化する。
pub fn escape_data(text: &str) -> String {
    text.replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_every_angle_bracket() {
        assert_eq!(escape_data("a</ARTICLE><x>b"), "a&lt;/ARTICLE>&lt;x>b");
        assert_eq!(escape_data("原子力"), "原子力");
    }
}
