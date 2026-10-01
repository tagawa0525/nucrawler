//! 記事への指摘とコメント。

use super::*;

/// 指摘の種類。訳語の指摘は気になった訳を、ほかの種類は内容を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    /// 訳語
    Term,
    /// 和訳の誤り
    Translation,
    /// 要約の誤り
    Digest,
    /// トピック
    Topic,
    /// 本文の取得漏れ
    Body,
    /// その他
    Other,
}

impl ReportKind {
    pub const ALL: [ReportKind; 6] = [
        ReportKind::Term,
        ReportKind::Translation,
        ReportKind::Digest,
        ReportKind::Topic,
        ReportKind::Body,
        ReportKind::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReportKind::Term => "term",
            ReportKind::Translation => "translation",
            ReportKind::Digest => "digest",
            ReportKind::Topic => "topic",
            ReportKind::Body => "body",
            ReportKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// 送られた指摘。訳語の指摘は `found`（気になった訳）が必須で、ほかは分からなければ `None`。
/// ほかの種類は内容（`body`）だけを持つ。
#[derive(Debug, Clone, Copy)]
pub enum NewReport<'a> {
    Term {
        found: &'a str,
        wanted: Option<&'a str>,
        source: Option<&'a str>,
        note: Option<&'a str>,
    },
    Other {
        kind: ReportKind,
        body: &'a str,
    },
}

/// 指摘の対応状況。状況を変えた時刻を対応日時にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStatus {
    /// まだ対応していない
    Pending,
    /// 訳語集に反映した（訳語の指摘だけ）
    Added,
    /// 訳語集にあったのに、その訳が使われていなかった（訳語の指摘だけ）
    Existing,
    /// 対応した（訳語以外の指摘）
    Done,
    /// 今のままでよい
    Rejected,
}

impl ReportStatus {
    pub const ALL: [ReportStatus; 5] = [
        ReportStatus::Pending,
        ReportStatus::Added,
        ReportStatus::Existing,
        ReportStatus::Done,
        ReportStatus::Rejected,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReportStatus::Pending => "pending",
            ReportStatus::Added => "added",
            ReportStatus::Existing => "existing",
            ReportStatus::Done => "done",
            ReportStatus::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }

    /// その種類の指摘に付けられる状況。
    pub fn for_kind(kind: ReportKind) -> &'static [ReportStatus] {
        use ReportStatus::*;
        match kind {
            ReportKind::Term => &[Pending, Added, Existing, Rejected],
            _ => &[Pending, Done, Rejected],
        }
    }
}

/// 受付箱の 1 件。訳語の指摘は `found` を、ほかの種類は内容を `note` に持つ。
/// `term` は結び付けた訳語（id と訳。訳語の指摘だけ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub id: i64,
    pub article_id: i64,
    /// 閲覧者が見られる最新の要約の見出し（無ければ原題）
    pub article_title: String,
    pub kind: ReportKind,
    pub found: Option<String>,
    pub wanted: Option<String>,
    pub source: Option<String>,
    pub note: Option<String>,
    pub status: ReportStatus,
    pub term: Option<(i64, String)>,
    pub reply: Option<String>,
    pub reported_at: String,
    pub resolved_at: Option<String>,
}

/// 受付箱の絞り込み。`None` は絞らない。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportFilter {
    pub status: Option<ReportStatus>,
    pub kind: Option<ReportKind>,
    pub article_id: Option<i64>,
    /// この利用者が出した指摘だけ（一般の利用者には自分の指摘だけを見せる）
    pub reporter: Option<i64>,
}

/// コメントの公開範囲。公開はほかの利用者にも見せ、非公開は書いた本人だけが見る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Visibility::Public, Visibility::Private]
            .into_iter()
            .find(|x| x.as_str() == s)
    }
}

