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
