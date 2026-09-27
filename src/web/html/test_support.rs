//! 画面のテストで共有する補助。

use super::*;

pub(super) fn item(id: i64, fetched_at: &str) -> ListItem {
    ListItem {
        article_id: id,
        source_id: "wnn".into(),
        url: format!("https://e.com/{id}"),
        title: format!("Title {id}"),
        lang: "en".into(),
        at: "2026-09-26T00:00:00.000Z".into(),
        fetched_at: fetched_at.into(),
        title_ja: Some(format!("見出し{id}")),
        summary_ja: Some("要約<b>".into()),
        lwr_relevant: Some(true),
        score: Some(80),
        reason: Some("理由".into()),
        read: false,
        feedback: None,
        has_translation: false,
        translation_requested: false,
        bookmarked: false,
        locked_by: vec![],
    }
}
