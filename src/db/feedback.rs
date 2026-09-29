//! 開いた記録・ブックマーク・見送りなどの利用者の行動。評価のラベルにはしない（ラベルは `signals` の評価）。

use super::*;

/// 利用者の行動。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    OpenDetail,
    OpenTranslation,
    /// 一覧で後で読むために残した（外すまでブックマークとして残る）
    Bookmark,
    /// 一覧で見出しだけ見て見送った
    Dismiss,
}

impl SignalKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::OpenDetail => "open_detail",
            Self::OpenTranslation => "open_translation",
            Self::Bookmark => "bookmark",
            Self::Dismiss => "dismiss",
        }
    }
}

impl Db {
    /// 行動を記録する。ブックマークなら、外すまでブックマークとしても残す。
    pub fn record_event(
        &self,
        user_id: i64,
        article_id: i64,
        kind: SignalKind,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO events (user_id, article_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![user_id, article_id, kind.as_str(), timestamp(now)],
        )?;
        if kind == SignalKind::Bookmark {
            tx.execute(
                "INSERT OR IGNORE INTO bookmarks (user_id, article_id, event_id)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![user_id, article_id, tx.last_insert_rowid()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// ブックマークを外す。ブックマークした行動は残す。
    pub fn unbookmark(&self, user_id: i64, article_id: i64) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM bookmarks WHERE user_id = ?1 AND article_id = ?2",
            rusqlite::params![user_id, article_id],
        )?;
        Ok(())
    }

    /// 誤操作の取り消し。その種類の最新の行動を無かったことにする（ブックマークなら外す）。
    pub fn undo_event(
        &self,
        user_id: i64,
        article_id: i64,
        kind: SignalKind,
    ) -> Result<(), DbError> {
        // その行動で付いたブックマークは、外部キー（bookmarks.event_id）の CASCADE で外れる
        self.conn.execute(
            "DELETE FROM events WHERE id = (
               SELECT id FROM events
               WHERE user_id = ?1 AND article_id = ?2 AND kind = ?3
               ORDER BY created_at DESC, id DESC LIMIT 1)",
            rusqlite::params![user_id, article_id, kind.as_str()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    /// 残っている行動の種類（新しい順）
    fn events(db: &Db) -> Vec<String> {
        db.query_strings("SELECT kind FROM events ORDER BY created_at DESC, id DESC")
            .unwrap()
    }

    /// 一覧でブックマークした記事は、外すまでブックマークとして残る。
    /// ブックマークした行動は、外しても評価のラベルとして残る。
    #[test]
    fn bookmark_marks_items_until_removed_and_keeps_the_signal() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let bookmarked = |db: &Db| db.search_articles(&search_query(db)).unwrap()[0].bookmarked;
        assert!(!bookmarked(&db));

        db.record_event(owner, a, SignalKind::Bookmark, t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert!(bookmarked(&db));
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    bookmarked: true,
                    ..search_query(&db)
                }
            ),
            [a]
        );

        db.unbookmark(owner, a).unwrap();
        assert!(!bookmarked(&db));
        assert_eq!(events(&db), ["bookmark"]);
    }

    /// 「見ない」にした記事は 👎 と同じく一覧の既定から隠れ、行動として残る。
    #[test]
    fn dismissed_articles_are_hidden_by_default_and_become_signals() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let kept = scored_article(
            &db,
            "https://e.com/kept",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            80,
        );
        let dismissed = scored_article(
            &db,
            "https://e.com/dismissed",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.record_event(
            owner,
            dismissed,
            SignalKind::Dismiss,
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();

        assert_eq!(list_ids(&db, false), [kept]);
        assert_eq!(list_ids(&db, true), [dismissed, kept]);
        assert_eq!(
            found(
                &db,
                SearchQuery {
                    hide_below: Some(60),
                    ..search_query(&db)
                }
            ),
            [kept]
        );
        assert_eq!(events(&db), ["dismiss"]);
    }

    /// 誤って振り分けたときの取り消しは、その行動が無かったことにする（評価のラベルにも使わない）。
    #[test]
    fn undo_removes_the_latest_event_and_the_bookmark() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let b = scored_article(
            &db,
            "https://e.com/b",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            80,
        );
        db.record_event(owner, a, SignalKind::Bookmark, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.record_event(owner, b, SignalKind::Dismiss, t("2026-09-27T00:01:00Z"))
            .unwrap();
        db.record_event(owner, b, SignalKind::OpenDetail, t("2026-09-27T00:02:00Z"))
            .unwrap();

        db.undo_event(owner, a, SignalKind::Bookmark).unwrap();
        db.undo_event(owner, b, SignalKind::Dismiss).unwrap();

        let items = db.list_articles(list_query(&db, false)).unwrap();
        let ids: Vec<i64> = items.iter().map(|i| i.article_id).collect();
        assert_eq!(ids, [a, b]);
        assert!(!items[0].bookmarked);
        // 取り消したものだけが消え、ほかの行動は残る
        assert_eq!(events(&db), ["open_detail"]);
    }

    /// 取り消すのは、取り消す行動で付いたブックマークだけ。それより前からのブックマークは残す。
    #[test]
    fn undo_keeps_a_bookmark_made_before() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        db.record_event(owner, a, SignalKind::Bookmark, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.record_event(owner, a, SignalKind::Bookmark, t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.undo_event(owner, a, SignalKind::Bookmark).unwrap();
        assert!(db.search_articles(&search_query(&db)).unwrap()[0].bookmarked);
        assert_eq!(events(&db), ["bookmark"]);
    }

    /// 同じ時刻（ミリ秒）の行動が重なっても、取り消すのはその行動で付いたブックマークだけ。
    /// 外した後に付け直したブックマークは、付け直した行動の取り消しで外れる。
    #[test]
    fn undo_follows_the_event_that_made_the_bookmark() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = scored_article(
            &db,
            "https://e.com/a",
            Lang::En,
            "2026-09-26T00:00:00.000Z",
            90,
        );
        let bookmarked = |db: &Db| db.search_articles(&search_query(db)).unwrap()[0].bookmarked;
        let at = t("2026-09-27T00:00:00Z");
        db.record_event(owner, a, SignalKind::Bookmark, at).unwrap();
        db.record_event(owner, a, SignalKind::Bookmark, at).unwrap();
        db.undo_event(owner, a, SignalKind::Bookmark).unwrap();
        assert!(bookmarked(&db));

        db.unbookmark(owner, a).unwrap();
        db.record_event(owner, a, SignalKind::Bookmark, t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.undo_event(owner, a, SignalKind::Bookmark).unwrap();
        assert!(!bookmarked(&db));
    }
}