/// 記事へのコメント。`mine` は閲覧者が書いたもの（直せる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub visibility: Visibility,
    pub mine: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Db {
    /// 記事へのコメント（古い順）。利用者 `user_id` が書いたものと、ほかの利用者の公開のもの。
    pub fn comments(&self, user_id: i64, article_id: i64) -> Result<Vec<Comment>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, body, visibility, user_id = ?1, created_at, updated_at
             FROM comments
             WHERE article_id = ?2 AND (user_id = ?1 OR visibility = 'public')
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([user_id, article_id], |r| {
            Ok((
                r.get::<_, String>(2)?,
                Comment {
                    id: r.get(0)?,
                    body: r.get(1)?,
                    visibility: Visibility::Private,
                    mine: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                },
            ))
        })?;
        rows.map(|row| {
            let (visibility, comment) = row?;
            let visibility = Visibility::parse(&visibility).ok_or_else(|| {
                DbError::UnexpectedValue(format!("comment visibility {visibility:?}"))
            })?;
            Ok(Comment {
                visibility,
                ..comment
            })
        })
        .collect()
    }

    /// コメントを書いて id を返す。
    pub fn add_comment(
        &self,
        user_id: i64,
        article_id: i64,
        body: &str,
        visibility: Visibility,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, DbError> {
        self.conn.execute(
            "INSERT INTO comments (user_id, article_id, body, visibility, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            rusqlite::params![
                user_id,
                article_id,
                body,
                visibility.as_str(),
                timestamp(now)
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 自分のコメントを直し、その記事の id を返す。無いか他人のものなら None。
    pub fn update_comment(
        &self,
        user_id: i64,
        id: i64,
        body: &str,
        visibility: Visibility,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<i64>, DbError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "UPDATE comments SET body = ?3, visibility = ?4, updated_at = ?5
                 WHERE id = ?1 AND user_id = ?2
                 RETURNING article_id",
                rusqlite::params![id, user_id, body, visibility.as_str(), timestamp(now)],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 自分のコメントを消し、その記事の id を返す。無いか他人のものなら None。
    pub fn delete_comment(&self, user_id: i64, id: i64) -> Result<Option<i64>, DbError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "DELETE FROM comments WHERE id = ?1 AND user_id = ?2 RETURNING article_id",
                [id, user_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 指摘を受付箱に入れる。
    pub fn add_report(
        &self,
        user_id: i64,
        article_id: i64,
        report: &NewReport<'_>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let (kind, found, wanted, source, note) = match *report {
            NewReport::Term {
                found,
                wanted,
                source,
                note,
            } => (ReportKind::Term, Some(found), wanted, source, note),
            NewReport::Other { kind, body } => (kind, None, None, None, Some(body)),
        };
        self.conn.execute(
            "INSERT INTO reports
               (user_id, article_id, kind, found, wanted, source, note, reported_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                user_id,
                article_id,
                kind.as_str(),
                found,
                wanted,
                source,
                note,
                timestamp(now),
            ],
        )?;
        Ok(())
    }

    /// 受付箱（新しい順）。記事の見出しは、利用者 `user_id` が閲覧できる最新の要約から取る
    /// （無ければ見出しの和訳、それも無ければ原題）。
    pub fn reports(&self, user_id: i64, filter: &ReportFilter) -> Result<Vec<Report>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT r.id, r.article_id,
                    coalesce({title_ja}, a.title),
                    r.kind, r.found, r.wanted, r.source, r.note, r.status, r.term_id, t.target,
                    r.reply, r.reported_at, r.resolved_at
             FROM reports AS r
             JOIN articles AS a ON a.id = r.article_id
             LEFT JOIN glossary_terms AS t ON t.id = r.term_id
             WHERE (:status IS NULL OR r.status = :status)
               AND (:kind IS NULL OR r.kind = :kind)
               AND (:article IS NULL OR r.article_id = :article)
               AND (:reporter IS NULL OR r.user_id = :reporter)
             ORDER BY r.reported_at DESC, r.id DESC",
            title_ja = super::read::title_ja(
                &super::read::latest_digest("title_ja", "a.id", ":user"),
                "a.id",
            ),
        ))?;
        let mut rows = stmt.query(rusqlite::named_params! {
            ":user": user_id,
            ":status": filter.status.map(ReportStatus::as_str),
            ":kind": filter.kind.map(ReportKind::as_str),
            ":article": filter.article_id,
            ":reporter": filter.reporter,
        })?;
        let mut reports = Vec::new();
        while let Some(r) = rows.next()? {
            let kind: String = r.get(3)?;
            let status: String = r.get(8)?;
            let term = match (r.get::<_, Option<i64>>(9)?, r.get::<_, Option<String>>(10)?) {
                (Some(id), Some(target)) => Some((id, target)),
                _ => None,
            };
            reports.push(Report {
                id: r.get(0)?,
                article_id: r.get(1)?,
                article_title: r.get(2)?,
                kind: ReportKind::parse(&kind)
                    .ok_or_else(|| DbError::UnexpectedValue(format!("report kind {kind:?}")))?,
                found: r.get(4)?,
                wanted: r.get(5)?,
                source: r.get(6)?,
                note: r.get(7)?,
                status: ReportStatus::parse(&status)
                    .ok_or_else(|| DbError::UnexpectedValue(format!("report status {status:?}")))?,
                term,
                reply: r.get(11)?,
                reported_at: r.get(12)?,
                resolved_at: r.get(13)?,
            });
        }
        Ok(reports)
    }

    /// 指摘の種類。無ければ None。
    pub fn report_kind(&self, id: i64) -> Result<Option<ReportKind>, DbError> {
        use rusqlite::OptionalExtension;
        let kind: Option<String> = self
            .conn
            .query_row("SELECT kind FROM reports WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        kind.map(|k| {
            ReportKind::parse(&k)
                .ok_or_else(|| DbError::UnexpectedValue(format!("report kind {k:?}")))
        })
        .transpose()
    }

    /// 対応状況ごとの件数（`ReportStatus::ALL` の順。0 件も含む）。
    pub fn report_counts(&self) -> Result<Vec<(ReportStatus, i64)>, DbError> {
        ReportStatus::ALL
            .into_iter()
            .map(|status| {
                let n = self.conn.query_row(
                    "SELECT count(*) FROM reports WHERE status = ?1",
                    [status.as_str()],
                    |r| r.get(0),
                )?;
                Ok((status, n))
            })
            .collect()
    }

    /// 指摘の対応状況を変える。無ければ false。対応日時は状況が変わったときだけ `now` にし、
    /// 受付中に戻せば消す。
    pub fn resolve_report(
        &self,
        id: i64,
        status: ReportStatus,
        term_id: Option<i64>,
        reply: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        // 対応日時は状況を変えたときだけ進める（ひとことや訳語だけの修正では変えない）
        Ok(self.conn.execute(
            "UPDATE reports
             SET resolved_at = CASE WHEN ?2 = 'pending' THEN NULL
                                    WHEN status = ?2 THEN resolved_at
                                    ELSE ?5 END,
                 status = ?2, term_id = ?3, reply = ?4
             WHERE id = ?1",
            rusqlite::params![id, status.as_str(), term_id, reply, timestamp(now)],
        )? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    fn report(db: &Db, article_id: i64, found: &str, at: &str) {
        let owner = db.owner_id().unwrap();
        let report = NewReport::Term {
            found,
            wanted: Some("燃料取替停止"),
            source: None,
            note: None,
        };
        db.add_report(owner, article_id, &report, t(at)).unwrap();
    }

    /// 指摘は受付中で入り、新しい順に並ぶ。見出しは要約が無ければ原題。
    #[test]
    fn term_reports_start_pending_and_are_listed_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let b = db
            .insert_article(&article("https://e.com/b"))
            .unwrap()
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        report(&db, b, "燃料補給停止", "2026-09-27T01:00:00Z");
        let reports = db
            .reports(db.owner_id().unwrap(), &ReportFilter::default())
            .unwrap();
        let found: Vec<&str> = reports.iter().filter_map(|r| r.found.as_deref()).collect();
        assert_eq!(found, ["燃料補給停止", "給油停止"]);
        let r = &reports[1];
        assert_eq!(r.article_id, a);
        assert_eq!(r.article_title, "t");
        assert_eq!(r.wanted.as_deref(), Some("燃料取替停止"));
        assert_eq!(r.status, ReportStatus::Pending);
        assert_eq!(r.reported_at, "2026-09-27T00:00:00.000Z");
        assert_eq!(
            (r.resolved_at.as_deref(), &r.term, &r.reply),
            (None, &None, &None)
        );
        assert_eq!(
            db.reports(
                db.owner_id().unwrap(),
                &ReportFilter {
                    article_id: Some(a),
                    ..ReportFilter::default()
                }
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            db.report_counts().unwrap(),
            [
                (ReportStatus::Pending, 2),
                (ReportStatus::Added, 0),
                (ReportStatus::Existing, 0),
                (ReportStatus::Done, 0),
                (ReportStatus::Rejected, 0),
            ]
        );
    }

    /// 対応すると状況・訳語・ひとことと対応日時を残し、受付中に戻すと対応日時を消す。
    /// 結び付けた訳語を消しても指摘は残る。
    #[test]
    fn term_reports_are_resolved_and_can_be_reopened() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let id = db
            .reports(db.owner_id().unwrap(), &ReportFilter::default())
            .unwrap()[0]
            .id;
        let term = db
            .add_glossary_term(
                &glossary_term(&["refuelling outage"], "燃料取替停止（英綴り）", None),
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        assert!(
            db.resolve_report(
                id,
                ReportStatus::Added,
                Some(term),
                Some("英綴りを追加"),
                t("2026-09-27T02:00:00Z")
            )
            .unwrap()
        );
        let r = &db
            .reports(
                db.owner_id().unwrap(),
                &ReportFilter {
                    status: Some(ReportStatus::Added),
                    ..ReportFilter::default()
                },
            )
            .unwrap()[0];
        assert_eq!(r.term, Some((term, "燃料取替停止（英綴り）".to_string())));
        assert_eq!(r.reply.as_deref(), Some("英綴りを追加"));
        assert_eq!(r.resolved_at.as_deref(), Some("2026-09-27T02:00:00.000Z"));
        assert!(
            db.reports(
                db.owner_id().unwrap(),
                &ReportFilter {
                    status: Some(ReportStatus::Pending),
                    ..ReportFilter::default()
                }
            )
            .unwrap()
            .is_empty()
        );

        db.delete_glossary_term(term).unwrap();
        assert_eq!(
            db.reports(db.owner_id().unwrap(), &ReportFilter::default())
                .unwrap()[0]
                .term,
            None
        );

        assert!(
            db.resolve_report(
                id,
                ReportStatus::Pending,
                None,
                None,
                t("2026-09-27T03:00:00Z")
            )
            .unwrap()
        );
        let r = &db
            .reports(db.owner_id().unwrap(), &ReportFilter::default())
            .unwrap()[0];
        assert_eq!(
            (r.status, r.resolved_at.as_deref()),
            (ReportStatus::Pending, None)
        );
        assert!(
            !db.resolve_report(
                9999,
                ReportStatus::Rejected,
                None,
                None,
                t("2026-09-27T03:00:00Z")
            )
            .unwrap()
        );
    }

    /// 訳語以外の指摘は内容だけを持ち、種類で絞れる。対応は「対応済」で、訳語は結び付けない。
    #[test]
    fn other_reports_carry_their_kind_and_body() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let other = NewReport::Other {
            kind: ReportKind::Digest,
            body: "要約の数値が原文と違う",
        };
        db.add_report(owner, a, &other, t("2026-09-27T01:00:00Z"))
            .unwrap();
        let only = |kind| {
            let filter = ReportFilter {
                kind: Some(kind),
                ..ReportFilter::default()
            };
            db.reports(owner, &filter).unwrap()
        };
        let digest = only(ReportKind::Digest);
        assert_eq!(digest.len(), 1);
        let r = &digest[0];
        assert_eq!(r.kind, ReportKind::Digest);
        assert_eq!(
            (r.found.as_deref(), r.note.as_deref()),
            (None, Some("要約の数値が原文と違う"))
        );
        assert_eq!(db.report_kind(r.id).unwrap(), Some(ReportKind::Digest));
        assert_eq!(db.report_kind(9999).unwrap(), None);
        assert_eq!(only(ReportKind::Term)[0].found.as_deref(), Some("給油停止"));

        assert!(
            db.resolve_report(
                r.id,
                ReportStatus::Done,
                None,
                None,
                t("2026-09-27T02:00:00Z")
            )
            .unwrap()
        );
        // 訳語集の状況や訳語は、訳語以外の指摘には付けられない
        for (status, term) in [(ReportStatus::Added, None), (ReportStatus::Done, Some(1))] {
            assert!(
                db.resolve_report(r.id, status, term, None, t("2026-09-27T03:00:00Z"))
                    .is_err()
            );
        }
        let term_id = only(ReportKind::Term)[0].id;
        assert!(
            db.resolve_report(
                term_id,
                ReportStatus::Done,
                None,
                None,
                t("2026-09-27T03:00:00Z")
            )
            .is_err()
        );
    }

    /// 受付箱の見出しには、利用者が閲覧できない（会員限定の本文から作った）要約を使わない。
    #[test]
    fn term_reports_do_not_show_titles_of_digests_the_viewer_cannot_see() {
        let db = Db::open_in_memory().unwrap();
        let m = insert_membership(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let gated = insert_content(&db, a, Some(m));
        let digest = insert_artifact(&db, a, "m");
        link_input(&db, digest, gated).unwrap();
        db.conn()
            .execute(
                "UPDATE artifacts SET payload = '{\"title_ja\": \"会員限定の見出し\"}' WHERE id = ?1",
                [digest],
            )
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let owner = db.owner_id().unwrap();
        assert_eq!(
            db.reports(owner, &ReportFilter::default()).unwrap()[0].article_title,
            "t"
        );
    }

    /// 要約の無い記事は、受付箱でも見出しの和訳を見出しに使う。
    #[test]
    fn reports_use_the_title_translation_without_a_digest() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        db.insert_artifact(
            &NewArtifact {
                article_id: a,
                kind: ArtifactKind::Title,
                backend: "claude-cli",
                model: "sonnet",
                prompt_version: 1,
                payload: &serde_json::json!({ "title_ja": "見出しの和訳" }),
                inputs: &[],
                glossary_at: None,
            },
            t("2026-09-26T00:00:00Z"),
        )
        .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let owner = db.owner_id().unwrap();
        assert_eq!(
            db.reports(owner, &ReportFilter::default()).unwrap()[0].article_title,
            "見出しの和訳"
        );
    }

    /// 対応日時は状況を変えたときだけ進み、ひとことや訳語だけを直しても変わらない。
    #[test]
    fn term_report_resolution_time_moves_only_with_the_status() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        report(&db, a, "給油停止", "2026-09-27T00:00:00Z");
        let owner = db.owner_id().unwrap();
        let id = db.reports(owner, &ReportFilter::default()).unwrap()[0].id;
        let resolve = |status, reply, at| {
            db.resolve_report(id, status, None, Some(reply), t(at))
                .unwrap();
            db.reports(owner, &ReportFilter::default()).unwrap()[0]
                .resolved_at
                .clone()
        };
        let first = resolve(ReportStatus::Added, "a", "2026-09-27T01:00:00Z");
        assert_eq!(first.as_deref(), Some("2026-09-27T01:00:00.000Z"));
        let same = resolve(ReportStatus::Added, "b", "2026-09-27T02:00:00Z");
        assert_eq!(same, first);
        let changed = resolve(ReportStatus::Rejected, "c", "2026-09-27T03:00:00Z");
        assert_eq!(changed.as_deref(), Some("2026-09-27T03:00:00.000Z"));
    }

    fn other_user(db: &Db) -> i64 {
        db.conn()
            .execute(
                "INSERT INTO users (login, display_name) VALUES ('other', 'other')",
                [],
            )
            .unwrap();
        db.conn().last_insert_rowid()
    }

    /// コメントは古い順に並び、自分のものと他人の公開のものだけが見える。
    #[test]
    fn comments_show_own_and_public_ones() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = other_user(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let at = |h: u32| t(&format!("2026-09-27T0{h}:00:00Z"));
        db.add_comment(owner, a, "自分のメモ", Visibility::Private, at(0))
            .unwrap();
        db.add_comment(other, a, "他人の公開", Visibility::Public, at(1))
            .unwrap();
        db.add_comment(other, a, "他人の非公開", Visibility::Private, at(2))
            .unwrap();
        let seen: Vec<(String, bool)> = db
            .comments(owner, a)
            .unwrap()
            .into_iter()
            .map(|c| (c.body, c.mine))
            .collect();
        assert_eq!(
            seen,
            [("自分のメモ".into(), true), ("他人の公開".into(), false)]
        );
        let mine = &db.comments(owner, a).unwrap()[0];
        assert_eq!(mine.visibility, Visibility::Private);
        assert_eq!(mine.created_at, "2026-09-27T00:00:00.000Z");
        assert_eq!(mine.updated_at, mine.created_at);
    }

    /// 直せるのも消せるのも自分のコメントだけ。
    #[test]
    fn comments_are_updated_and_deleted_only_by_their_author() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = other_user(&db);
        let a = db
            .insert_article(&article("https://e.com/a"))
            .unwrap()
            .unwrap();
        let id = db
            .add_comment(
                owner,
                a,
                "下書き",
                Visibility::Private,
                t("2026-09-27T00:00:00Z"),
            )
            .unwrap();
        let later = t("2026-09-27T01:00:00Z");
        assert_eq!(
            db.update_comment(other, id, "乗っ取り", Visibility::Public, later)
                .unwrap(),
            None
        );
        assert_eq!(
            db.update_comment(owner, id, "清書", Visibility::Public, later)
                .unwrap(),
            Some(a)
        );
        let c = &db.comments(other, a).unwrap()[0];
        assert_eq!(
            (c.body.as_str(), c.visibility, c.mine),
            ("清書", Visibility::Public, false)
        );
        assert_eq!(c.updated_at, "2026-09-27T01:00:00.000Z");
        assert_eq!(db.delete_comment(other, id).unwrap(), None);
        assert_eq!(db.delete_comment(owner, id).unwrap(), Some(a));
        assert!(db.comments(owner, a).unwrap().is_empty());
    }
}
