//! 取得元ごとの取得の成否。

use super::*;

/// `status` 用：ソースごとの記事数と取得状況。
#[derive(Debug, PartialEq, Eq)]
pub struct SourceOverview {
    pub source_id: String,
    pub articles: i64,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
}

/// 抽出待ちの記事。
#[derive(Debug, PartialEq, Eq)]
pub struct PendingPage {
    pub article_id: i64,
    pub source_id: String,
    pub url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SourceState {
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
}

impl Db {
    /// 取得に成功した時刻を記録する。直前のエラーは消す。
    pub fn record_source_success(&self, source_id: &str) -> Result<(), DbError> {
        self.conn.execute(
            &format!(
                "INSERT INTO source_state (source_id, last_success_at) VALUES (?1, {NOW})
                 ON CONFLICT (source_id) DO UPDATE SET
                   last_success_at = excluded.last_success_at,
                   last_error = NULL,
                   last_error_at = NULL"
            ),
            [source_id],
        )?;
        Ok(())
    }

    /// 取得の失敗を記録する。最後に成功した時刻は残す。
    pub fn record_source_failure(&self, source_id: &str, error: &str) -> Result<(), DbError> {
        self.conn.execute(
            &format!(
                "INSERT INTO source_state (source_id, last_error, last_error_at) VALUES (?1, ?2, {NOW})
                 ON CONFLICT (source_id) DO UPDATE SET
                   last_error = excluded.last_error,
                   last_error_at = excluded.last_error_at"
            ),
            [source_id, error],
        )?;
        Ok(())
    }

    pub fn source_state(&self, source_id: &str) -> Result<Option<SourceState>, DbError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT last_success_at, last_error, last_error_at FROM source_state
                 WHERE source_id = ?1",
                [source_id],
                |r| {
                    Ok(SourceState {
                        last_success_at: r.get(0)?,
                        last_error: r.get(1)?,
                        last_error_at: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// 記事か取得記録のあるソースすべて（source_id 順）。
    pub fn source_overview(&self) -> Result<Vec<SourceOverview>, DbError> {
        let mut stmt = self.conn.prepare(
            "WITH ids AS (SELECT source_id FROM articles UNION SELECT source_id FROM source_state),
                  counts AS (SELECT source_id, count(*) AS n FROM articles GROUP BY source_id)
             SELECT ids.source_id, coalesce(counts.n, 0),
                    st.last_success_at, st.last_error, st.last_error_at
             FROM ids
             LEFT JOIN counts USING (source_id)
             LEFT JOIN source_state AS st USING (source_id)
             ORDER BY ids.source_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SourceOverview {
                source_id: r.get(0)?,
                articles: r.get(1)?,
                last_success_at: r.get(2)?,
                last_error: r.get(3)?,
                last_error_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    #[test]
    fn records_source_success_and_failure() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.source_state("s").unwrap(), None);

        db.record_source_failure("s", "HTTP 403").unwrap();
        let st = db.source_state("s").unwrap().unwrap();
        assert_eq!(st.last_error.as_deref(), Some("HTTP 403"));
        assert!(st.last_error_at.is_some());
        assert!(st.last_success_at.is_none());

        db.record_source_success("s").unwrap();
        let st = db.source_state("s").unwrap().unwrap();
        assert!(st.last_success_at.is_some());
        assert_eq!((st.last_error, st.last_error_at), (None, None));

        db.record_source_failure("s", "timeout").unwrap();
        let st = db.source_state("s").unwrap().unwrap();
        assert!(st.last_success_at.is_some(), "last success is kept");
        assert_eq!(st.last_error.as_deref(), Some("timeout"));
    }

    #[test]
    fn overview_combines_articles_and_state() {
        let db = Db::open_in_memory().unwrap();
        for url in ["https://e.com/1", "https://e.com/2"] {
            db.insert_article(&NewArticle {
                source_id: "a",
                ..article(url)
            })
            .unwrap();
        }
        db.record_source_success("a").unwrap();
        db.record_source_failure("b", "HTTP 403").unwrap();
        let ov = db.source_overview().unwrap();
        let ids: Vec<_> = ov
            .iter()
            .map(|o| (o.source_id.as_str(), o.articles))
            .collect();
        assert_eq!(ids, [("a", 2), ("b", 0)]);
        assert!(ov[0].last_success_at.is_some());
        assert_eq!(ov[1].last_error.as_deref(), Some("HTTP 403"));
    }
}
