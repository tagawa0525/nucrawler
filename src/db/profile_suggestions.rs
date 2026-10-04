//! プロファイルの更新案（計画 016）。評価を根拠に LLM が作った案と、今のプロファイルとの比較を残し、
//! 自動か人の判断で版にする。

use super::*;

use super::profile_versions::save_version;
use crate::prompt::suggest::Reason;

/// 案を作ったきっかけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionTrigger {
    /// 評価が増えたので `crawl` が作った
    Auto,
    /// 利用者が頼んだ
    Manual,
}

impl SuggestionTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Auto, Self::Manual]
            .into_iter()
            .find(|t| t.as_str() == value)
    }
}

/// 案の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionStatus {
    /// 採用か見送りを待つ
    Pending,
    /// 版にした
    Applied,
    /// 利用者が見送った
    Dismissed,
    /// 新しい案に置き換わった
    Superseded,
    /// 今のプロファイルと同じ中身だった（変える根拠が無かった）
    Unchanged,
}

impl SuggestionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Dismissed => "dismissed",
            Self::Superseded => "superseded",
            Self::Unchanged => "unchanged",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [
            Self::Pending,
            Self::Applied,
            Self::Dismissed,
            Self::Superseded,
            Self::Unchanged,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
    }
}

/// 保存する案。
#[derive(Debug, Clone, Copy)]
pub struct NewSuggestion<'a> {
    pub user_id: i64,
    /// 案の基にしたプロファイルの hash（案を作り始めたときの今のプロファイル）
    pub base_hash: &'a str,
    pub profile: &'a crate::profile::Profile,
    pub reasons: &'a [Reason],
    /// 根拠にした記事
    pub evidence: &'a [i64],
    /// 今のプロファイルの、評価した記事での一致率（一覧と同じ規則で選んだ点数）
    pub current: VersionStats,
    /// 案の、同じ評価での一致率
    pub candidate: VersionStats,
    pub trigger: SuggestionTrigger,
    /// `Pending` か `Unchanged`
    pub status: SuggestionStatus,
}

/// 保存した案。
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileSuggestion {
    pub id: i64,
    /// 案を作ったときの今の版
    pub base_version_id: i64,
    pub profile: crate::profile::Profile,
    pub reasons: Vec<Reason>,
    pub evidence: Vec<i64>,
    pub current: VersionStats,
    pub candidate: VersionStats,
    pub trigger: SuggestionTrigger,
    pub status: SuggestionStatus,
    pub created_at: String,
    /// 採用・見送り・置き換えの時刻
    pub decided_at: Option<String>,
}

