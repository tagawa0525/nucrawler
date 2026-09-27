//! 既読・不要・ブックマークなどの操作と、採点に使う信号。

use super::*;

/// 利用者の行動。推薦への効き方は、不要が 👎 ≫ 見ない、関心が
/// 詳細を開いた ＜ 和訳を開いた・ブックマーク ≪ 👍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    OpenDetail,
    OpenTranslation,
    Up,
    Down,
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
            Self::Up => "up",
            Self::Down => "down",
            Self::Bookmark => "bookmark",
            Self::Dismiss => "dismiss",
        }
    }

    /// DB の `events.kind` の値を読む。
    pub(super) fn parse(s: &str) -> Result<Self, DbError> {
        Ok(match s {
            "open_detail" => Self::OpenDetail,
            "open_translation" => Self::OpenTranslation,
            "up" => Self::Up,
            "down" => Self::Down,
            "bookmark" => Self::Bookmark,
            "dismiss" => Self::Dismiss,
            other => return Err(DbError::UnexpectedValue(format!("events.kind = {other:?}"))),
        })
    }
}

/// 採点の参考にする直近の行動と、その記事の見出し。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub kind: SignalKind,
    pub title_ja: String,
}

/// 利用者の最新の 👍/👎。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    Up,
    Down,
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

    /// ブックマークを外す。ブックマークした行動は採点の手がかりとして残す。
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

    /// 直近の行動を新しい順に最大 `limit` 件。digest の無い記事の行動は含めない。
    pub fn recent_signals(&self, user_id: i64, limit: usize) -> Result<Vec<Signal>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, title_ja FROM (
               SELECT e.id, e.created_at, e.kind,
                      (SELECT r.title_ja FROM artifacts AS r
                       WHERE r.article_id = e.article_id AND r.kind = 'digest'
                         -- 利用者が閲覧できない（会員限定の）digest の見出しは使わない
                         AND NOT EXISTS (
                           SELECT 1 FROM artifact_access AS aa
                           WHERE aa.artifact_id = r.id
                             AND aa.membership_id NOT IN (
                               SELECT membership_id FROM user_memberships
                               WHERE user_id = ?1))
                       ORDER BY r.created_at DESC, r.id DESC LIMIT 1) AS title_ja
               FROM events AS e WHERE e.user_id = ?1)
             WHERE title_ja IS NOT NULL
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![user_id, i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?;
        rows.map(|row| {
            let (kind, title_ja) = row?;
            Ok(Signal {
                kind: SignalKind::parse(&kind)?,
                title_ja,
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    /// 見出しは、利用者が閲覧できる digest からだけ取る。
    #[test]
    fn recent_signals_use_viewable_digests_only() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let aesj: i64 = db
            .conn()
            .query_row("SELECT id FROM memberships WHERE code = 'aesj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(
            &db,
            a,
            "sonnet",
            "公開の見出し",
            true,
            "2026-09-26T01:00:00Z",
        );
        let gated = insert_content(&db, a, Some(aesj));
        let payload = serde_json::json!({
            "title_ja": "会員限定の見出し", "summary_ja": "s", "points_ja": ["p"],
            "implications_ja": "", "lwr_relevant": true, "topics": ["燃料"],
        });
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Digest,
                backend: "claude-cli",
                model: "opus",
                prompt_version: 1,
                payload: &payload,
                inputs: &[gated],
                glossary_at: None,
            },
            t("2026-09-26T02:00:00Z"),
        )
        .unwrap();
        db.record_event(owner, a, SignalKind::Up, t("2026-09-27T01:00:00Z"))
            .unwrap();
        assert_eq!(
            db.recent_signals(owner, 10).unwrap()[0].title_ja,
            "公開の見出し"
        );
    }

    #[test]
    fn recent_signals_are_newest_first_with_titles() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        add_digest(&db, a, "sonnet", "記事A", true, "2026-09-26T01:00:00Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        add_digest(&db, b, "sonnet", "記事B", true, "2026-09-26T01:00:00Z");
        let no_digest = page_article(&db, "https://e.com/c", "2026-09-26T00:00:00.000Z");
        db.record_event(owner, a, SignalKind::OpenDetail, t("2026-09-27T01:00:00Z"))
            .unwrap();
        db.record_event(owner, b, SignalKind::Down, t("2026-09-27T02:00:00Z"))
            .unwrap();
        db.record_event(owner, no_digest, SignalKind::Up, t("2026-09-27T03:00:00Z"))
            .unwrap();
        db.record_event(owner, a, SignalKind::Up, t("2026-09-27T04:00:00Z"))
            .unwrap();
        assert_eq!(
            db.recent_signals(owner, 10).unwrap(),
            [
                Signal {
                    kind: SignalKind::Up,
                    title_ja: "記事A".into()
                },
                Signal {
                    kind: SignalKind::Down,
                    title_ja: "記事B".into()
                },
                Signal {
                    kind: SignalKind::OpenDetail,
                    title_ja: "記事A".into()
                },
            ]
        );
        assert_eq!(db.recent_signals(owner, 1).unwrap().len(), 1);
    }

    /// 一覧でブックマークした記事は、外すまでブックマークとして残る。
    /// ブックマークした行動は、外しても採点の手がかりとして残る。
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
        assert_eq!(
            db.recent_signals(owner, 10).unwrap(),
            [Signal {
                kind: SignalKind::Bookmark,
                title_ja: "題".into()
            }]
        );
    }

    /// 「見ない」にした記事は 👎 と同じく一覧の既定から隠れ、弱い不要として採点に渡る。
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
        assert_eq!(
            db.recent_signals(owner, 10).unwrap(),
            [Signal {
                kind: SignalKind::Dismiss,
                title_ja: "題".into()
            }]
        );
    }

    /// 誤って振り分けたときの取り消しは、その行動が無かったことにする（採点にも渡さない）。
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
        assert_eq!(
            db.recent_signals(owner, 10).unwrap(),
            [Signal {
                kind: SignalKind::OpenDetail,
                title_ja: "題".into()
            }]
        );
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
        assert_eq!(
            db.recent_signals(owner, 10).unwrap(),
            [Signal {
                kind: SignalKind::Bookmark,
                title_ja: "題".into()
            }]
        );
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
