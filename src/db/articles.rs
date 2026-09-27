//! 記事と本文の登録。

use super::*;

pub struct NewArticle<'a> {
    pub source_id: &'a str,
    pub url: &'a str,
    pub title: &'a str,
    pub lang: Lang,
    /// RFC 3339
    pub published_at: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Lead,
    Body,
    Abstract,
    Fulltext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentOrigin {
    Feed,
    Page,
    Pdf,
    Upload,
    Login,
}

impl ContentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Body => "body",
            Self::Abstract => "abstract",
            Self::Fulltext => "fulltext",
        }
    }
}

impl ContentOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Feed => "feed",
            Self::Page => "page",
            Self::Pdf => "pdf",
            Self::Upload => "upload",
            Self::Login => "login",
        }
    }
}

impl Db {
    /// URL を正規化して登録する。既に同じ URL があれば `None`。
    pub fn insert_article(&self, a: &NewArticle) -> Result<Option<i64>, DbError> {
        let url = normalize_url(a.url)?;
        let lang = lang_code(a.lang);
        let inserted = self.conn.execute(
            &format!(
                "INSERT INTO articles (source_id, url, title, lang, published_at, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, {NOW})
                 ON CONFLICT (url) DO NOTHING"
            ),
            rusqlite::params![a.source_id, url, a.title, lang, a.published_at],
        )?;
        Ok((inserted > 0).then(|| self.conn.last_insert_rowid()))
    }

    /// 公開の本文の部分を登録する。会員限定の部分はログイン取得の実装時に別の関数で扱う。
    pub fn insert_content(
        &self,
        article_id: i64,
        kind: ContentKind,
        origin: ContentOrigin,
        text: &str,
    ) -> Result<i64, DbError> {
        Ok(self.conn.query_row(
            &format!(
                "INSERT INTO contents (article_id, kind, origin, text, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, {NOW}) RETURNING id"
            ),
            rusqlite::params![article_id, kind.as_str(), origin.as_str(), text],
            |r| r.get(0),
        )?)
    }

    /// 記事と、その公開の本文の部分を 1 つのトランザクションで登録する。既に同じ URL があれば
    /// 何もせず `None`。途中で止まっても「記事だけあって本文が無い」状態を残さない。
    pub fn insert_article_with_contents(
        &self,
        a: &NewArticle,
        contents: &[(ContentKind, ContentOrigin, &str)],
    ) -> Result<Option<i64>, DbError> {
        let tx = self.conn.unchecked_transaction()?;
        let Some(id) = self.insert_article(a)? else {
            return Ok(None);
        };
        for &(kind, origin, text) in contents {
            self.insert_content(id, kind, origin, text)?;
        }
        tx.commit()?;
        Ok(Some(id))
    }
}

/// 重複判定用に URL を正規化する：fragment と追跡用のクエリ（utm_*、fbclid、gclid）を除く。
pub fn normalize_url(url: &str) -> Result<String, DbError> {
    let mut u = url::Url::parse(url).map_err(|source| DbError::InvalidUrl {
        url: url.to_string(),
        source,
    })?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(DbError::UnsupportedScheme {
            url: url.to_string(),
            scheme: u.scheme().to_string(),
        });
    }
    u.set_fragment(None);
    // 追跡用パラメータがあるときだけクエリを組み直し、それ以外の元の表記は保つ。
    if u.query_pairs().any(|(k, _)| is_tracking_param(&k)) {
        let kept: Vec<(String, String)> = u
            .query_pairs()
            .filter(|(k, _)| !is_tracking_param(k))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if kept.is_empty() {
            u.set_query(None);
        } else {
            u.query_pairs_mut().clear().extend_pairs(kept);
        }
    }
    Ok(u.into())
}

fn is_tracking_param(key: &str) -> bool {
    key.starts_with("utm_") || matches!(key, "fbclid" | "gclid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    #[test]
    fn inserts_public_content() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        db.insert_content(a, ContentKind::Lead, ContentOrigin::Feed, "概要")
            .unwrap();
        let rows = db
            .query_strings(
                "SELECT kind || '|' || origin || '|' || coalesce(access_membership_id, 'public')
                        || '|' || text || '|' || (fetched_at LIKE '____-__-__T__:__:__%Z')
                 FROM contents",
            )
            .unwrap();
        assert_eq!(rows, ["lead|feed|public|概要|1"]);
    }

    #[test]
    fn insert_article_dedupes_by_normalized_url() {
        let db = Db::open_in_memory().unwrap();
        let first = db
            .insert_article(&article("https://example.com/a?id=1"))
            .unwrap();
        assert!(first.is_some());
        let again = db
            .insert_article(&article("https://EXAMPLE.com/a?id=1&utm_source=rss#top"))
            .unwrap();
        assert_eq!(again, None);
        let other = db
            .insert_article(&article("https://example.com/a?id=2"))
            .unwrap();
        assert!(other.is_some());
    }

    #[test]
    fn insert_article_rejects_invalid_url() {
        let db = Db::open_in_memory().unwrap();
        let err = db.insert_article(&article("not a url")).unwrap_err();
        assert!(matches!(err, DbError::InvalidUrl { .. }), "{err}");
    }

    #[test]
    fn normalize_url_cases() {
        let cases = [
            ("https://Example.COM/a#frag", "https://example.com/a"),
            (
                "https://e.com/a?utm_source=x&id=3&utm_medium=y&fbclid=z",
                "https://e.com/a?id=3",
            ),
            ("https://e.com/a?utm_source=x", "https://e.com/a"),
            ("https://e.com/a?gclid=1&b=2&a=1", "https://e.com/a?b=2&a=1"),
            ("http://e.com/", "http://e.com/"),
        ];
        for (input, want) in cases {
            assert_eq!(normalize_url(input).unwrap(), want, "{input}");
        }
    }

    #[test]
    fn normalize_url_rejects_non_http() {
        let err = normalize_url("ftp://e.com/a").unwrap_err();
        assert!(matches!(err, DbError::UnsupportedScheme { .. }), "{err}");
    }
}