impl Db {
    /// 案を保存する（作ったときの今の版を基にする）。待っている前の案は置き換える（古い評価で作った案を
    /// 残しても判断を迷わせるだけ）。案を作る間にプロファイルが変わっていたら（基にした hash が今の版と違う）、
    /// 保存せずに `None`。プロファイルが無ければ `DbError::NoProfileVersion`。
    pub fn save_suggestion(
        &self,
        suggestion: &NewSuggestion,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<i64>, DbError> {
        use rusqlite::OptionalExtension;
        let now = timestamp(now);
        let tx = self.immediate()?;
        let (base, hash): (i64, String) = tx
            .query_row(
                "SELECT id, hash FROM profile_versions WHERE user_id = ?1 AND retired_at IS NULL",
                [suggestion.user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(DbError::NoProfileVersion)?;
        if hash != suggestion.base_hash {
            return Ok(None);
        }
        tx.execute(
            "UPDATE profile_suggestions SET status = 'superseded', decided_at = ?2
             WHERE user_id = ?1 AND status = 'pending'",
            rusqlite::params![suggestion.user_id, now],
        )?;
        tx.execute(
            "INSERT INTO profile_suggestions
               (user_id, base_version_id, interests, excludes, reasons, evidence,
                current_rated, current_concordance, candidate_rated, candidate_concordance,
                trigger, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                suggestion.user_id,
                base,
                serde_json::to_string(&suggestion.profile.interests)?,
                serde_json::to_string(&suggestion.profile.exclude)?,
                serde_json::to_string(suggestion.reasons)?,
                serde_json::to_string(suggestion.evidence)?,
                suggestion.current.rated as i64,
                suggestion.current.concordance,
                suggestion.candidate.rated as i64,
                suggestion.candidate.concordance,
                suggestion.trigger.as_str(),
                suggestion.status.as_str(),
                now,
            ],
        )?;
        let id = tx.last_insert_rowid();
        // 頼まれた案を作ったので、依頼を片付ける
        if suggestion.trigger == SuggestionTrigger::Manual {
            tx.execute(
                "DELETE FROM profile_review_requests WHERE user_id = ?1",
                [suggestion.user_id],
            )?;
        }
        tx.commit()?;
        Ok(Some(id))
    }

    /// 利用者の案（新しい順）。
    pub fn profile_suggestions(&self, user_id: i64) -> Result<Vec<ProfileSuggestion>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, base_version_id, interests, excludes, reasons, evidence,
                    current_rated, current_concordance, candidate_rated, candidate_concordance,
                    trigger, status, created_at, decided_at
             FROM profile_suggestions WHERE user_id = ?1
             ORDER BY created_at DESC, id DESC",
        )?;
        let mut rows = stmt.query([user_id])?;
        let mut suggestions = Vec::new();
        while let Some(r) = rows.next()? {
            let stats = |rated: usize, concordance: usize| -> rusqlite::Result<VersionStats> {
                Ok(VersionStats {
                    rated: r.get::<_, i64>(rated)? as usize,
                    concordance: r.get(concordance)?,
                })
            };
            let trigger: String = r.get(10)?;
            let status: String = r.get(11)?;
            suggestions.push(ProfileSuggestion {
                id: r.get(0)?,
                base_version_id: r.get(1)?,
                profile: crate::profile::Profile {
                    interests: serde_json::from_str(&r.get::<_, String>(2)?)?,
                    exclude: serde_json::from_str(&r.get::<_, String>(3)?)?,
                },
                reasons: serde_json::from_str(&r.get::<_, String>(4)?)?,
                evidence: serde_json::from_str(&r.get::<_, String>(5)?)?,
                current: stats(6, 7)?,
                candidate: stats(8, 9)?,
                trigger: SuggestionTrigger::parse(&trigger).ok_or_else(|| {
                    DbError::UnexpectedValue(format!("suggestion trigger {trigger:?}"))
                })?,
                status: SuggestionStatus::parse(&status).ok_or_else(|| {
                    DbError::UnexpectedValue(format!("suggestion status {status:?}"))
                })?,
                created_at: r.get(12)?,
                decided_at: r.get(13)?,
            });
        }
        Ok(suggestions)
    }

    /// 待っている案を、`origin`（自動なら `Auto`、人が採用したなら `Suggest`）の版にする。案の根拠にした
    /// 記事は、その版の一致率から除く。待っている案でなければ何もせず `false`。利用者の案でなければ
    /// `DbError::UnknownProfileSuggestion`。
    pub fn apply_suggestion(
        &self,
        user_id: i64,
        suggestion_id: i64,
        origin: ProfileOrigin,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        use rusqlite::OptionalExtension;
        let tx = self.immediate()?;
        let (interests, excludes, evidence, status): (String, String, String, String) = tx
            .query_row(
                "SELECT interests, excludes, evidence, status FROM profile_suggestions
                 WHERE id = ?1 AND user_id = ?2",
                [suggestion_id, user_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or(DbError::UnknownProfileSuggestion(suggestion_id))?;
        if status != SuggestionStatus::Pending.as_str() {
            return Ok(false);
        }
        let profile = crate::profile::Profile {
            interests: serde_json::from_str(&interests)?,
            exclude: serde_json::from_str(&excludes)?,
        };
        let evidence: Vec<i64> = serde_json::from_str(&evidence)?;
        tx.execute(
            "UPDATE profile_suggestions SET status = 'applied', decided_at = ?2 WHERE id = ?1",
            rusqlite::params![suggestion_id, timestamp(now)],
        )?;
        save_version(&tx, user_id, &profile, origin, &evidence, now)?;
        tx.commit()?;
        Ok(true)
    }

    /// 前の案（無ければ今の版）の後に付けた評価の件数。案を作るかの判定に使う。
    pub fn ratings_since_review(&self, user_id: i64) -> Result<usize, DbError> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM ratings
             WHERE user_id = ?1
               AND rated_at > coalesce(
                 (SELECT max(at) FROM (
                    SELECT created_at AS at FROM profile_suggestions WHERE user_id = ?1
                    UNION ALL
                    SELECT created_at FROM profile_versions
                    WHERE user_id = ?1 AND retired_at IS NULL)),
                 '')",
            [user_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// 利用者の評価のうち、hash が `current` と `candidate` の両方のプロファイルの点数（一覧と同じ規則で
    /// 選ぶ）がそろった記事で測った、それぞれの一致率。
    pub fn paired_stats(
        &self,
        user_id: i64,
        current: &str,
        candidate: &str,
    ) -> Result<(VersionStats, VersionStats), DbError> {
        let score = |profile: &str| {
            list_score(
                "score",
                &latest_digest("id", "r.article_id", "?1"),
                "?1",
                profile,
            )
        };
        let sql = format!(
            "SELECT r.value, {current}, {candidate} FROM ratings AS r WHERE r.user_id = ?1",
            current = score("?2"),
            candidate = score("?3"),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![user_id, current, candidate], |r| {
            Ok((
                r.get::<_, Rating>(0)?,
                r.get::<_, Option<u8>>(1)?,
                r.get::<_, Option<u8>>(2)?,
            ))
        })?;
        let (mut current, mut candidate) = (Vec::new(), Vec::new());
        for row in rows {
            if let (rating, Some(a), Some(b)) = row? {
                current.push((a, rating));
                candidate.push((b, rating));
            }
        }
        let stats = |pairs: &[(u8, Rating)]| VersionStats {
            rated: pairs.len(),
            concordance: crate::eval::concordance(pairs),
        };
        Ok((stats(&current), stats(&candidate)))
    }

    /// 待っている案を見送る。待っている案でなければ何もせず `false`。利用者の案でなければ
    /// `DbError::UnknownProfileSuggestion`。
    pub fn dismiss_suggestion(
        &self,
        user_id: i64,
        suggestion_id: i64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        let n = self.conn.execute(
            "UPDATE profile_suggestions SET status = 'dismissed', decided_at = ?3
             WHERE id = ?1 AND user_id = ?2 AND status = 'pending'",
            rusqlite::params![suggestion_id, user_id, timestamp(now)],
        )?;
        if n > 0 {
            return Ok(true);
        }
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM profile_suggestions WHERE id = ?1 AND user_id = ?2)",
            [suggestion_id, user_id],
            |r| r.get(0),
        )?;
        if exists {
            Ok(false)
        } else {
            Err(DbError::UnknownProfileSuggestion(suggestion_id))
        }
    }

    /// 案を自動で当てるかを変える。
    pub fn set_auto_apply_profile(&self, user_id: i64, on: bool) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE users SET auto_apply_profile = ?2 WHERE id = ?1",
            rusqlite::params![user_id, on],
        )?;
        Ok(())
    }

