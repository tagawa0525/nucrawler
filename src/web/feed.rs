//! 記事のフィード（Atom）。`html` と同じく I/O を持たない関数だけにする。
//!
//! RSS 2.0 ではなく Atom にするのは、記事ごとに Web UI の詳細と原文の 2 つのリンクを
//! `rel` で区別して書け、日付が DB と同じ RFC 3339 で済み、`summary` がプレーンテキストだと
//! 決まっている（RSS の `description` は HTML かどうかが曖昧）ため。

use crate::db::ListItem;
use crate::web::html::{SourceLabels, display_title};

/// XML の特殊文字を実体参照にし、XML 1.0 に書けない文字（タブ・改行・復帰以外の制御文字など）は
/// 実体参照にもできないので落とす。
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c < ' ' || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            _ => out.push(c),
        }
    }
    out
}

/// フィードとエントリの ID（tag URI、RFC 4151）の接頭辞。
///
/// ID はリーダーが既読を判定するのに使うので、待ち受けのアドレスやアクセスした名前（リンクには
/// それを使う）に依存させない。authority は作者が持つドメイン、日付はこの形式を決めた年。
const TAG: &str = "tag:tagawa0525.github.io,2026:nucrawler:";

/// エントリの ID。DB の記事 ID は DB を作り直すと別の記事に振り直され、既読の記事と取り違える
/// ので、元記事の正規化済み URL（`articles.url` は UNIQUE）から作る。元記事の URL そのものに
/// しないのは、要約は元のサイトのフィードのエントリとは別物なので、同じ ID にしないため。
/// tag URI に書けない文字はパーセントエンコードする。
fn entry_id(url: &str) -> String {
    let mut out = format!("{TAG}article:");
    for b in url.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/?%".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Atom のフィード。`base` は Web UI の URL（`http://host:port`）でリンクにだけ使い、`updated` は
/// 記事が無いときのフィードの更新日時。記事は新しい順に並べる（どの記事を出すかは Web の既定の
/// 一覧と同じ）。
pub fn atom(items: &[ListItem], base: &str, labels: &SourceLabels, updated: &str) -> String {
    let mut items: Vec<&ListItem> = items.iter().collect();
    items.sort_by(|a, b| (&b.at, b.article_id).cmp(&(&a.at, a.article_id)));
    // 時刻はどれも同じ書式（`db::timestamp`）なので、文字列の最大が最新
    let updated = items.first().map_or(updated, |i| i.at.as_str());
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <feed xmlns=\"http://www.w3.org/2005/Atom\">\
         <id>{TAG}feed</id><title>nucrawler</title><updated>{updated}</updated>\
         <link rel=\"self\" href=\"{base}/feed.xml\"/><link rel=\"alternate\" href=\"{base}/\"/>\
         <author><name>nucrawler</name></author>",
        base = escape(base),
        updated = escape(updated),
    );
    for i in items {
        let detail = escape(&format!("{base}/articles/{}", i.article_id));
        let source = labels.get(&i.source_id).unwrap_or(&i.source_id);
        out.push_str(&format!(
            "<entry><id>{id}</id><title>{title}</title><updated>{at}</updated>\
             <link rel=\"alternate\" href=\"{detail}\"/><link rel=\"related\" href=\"{url}\"/>\
             <author><name>{source}</name></author>{summary}</entry>",
            id = escape(&entry_id(&i.url)),
            title = escape(display_title(i.title_ja.as_deref(), i)),
            at = escape(&i.at),
            url = escape(&i.url),
            source = escape(source),
            summary = i
                .summary_ja
                .as_deref()
                .map_or_else(String::new, |s| format!("<summary>{}</summary>", escape(s))),
        ));
    }
    out.push_str("</feed>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: i64, url: &str) -> ListItem {
        ListItem {
            article_id: id,
            source_id: "wnn".into(),
            url: url.into(),
            title: format!("Title {id}"),
            lang: "en".into(),
            at: "2026-09-26T00:00:00.000Z".into(),
            fetched_at: "2026-09-26T00:00:00.000Z".into(),
            title_ja: None,
            summary_ja: None,
            lwr_relevant: Some(true),
            score: Some(80),
            llm_score: Some(80),
            reason: None,
            matched: Vec::new(),
            excluded: Vec::new(),
            read_at: None,
            rating: None,
            has_translation: false,
            translation_requested: false,
            bookmarked: false,
            locked_by: vec![],
            story_id: id,
            story_others: vec![],
            story_read: false,
            story_rated: false,
        }
    }

    /// ID は tag URI（RFC 4151）で、エントリは DB の ID ではなく元記事の正規化済み URL から作る。
    /// tag URI に書けない文字（IPv6 の角括弧や `|` など）はパーセントエンコードする。
    #[test]
    fn ids_are_tag_uris_from_the_article_url() {
        let items = [
            item(1, "https://e.com/a?b=1&c=2"),
            item(2, "http://[::1]/x|y"),
        ];
        let xml = atom(
            &items,
            "http://h",
            &SourceLabels::new(),
            "2026-09-27T00:00:00Z",
        );
        assert!(
            xml.contains("<id>tag:tagawa0525.github.io,2026:nucrawler:feed</id>"),
            "{xml}"
        );
        assert!(
            xml.contains(
                "<id>tag:tagawa0525.github.io,2026:nucrawler:article:https://e.com/a?b=1&amp;c=2</id>"
            ),
            "{xml}"
        );
        assert!(
            xml.contains(
                "<id>tag:tagawa0525.github.io,2026:nucrawler:article:http://%5B::1%5D/x%7Cy</id>"
            ),
            "{xml}"
        );
    }
}
