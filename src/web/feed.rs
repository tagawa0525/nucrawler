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

/// Atom のフィード。`base` は Web UI の URL（`http://host:port`）で、`updated` は記事が無いときの
/// フィードの更新日時。記事は新しい順に並べる（どの記事を出すかは Web の既定の一覧と同じ）。
pub fn atom(items: &[ListItem], base: &str, labels: &SourceLabels, updated: &str) -> String {
    let mut items: Vec<&ListItem> = items.iter().collect();
    items.sort_by(|a, b| (&b.at, b.article_id).cmp(&(&a.at, a.article_id)));
    // 時刻はどれも同じ書式（`db::timestamp`）なので、文字列の最大が最新
    let updated = items.first().map_or(updated, |i| i.at.as_str());
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <feed xmlns=\"http://www.w3.org/2005/Atom\">\
         <id>{base}/feed.xml</id><title>nucrawler</title><updated>{updated}</updated>\
         <link rel=\"self\" href=\"{base}/feed.xml\"/><link rel=\"alternate\" href=\"{base}/\"/>\
         <author><name>nucrawler</name></author>",
        base = escape(base),
        updated = escape(updated),
    );
    for i in items {
        let detail = escape(&format!("{base}/articles/{}", i.article_id));
        let source = labels.get(&i.source_id).unwrap_or(&i.source_id);
        out.push_str(&format!(
            "<entry><id>{detail}</id><title>{title}</title><updated>{at}</updated>\
             <link rel=\"alternate\" href=\"{detail}\"/><link rel=\"related\" href=\"{url}\"/>\
             <author><name>{source}</name></author>{summary}</entry>",
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