    /// 案を今すぐ作るよう頼む（評価の件数によらず、次の `crawl --requests-only` か `crawl` で作る）。
    /// 頼んだまま案を作る前なら、頼んだ時刻を変えない。
    pub fn request_review(
        &self,
        user_id: i64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO profile_review_requests (user_id, requested_at) VALUES (?1, ?2)
             ON CONFLICT (user_id) DO NOTHING",
            rusqlite::params![user_id, timestamp(now)],
        )?;
        Ok(())
    }

    /// 案を頼んで、まだ作っていない利用者（頼んだ順）。
    pub fn review_requests(&self) -> Result<Vec<i64>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id FROM profile_review_requests ORDER BY requested_at, user_id",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 頼まれた案を作らずに終える（評価が無くて案を作れないとき）。
    pub fn drop_review_request(&self, user_id: i64) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM profile_review_requests WHERE user_id = ?1",
            [user_id],
        )?;
        Ok(())
    }

    /// 案を自動で当てるか（利用者の設定。既定は当てる）。
    pub fn auto_apply_profile(&self, user_id: i64) -> Result<bool, DbError> {
        Ok(self.conn.query_row(
            "SELECT auto_apply_profile FROM users WHERE id = ?1",
            [user_id],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;
    use crate::profile::{Interest, Profile};

    fn profile(topic: &str) -> Profile {
        Profile {
            interests: vec![Interest {
                topic: topic.into(),
                weight: 1.0,
                note: None,
            }],
            exclude: vec![],
        }
    }

    fn stats(rated: usize, concordance: f64) -> VersionStats {
        VersionStats {
            rated,
            concordance: Some(concordance),
        }
    }

    fn reasons() -> Vec<Reason> {
        vec![Reason {
            change: "b を足した".into(),
            evidence: "関心 3 件".into(),
        }]
    }

    /// テストの案の基にするプロファイル `profile("a")` の hash。
    static BASE: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| crate::profile::hash(&profile("a")));

    fn suggestion<'a>(
        user_id: i64,
        profile: &'a Profile,
        reasons: &'a [Reason],
        evidence: &'a [i64],
    ) -> NewSuggestion<'a> {
        NewSuggestion {
            user_id,
            base_hash: &BASE,
            profile,
            reasons,
            evidence,
            current: stats(10, 0.4),
            candidate: stats(10, 0.6),
            trigger: SuggestionTrigger::Auto,
            status: SuggestionStatus::Pending,
        }
    }

    /// 案は作ったときの今の版を基にして残し、新しい案ができたら待っている前の案を置き換える。
    #[test]
    fn saves_suggestions_and_supersedes_pending_ones() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let (b, c) = (profile("b"), profile("c"));
        let reasons = reasons();
        assert!(matches!(
            db.save_suggestion(
                &suggestion(owner, &b, &reasons, &[]),
                t("2026-10-01T00:00:00Z")
            ),
            Err(DbError::NoProfileVersion)
        ));
        db.save_profile(owner, &profile("a"), t("2026-10-01T00:00:00Z"))
            .unwrap();
        let base = db.profile_versions(owner).unwrap()[0].id;
        let first = db
            .save_suggestion(
                &suggestion(owner, &b, &reasons, &[3, 1]),
                t("2026-10-02T00:00:00Z"),
            )
            .unwrap()
            .unwrap();
        let saved = db.profile_suggestions(owner).unwrap();
        assert_eq!(
            saved,
            [ProfileSuggestion {
                id: first,
                base_version_id: base,
                profile: b.clone(),
                reasons: reasons.clone(),
                evidence: vec![3, 1],
                current: stats(10, 0.4),
                candidate: stats(10, 0.6),
                trigger: SuggestionTrigger::Auto,
                status: SuggestionStatus::Pending,
                created_at: "2026-10-02T00:00:00.000Z".into(),
                decided_at: None,
            }]
        );
        let second = db
            .save_suggestion(
                &NewSuggestion {
                    trigger: SuggestionTrigger::Manual,
                    ..suggestion(owner, &c, &reasons, &[])
                },
                t("2026-10-03T00:00:00Z"),
            )
            .unwrap()
            .unwrap();
        let saved = db.profile_suggestions(owner).unwrap();
        let state: Vec<(i64, SuggestionTrigger, SuggestionStatus, Option<&str>)> = saved
            .iter()
            .map(|s| (s.id, s.trigger, s.status, s.decided_at.as_deref()))
            .collect();
        assert_eq!(
            state,
            [
                (
                    second,
                    SuggestionTrigger::Manual,
                    SuggestionStatus::Pending,
                    None
                ),
                (
                    first,
                    SuggestionTrigger::Auto,
                    SuggestionStatus::Superseded,
                    Some("2026-10-03T00:00:00.000Z")
                ),
            ]
        );
    }

    /// 案の後にプロファイルが変わったら（取り込み・戻し）、待っている案は置き換わり、採用できない
    /// （案は前の版を基に作り、比べたので、新しい版を上書きしない）。
    #[test]
    fn a_new_version_supersedes_pending_suggestions() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.save_profile(owner, &profile("a"), t("2026-10-01T00:00:00Z"))
            .unwrap();
        let b = profile("b");
        let reasons = reasons();
        let id = db
            .save_suggestion(
                &suggestion(owner, &b, &reasons, &[]),
                t("2026-10-02T00:00:00Z"),
            )
            .unwrap()
            .unwrap();
        db.save_profile(owner, &profile("c"), t("2026-10-03T00:00:00Z"))
            .unwrap();
        let saved = &db.profile_suggestions(owner).unwrap()[0];
        assert_eq!(
            (saved.status, saved.decided_at.as_deref()),
            (
                SuggestionStatus::Superseded,
                Some("2026-10-03T00:00:00.000Z")
            )
        );
        assert!(
            !db.apply_suggestion(owner, id, ProfileOrigin::Suggest, t("2026-10-04T00:00:00Z"))
                .unwrap()
        );
        assert_eq!(db.load_profile(owner).unwrap().unwrap().0, profile("c"));
    }

    /// 案を作る間にプロファイルが変わっていたら（基にした hash が今の版と違う）、案を保存しない。
    #[test]
    fn skips_suggestions_made_from_an_old_profile() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.save_profile(owner, &profile("a"), t("2026-10-01T00:00:00Z"))
            .unwrap();
        db.save_profile(owner, &profile("c"), t("2026-10-02T00:00:00Z"))
            .unwrap();
        let b = profile("b");
        let reasons = reasons();
        assert_eq!(
            db.save_suggestion(
                &suggestion(owner, &b, &reasons, &[]),
                t("2026-10-03T00:00:00Z")
            )
            .unwrap(),
            None
        );
        assert!(db.profile_suggestions(owner).unwrap().is_empty());
    }

    /// 案を版にすると、根拠にした記事を持つ版ができ、案は採用済みになる。待っている案でなければ何もしない。
    #[test]
    fn applies_a_pending_suggestion() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        db.save_profile(owner, &profile("a"), t("2026-10-01T00:00:00Z"))
            .unwrap();
        let b = profile("b");
        let reasons = reasons();
        let id = db
            .save_suggestion(
                &suggestion(owner, &b, &reasons, &[7]),
                t("2026-10-02T00:00:00Z"),
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            db.apply_suggestion(other, id, ProfileOrigin::Suggest, t("2026-10-03T00:00:00Z")),
            Err(DbError::UnknownProfileSuggestion(i)) if i == id
        ));
        assert!(
            db.apply_suggestion(owner, id, ProfileOrigin::Auto, t("2026-10-03T00:00:00Z"))
                .unwrap()
        );
        let current = &db.profile_versions(owner).unwrap()[0];
        assert_eq!(
            (&current.profile, current.origin),
            (&b, ProfileOrigin::Auto)
        );
        assert_eq!(
            db.query_strings("SELECT evidence FROM profile_versions ORDER BY id DESC LIMIT 1")
                .unwrap(),
            ["[7]"]
        );
        let saved = &db.profile_suggestions(owner).unwrap()[0];
        assert_eq!(
            (saved.status, saved.decided_at.as_deref()),
            (SuggestionStatus::Applied, Some("2026-10-03T00:00:00.000Z"))
        );
        // 2 度目は何もしない
        assert!(
            !db.apply_suggestion(owner, id, ProfileOrigin::Auto, t("2026-10-04T00:00:00Z"))
                .unwrap()
        );
        assert_eq!(db.profile_versions(owner).unwrap().len(), 2);
    }

    fn rated(db: &Db, url: &str, hash: &str, score: u8, value: u8, at: &str) -> i64 {
        let article = page_article(db, url, "2026-09-30T00:00:00Z");
        let digest = add_digest(db, article, "sonnet", "題", true, "2026-09-30T01:00:00Z");
        db.insert_score(
            ScoreKey {
                profile_hash: hash,
                ..score_key(db)
            },
            digest,
            score,
            None,
            t("2026-09-30T02:00:00Z"),
        )
        .unwrap();
        db.rate(db.owner_id().unwrap(), article, Rating::new(value), t(at))
            .unwrap();
        article
    }

    /// 案を作るかは、前の案（無ければ今の版）の後に付けた評価の件数で決める。
    #[test]
    fn counts_ratings_since_the_last_review() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        assert_eq!(db.ratings_since_review(owner).unwrap(), 0);
        rated(&db, "https://e.com/old", "h", 50, 3, "2026-09-30T00:00:00Z");
        db.save_profile(owner, &profile("a"), t("2026-10-01T00:00:00Z"))
            .unwrap();
        rated(&db, "https://e.com/1", "h", 50, 3, "2026-10-01T01:00:00Z");
        rated(&db, "https://e.com/2", "h", 50, 3, "2026-10-01T02:00:00Z");
        assert_eq!(db.ratings_since_review(owner).unwrap(), 2);
        let b = profile("b");
        let reasons = reasons();
        db.save_suggestion(
            &suggestion(owner, &b, &reasons, &[]),
            t("2026-10-01T03:00:00Z"),
        )
        .unwrap();
        assert_eq!(db.ratings_since_review(owner).unwrap(), 0);
        rated(&db, "https://e.com/3", "h", 50, 3, "2026-10-01T04:00:00Z");
        assert_eq!(db.ratings_since_review(owner).unwrap(), 1);
    }

    /// 記事の最新の要約に、プロファイル `hash` の点数を足す。
    fn add_score(db: &Db, article: i64, hash: &str, score: u8) {
        let digest: i64 = db
            .conn()
            .query_row(
                "SELECT id FROM artifacts WHERE article_id = ?1 AND kind = 'digest'",
                [article],
                |r| r.get(0),
            )
            .unwrap();
        db.insert_score(
            ScoreKey {
                profile_hash: hash,
                ..score_key(db)
            },
            digest,
            score,
            None,
            t("2026-09-30T03:00:00Z"),
        )
        .unwrap();
    }

    /// 今と案は、評価した記事のうち両方の点数がそろった記事で、一覧と同じ規則で選んだ点数で比べる
    /// （片方の採点に失敗した記事で、比べる集合がずれないように）。
    #[test]
    fn compares_profiles_on_the_same_ratings() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let a = rated(&db, "https://e.com/a", "h", 80, 5, "2026-09-01T00:00:00Z");
        let b = rated(&db, "https://e.com/b", "h", 20, 1, "2026-10-01T00:00:00Z");
        // 案の採点に失敗した記事（今の点数だけがある）
        rated(&db, "https://e.com/c", "h", 90, 1, "2026-10-01T00:00:00Z");
        add_score(&db, a, "h2", 30);
        add_score(&db, b, "h2", 70);
        assert_eq!(
            db.paired_stats(owner, "h", "h2").unwrap(),
            (stats(2, 1.0), stats(2, 0.0))
        );
        let none = VersionStats {
            rated: 0,
            concordance: None,
        };
        assert_eq!(db.paired_stats(owner, "h", "none").unwrap(), (none, none));
        // 同じプロファイルどうしなら、点数のある評価すべて
        assert_eq!(
            db.paired_stats(owner, "h", "h").unwrap(),
            (stats(3, 0.5), stats(3, 0.5))
        );
    }

    /// 見送った案は版にならない。待っている案でなければ何もしない。
    #[test]
    fn dismisses_a_pending_suggestion() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        db.save_profile(owner, &profile("a"), t("2026-10-01T00:00:00Z"))
            .unwrap();
        let b = profile("b");
        let reasons = reasons();
        let id = db
            .save_suggestion(
                &suggestion(owner, &b, &reasons, &[]),
                t("2026-10-02T00:00:00Z"),
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            db.dismiss_suggestion(other, id, t("2026-10-03T00:00:00Z")),
            Err(DbError::UnknownProfileSuggestion(i)) if i == id
        ));
        assert!(
            db.dismiss_suggestion(owner, id, t("2026-10-03T00:00:00Z"))
                .unwrap()
        );
        assert!(
            !db.dismiss_suggestion(owner, id, t("2026-10-04T00:00:00Z"))
                .unwrap()
        );
        assert!(
            !db.apply_suggestion(owner, id, ProfileOrigin::Suggest, t("2026-10-04T00:00:00Z"))
                .unwrap()
        );
        let saved = &db.profile_suggestions(owner).unwrap()[0];
        assert_eq!(
            (saved.status, saved.decided_at.as_deref()),
            (
                SuggestionStatus::Dismissed,
                Some("2026-10-03T00:00:00.000Z")
            )
        );
        assert_eq!(db.load_profile(owner).unwrap().unwrap().0, profile("a"));
    }

    /// 頼んだ案は、手動の案を保存すると片付く（自動の案では片付かない）。評価が無ければ取り下げる。
    #[test]
    fn manual_suggestions_settle_requests() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        db.save_profile(owner, &profile("a"), t("2026-10-01T00:00:00Z"))
            .unwrap();
        db.request_review(other, t("2026-10-02T00:00:00Z")).unwrap();
        db.request_review(owner, t("2026-10-02T01:00:00Z")).unwrap();
        db.request_review(other, t("2026-10-02T02:00:00Z")).unwrap();
        assert_eq!(db.review_requests().unwrap(), [other, owner]);
        let b = profile("b");
        let reasons = reasons();
        db.save_suggestion(
            &suggestion(owner, &b, &reasons, &[]),
            t("2026-10-03T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(db.review_requests().unwrap(), [other, owner]);
        db.save_suggestion(
            &NewSuggestion {
                trigger: SuggestionTrigger::Manual,
                ..suggestion(owner, &b, &reasons, &[])
            },
            t("2026-10-03T01:00:00Z"),
        )
        .unwrap();
        assert_eq!(db.review_requests().unwrap(), [other]);
        db.drop_review_request(other).unwrap();
        assert!(db.review_requests().unwrap().is_empty());
    }

    #[test]
    fn switches_auto_apply() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.set_auto_apply_profile(owner, false).unwrap();
        assert!(!db.auto_apply_profile(owner).unwrap());
        db.set_auto_apply_profile(owner, true).unwrap();
        assert!(db.auto_apply_profile(owner).unwrap());
    }

    /// 案を自動で当てるかの既定は「当てる」。
    #[test]
    fn auto_apply_is_on_by_default() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.auto_apply_profile(db.owner_id().unwrap()).unwrap());
    }
}
