//! JSON API の応答の形。`html` と同じく I/O を持たない。
//! DB の型をそのまま出さず、ここで公開するフィールドを決める。

use serde::Serialize;
use serde_json::Value;

use crate::config::SourceLabels;
use crate::db::{ArticleDetail, ArtifactVersion, ListItem, Marks, Rating};

/// 記事の一覧（`GET /api/articles`）。
#[derive(Debug, Serialize)]
pub struct ArticleList<'a> {
    pub articles: Vec<Article<'a>>,
}

impl<'a> ArticleList<'a> {
    pub fn new(items: &'a [ListItem], labels: &'a SourceLabels) -> Self {
        Self {
            articles: items.iter().map(|i| Article::new(i, labels)).collect(),
        }
    }
}

/// 記事の印（`GET /api/marks`）。一覧に戻ったときに、カードの印を今の状態に合わせるために使う。
#[derive(Debug, Serialize)]
pub struct MarkList {
    pub marks: Vec<Mark>,
}

#[derive(Debug, Serialize)]
pub struct Mark {
    pub id: i64,
    /// 評価（1〜5）。評価なしは null
    pub rating: Option<Rating>,
    pub bookmarked: bool,
    pub read: bool,
}

impl MarkList {
    pub fn new(marks: Vec<Marks>) -> Self {
        Self {
            marks: marks
                .into_iter()
                .map(|m| Mark {
                    id: m.article_id,
                    rating: m.rating,
                    bookmarked: m.bookmarked,
                    read: m.read,
                })
                .collect(),
        }
    }
}

/// 一覧の 1 件。digest の項目は利用者が閲覧できる最新の版のもの。
#[derive(Debug, Serialize)]
pub struct Article<'a> {
    pub id: i64,
    pub source_id: &'a str,
    /// ソースの表示名
    pub source: &'a str,
    pub url: &'a str,
    pub title: &'a str,
    pub lang: &'a str,
    /// 公開日時（無ければ取得日時）。RFC 3339 の UTC
    pub at: &'a str,
    pub fetched_at: &'a str,
    pub title_ja: Option<&'a str>,
    pub summary_ja: Option<&'a str>,
    pub lwr_relevant: Option<bool>,
    /// 推薦点（LLM の点数に、評価から学んだ補正を足した点数）
    pub score: Option<u8>,
    /// 補正の前の LLM の点数
    #[serde(rename = "llm_score")]
    pub base_score: Option<u8>,
    pub reason: Option<&'a str>,
    /// 点数が当たった関心分野（プロファイルの語）
    pub matched: &'a [String],
    /// 点数が当たった推薦しない話題（プロファイルの語）
    pub excluded: &'a [String],
    pub read: bool,
    /// 評価（1〜5）。評価なしは null
    pub rating: Option<Rating>,
    pub has_translation: bool,
    pub translation_requested: bool,
    /// 原文を読むのに必要で、利用者が持っていない会員資格の名前
    pub locked_by: &'a [String],
}

impl<'a> Article<'a> {
    fn new(i: &'a ListItem, labels: &'a SourceLabels) -> Self {
        Self {
            id: i.article_id,
            source_id: &i.source_id,
            source: labels.get(&i.source_id).unwrap_or(&i.source_id),
            url: &i.url,
            title: &i.title,
            lang: &i.lang,
            at: &i.at,
            fetched_at: &i.fetched_at,
            title_ja: i.title_ja.as_deref(),
            summary_ja: i.summary_ja.as_deref(),
            lwr_relevant: i.lwr_relevant,
            score: i.score,
            base_score: i.base_score,
            reason: i.reason.as_deref(),
            matched: &i.matched,
            excluded: &i.excluded,
            read: i.is_read(),
            rating: i.rating,
            has_translation: i.has_translation,
            translation_requested: i.translation_requested,
            locked_by: &i.locked_by,
        }
    }
}

/// 記事の詳細（`GET /api/articles/{id}`）。要約と和訳は、利用者が閲覧できる最新の版。
#[derive(Debug, Serialize)]
pub struct ArticleBody<'a> {
    #[serde(flatten)]
    pub article: Article<'a>,
    pub digest: Option<Digest<'a>>,
    pub translation: Option<Translation<'a>>,
}

impl<'a> ArticleBody<'a> {
    pub fn new(d: &'a ArticleDetail, labels: &'a SourceLabels) -> Self {
        Self {
            article: Article::new(&d.item, labels),
            digest: d.digests.first().map(Digest::new),
            translation: d.translations.first().map(Translation::new),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Digest<'a> {
    pub id: i64,
    pub model: &'a str,
    pub created_at: &'a str,
    pub title_ja: &'a Value,
    pub summary_ja: &'a Value,
    pub points_ja: &'a Value,
    pub implications_ja: &'a Value,
    pub topics: &'a Value,
    pub lwr_relevant: &'a Value,
}

impl<'a> Digest<'a> {
    fn new(v: &'a ArtifactVersion) -> Self {
        Self {
            id: v.id,
            model: &v.model,
            created_at: &v.created_at,
            title_ja: &v.payload["title_ja"],
            summary_ja: &v.payload["summary_ja"],
            points_ja: &v.payload["points_ja"],
            implications_ja: &v.payload["implications_ja"],
            topics: &v.payload["topics"],
            lwr_relevant: &v.payload["lwr_relevant"],
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Translation<'a> {
    pub id: i64,
    pub model: &'a str,
    pub created_at: &'a str,
    pub body_ja: &'a Value,
}

impl<'a> Translation<'a> {
    fn new(v: &'a ArtifactVersion) -> Self {
        Self {
            id: v.id,
            model: &v.model,
            created_at: &v.created_at,
            body_ja: &v.payload["body_ja"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_carries_the_terms_the_score_matched() {
        let item = ListItem {
            article_id: 1,
            source_id: "wnn".into(),
            url: "https://e.com/1".into(),
            title: "t".into(),
            lang: "en".into(),
            at: "2026-09-26T00:00:00.000Z".into(),
            fetched_at: "2026-09-26T00:00:00.000Z".into(),
            title_ja: None,
            summary_ja: None,
            lwr_relevant: Some(true),
            score: Some(80),
            base_score: Some(80),
            reason: Some("理由".into()),
            matched: vec!["燃料".into()],
            excluded: vec!["核融合".into()],
            read_at: None,
            rating: crate::db::Rating::new(4),
            bookmarked: false,
            has_translation: false,
            translation_requested: false,
            locked_by: vec![],
            story_id: 1,
            story_others: vec![],
            story_read: false,
            story_rated: false,
        };
        let json = serde_json::to_value(Article::new(&item, &SourceLabels::default())).unwrap();
        assert_eq!(json["matched"], serde_json::json!(["燃料"]));
        assert_eq!(json["excluded"], serde_json::json!(["核融合"]));
        assert_eq!(json["rating"], 4);
        // 点数は推薦点で、補正の前の点数も並べる。`llm_score` は互換のため同じ値で残す
        assert_eq!(
            (
                json["score"].clone(),
                json["base_score"].clone(),
                json["llm_score"].clone()
            ),
            (80.into(), 80.into(), 80.into())
        );
        assert!(json.get("feedback").is_none(), "{json}");
    }
}
