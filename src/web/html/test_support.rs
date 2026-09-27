//! 画面のテストで共有する補助。

use super::*;
use crate::db::{ArtifactVersion, Feedback};

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
        matched: Vec::new(),
        excluded: Vec::new(),
        read: false,
        feedback: None,
        has_translation: false,
        translation_requested: false,
        bookmarked: false,
        locked_by: vec![],
    }
}

pub(super) fn detail() -> ArticleDetail {
    let digest = |id: i64, model: &str, title: &str| ArtifactVersion {
        id,
        backend: "claude-cli".into(),
        model: model.into(),
        prompt_version: 1,
        created_at: "2026-09-26T00:00:00.000Z".into(),
        payload: serde_json::json!({
            "title_ja": title, "summary_ja": "要約", "points_ja": ["要点A", "要点B"],
            "implications_ja": "示唆", "lwr_relevant": true, "topics": ["燃料"],
        }),
    };
    let mut i = item(7, "2026-09-26T00:00:00.000Z");
    i.feedback = Some(Feedback::Up);
    ArticleDetail {
        item: i,
        digests: vec![digest(11, "opus", "新版"), digest(10, "sonnet", "旧版")],
        translations: vec![],
        has_body: true,
    }
}

pub(super) fn term_report(id: i64, status: ReportStatus) -> Report {
    Report {
        id,
        article_id: 7,
        article_title: "見出し<A>".into(),
        kind: ReportKind::Term,
        found: Some("給油停止".into()),
        wanted: Some("燃料取替停止".into()),
        source: Some("refueling outage".into()),
        note: None,
        status,
        term: None,
        reply: None,
        reported_at: "2026-09-27T00:00:00.000Z".into(),
        resolved_at: None,
    }
}

pub(super) fn other_report(id: i64, kind: ReportKind) -> Report {
    Report {
        kind,
        found: None,
        wanted: None,
        source: None,
        note: Some("数値が<違う>".into()),
        ..term_report(id, ReportStatus::Pending)
    }
}

pub(super) fn entry(
    id: i64,
    sources: &[&str],
    target: &str,
    abbr: Option<&str>,
) -> crate::glossary::Entry {
    crate::glossary::Entry {
        id,
        term: crate::glossary::Term {
            sources: sources.iter().map(|s| s.to_string()).collect(),
            target: target.into(),
            abbr: abbr.map(Into::into),
            note: Some("注<記>".into()),
        },
        term_changed_at: None,
        sources_added_at: vec![None; sources.len()],
    }
}
