//! 画面の HTML。I/O を持たない関数だけにして、テストしやすくする。JavaScript は使わない。

use crate::db::{ArticleDetail, ListItem, Warning};

/// HTML の特殊文字を実体参照にする。
pub fn escape(_s: &str) -> String {
    todo!()
}

/// 一覧を「前回見てから届いた記事」と「それより前の未読の記事」に分ける。
/// `last_seen` が無ければ（初回）、すべてを前者にする。
pub fn split_sections(
    _items: Vec<ListItem>,
    _last_seen: Option<&str>,
) -> (Vec<ListItem>, Vec<ListItem>) {
    todo!()
}

/// 全ページ共通の外枠（スマホ向けの 1 カラム、警告のバナー）。
pub fn layout(_title: &str, _warnings: &[Warning], _body: &str) -> String {
    todo!()
}

pub fn list_page(
    _new: &[ListItem],
    _earlier: &[ListItem],
    _show_all: bool,
    _warnings: &[Warning],
) -> String {
    todo!()
}

/// 詳細画面の表示の選択。
#[derive(Debug, Clone, Copy, Default)]
pub struct DetailView {
    /// 表示する digest の版（無ければ最新）
    pub digest: Option<i64>,
    /// 全文和訳を表示する（`translation` があればその版、無ければ最新）
    pub show_translation: bool,
    pub translation: Option<i64>,
}

pub fn detail_page(_d: &ArticleDetail, _view: DetailView, _warnings: &[Warning]) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ArtifactVersion, Feedback};

    fn item(id: i64, fetched_at: &str) -> ListItem {
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
            locked_by: vec![],
        }
    }

    #[test]
    fn escapes_html() {
        assert_eq!(
            escape(r#"<a href="x">&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn splits_new_and_earlier_unread() {
        let mut read = item(3, "2026-09-26T00:00:00.000Z");
        read.read = true;
        let items = vec![
            item(1, "2026-09-27T05:00:00.000Z"),
            item(2, "2026-09-26T00:00:00.000Z"),
            read,
        ];
        let (new, earlier) = split_sections(items.clone(), Some("2026-09-27T00:00:00.000Z"));
        assert_eq!(new.iter().map(|i| i.article_id).collect::<Vec<_>>(), [1]);
        // 前回より前の記事は、未読のものだけを残す
        assert_eq!(
            earlier.iter().map(|i| i.article_id).collect::<Vec<_>>(),
            [2]
        );
        let (new, earlier) = split_sections(items, None);
        assert_eq!(new.len(), 3);
        assert!(earlier.is_empty());
    }

    #[test]
    fn layout_is_mobile_friendly_and_shows_warnings() {
        let html = layout(
            "一覧",
            &[
                Warning::SourceFailing {
                    source_id: "nei".into(),
                    error: "HTTP 403".into(),
                    at: "2026-09-27T00:00:00.000Z".into(),
                },
                Warning::LlmFailed {
                    error: "llm reported an error (error): Not logged in".into(),
                    at: "2026-09-27T01:00:00.000Z".into(),
                },
            ],
            "<p>body</p>",
        );
        assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
        assert!(html.contains(r#"name="viewport""#));
        assert!(html.contains("<p>body</p>"));
        assert!(html.contains("nei") && html.contains("HTTP 403"));
        // 認証切れらしいときは、対処を案内する
        assert!(
            html.contains("claude") && html.contains("ログイン"),
            "{html}"
        );
    }

    #[test]
    fn list_page_renders_cards_with_escaped_text() {
        let mut locked = item(2, "2026-09-26T00:00:00.000Z");
        locked.locked_by = vec!["日本原子力学会".into()];
        locked.translation_requested = true;
        let mut untitled = item(3, "2026-09-26T00:00:00.000Z");
        untitled.title_ja = None;
        untitled.score = None;
        let html = list_page(
            &[item(1, "2026-09-27T05:00:00.000Z")],
            &[locked, untitled],
            false,
            &[],
        );
        assert!(html.contains("前回から"));
        assert!(html.contains(r#"href="/articles/1""#));
        assert!(html.contains("見出し1"));
        assert!(html.contains("要約&lt;b&gt;"), "summary is escaped");
        assert!(!html.contains("要約<b>"));
        assert!(html.contains("80"));
        assert!(html.contains("🔒") && html.contains("日本原子力学会"));
        assert!(html.contains("和訳待ち"));
        // digest が無ければ原題を出す
        assert!(html.contains("Title 3"));
        assert!(html.contains(r#"href="/?all=1""#), "toggle to show all");
    }

    fn detail() -> ArticleDetail {
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

    #[test]
    fn detail_page_shows_latest_digest_and_version_links() {
        let html = detail_page(&detail(), DetailView::default(), &[]);
        assert!(
            html.contains("新版") && !html.contains("<h1>旧版"),
            "{html}"
        );
        assert!(html.contains("要点A") && html.contains("示唆") && html.contains("燃料"));
        assert!(
            html.contains(r#"href="https://e.com/7""#),
            "link to original"
        );
        assert!(
            html.contains(r#"href="/articles/7?digest=10""#),
            "switch to older version"
        );
        assert!(html.contains("sonnet"));
        assert!(html.contains(r#"action="/articles/7/feedback""#));
        // 英語で本文があり和訳が無いので、依頼ボタンを出す
        assert!(
            html.contains(r#"action="/articles/7/translation-request""#),
            "{html}"
        );
    }

    #[test]
    fn detail_page_switches_digest_version() {
        let view = DetailView {
            digest: Some(10),
            ..DetailView::default()
        };
        let html = detail_page(&detail(), view, &[]);
        assert!(html.contains("旧版"));
    }

    #[test]
    fn detail_page_shows_translation_or_its_link() {
        let mut d = detail();
        d.item.has_translation = true;
        d.translations = vec![ArtifactVersion {
            id: 20,
            backend: "claude-cli".into(),
            model: "sonnet".into(),
            prompt_version: 1,
            created_at: "2026-09-26T00:00:00.000Z".into(),
            payload: serde_json::json!({"body_ja": "第一段落。\n\n第二段落<script>"}),
        }];
        let html = detail_page(&d, DetailView::default(), &[]);
        assert!(
            html.contains(r#"href="/articles/7?view=translation""#),
            "{html}"
        );
        assert!(!html.contains("第一段落"));
        let view = DetailView {
            show_translation: true,
            ..DetailView::default()
        };
        let html = detail_page(&d, view, &[]);
        assert!(html.contains("<p>第一段落。</p>"), "{html}");
        assert!(html.contains("第二段落&lt;script&gt;"));
        assert!(!html.contains(r#"translation-request"#));
    }

    #[test]
    fn detail_page_shows_waiting_when_requested() {
        let mut d = detail();
        d.item.translation_requested = true;
        let html = detail_page(&d, DetailView::default(), &[]);
        assert!(html.contains("和訳待ち"));
        assert!(!html.contains(r#"action="/articles/7/translation-request""#));
    }
}
