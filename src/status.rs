//! `status`：ソースごとの記事数と取得状況を表示する。

use std::fmt::Write as _;

use crate::config::Source;
use crate::db::SourceOverview;

/// 設定にあるソースを設定順に表示し、DB にだけ残っている（設定から消した）ソースを後ろに並べる。
pub fn render(_sources: &[Source], _overview: &[SourceOverview]) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Category, Filter, Lang, SourceKind};

    fn src(id: &str, enabled: bool) -> Source {
        Source {
            id: id.into(),
            name: id.into(),
            kind: SourceKind::Feed,
            url: "https://e/".into(),
            lang: Lang::En,
            category: Category::Industry,
            enabled,
            filter: Filter::default(),
        }
    }

    fn ov(id: &str, articles: i64, ok: Option<&str>, err: Option<(&str, &str)>) -> SourceOverview {
        SourceOverview {
            source_id: id.into(),
            articles,
            last_success_at: ok.map(Into::into),
            last_error: err.map(|e| e.0.into()),
            last_error_at: err.map(|e| e.1.into()),
        }
    }

    #[test]
    fn renders_sources_in_config_order_with_state() {
        let sources = [src("wnn", true), src("nei", false), src("new", true)];
        let overview = [
            ov("nei", 0, None, Some(("HTTP 403", "2026-09-26T00:00:00Z"))),
            ov("wnn", 48, Some("2026-09-26T01:00:00.000Z"), None),
            ov("gone", 7, Some("2026-09-01T00:00:00Z"), None),
        ];
        let out = render(&sources, &overview);
        let lines: Vec<_> = out.lines().collect();
        let pos = |id: &str| lines.iter().position(|l| l.contains(id)).unwrap();
        assert!(pos("wnn") < pos("nei") && pos("nei") < pos("new") && pos("new") < pos("gone"));

        let wnn = lines[pos("wnn")];
        assert!(
            wnn.contains("48") && wnn.contains("2026-09-26 10:00"),
            "{wnn}"
        );
        let nei = lines[pos("nei")];
        assert!(
            nei.contains("disabled") && nei.contains("HTTP 403"),
            "{nei}"
        );
        let new = lines[pos("new")];
        assert!(new.contains("never"), "{new}");
        let gone = lines[pos("gone")];
        assert!(gone.contains("not in config"), "{gone}");
    }
}
