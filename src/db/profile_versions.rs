//! プロファイルの版（計画 016）。保存するたびに版を追記し、どの版にも戻せるようにする。

use super::*;

/// 版の出どころ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileOrigin {
    /// `profile import`（CLI）
    Import,
    /// 更新案を人が採用した
    Suggest,
    /// 更新案を自動で適用した
    Auto,
    /// 前の版に戻した
    Revert,
}

impl ProfileOrigin {
    /// DB での値（`profile_versions.origin`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Suggest => "suggest",
            Self::Auto => "auto",
            Self::Revert => "revert",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Import, Self::Suggest, Self::Auto, Self::Revert]
            .into_iter()
            .find(|o| o.as_str() == value)
    }
}

/// 版が今のプロファイルだった間に付けた評価での、一覧と同じ規則で選んだ点数の一致率。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VersionStats {
    /// 数えた評価の件数（その版の点数が付いた記事だけ）
    pub rated: usize,
    /// 一致率（評価の違う組が無ければ `None`）
    pub concordance: Option<f64>,
}

/// プロファイルの版。
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileVersion {
    pub id: i64,
    pub profile: crate::profile::Profile,
    pub hash: String,
    pub origin: ProfileOrigin,
    /// 今のプロファイルになった時刻
    pub created_at: String,
    /// 今のプロファイルでなくなった時刻（今の版なら `None`）
    pub retired_at: Option<String>,
    pub stats: VersionStats,
}

impl Db {
    /// プロファイルを新しい版として保存し、今のプロファイルにする。`evidence` は、案から作った版なら
    /// その根拠にした記事（その版の一致率から除く）。今のプロファイルと同じ中身なら何もせず `false`。
    pub fn save_profile_version(
        &self,
        user_id: i64,
        profile: &crate::profile::Profile,
        origin: ProfileOrigin,
        evidence: &[i64],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        // 今の版を読んでから退かせるので、読む前に書き込みのロックを取る（Web と CLI が同時に保存しても、
        // 古い版を読んだ側が書けずに失敗しないように）
        let tx = self.immediate()?;
        let saved = save_version(&tx, user_id, profile, origin, evidence, now)?;
        tx.commit()?;
        Ok(saved)
    }

    /// 利用者のプロファイルの版（新しい順）。
    pub fn profile_versions(&self, user_id: i64) -> Result<Vec<ProfileVersion>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, interests, excludes, hash, origin, evidence, created_at, retired_at,
                    rated, concordance
             FROM profile_versions WHERE user_id = ?1
             ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([user_id], |r| {
            Ok((
                Period {
                    id: r.get(0)?,
                    hash: r.get(3)?,
                    created_at: r.get(6)?,
                    retired_at: r.get(7)?,
                    evidence: r.get(5)?,
                },
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<f64>>(9)?,
            ))
        })?;
        let mut versions = Vec::new();
        for row in rows {
            let (period, interests, excludes, origin, rated, concordance) = row?;
            let stats = match rated {
                Some(rated) => VersionStats {
                    rated: rated as usize,
                    concordance,
                },
                None => version_stats(&self.conn, user_id, &period)?,
            };
            versions.push(ProfileVersion {
                id: period.id,
                profile: crate::profile::Profile {
                    interests: serde_json::from_str(&interests)?,
                    exclude: serde_json::from_str(&excludes)?,
                },
                hash: period.hash,
                origin: ProfileOrigin::parse(&origin).ok_or_else(|| {
                    DbError::UnexpectedValue(format!("profile origin {origin:?}"))
                })?,
                created_at: period.created_at,
                retired_at: period.retired_at,
                stats,
            });
        }
        Ok(versions)
    }

    /// 版 `version_id` の中身を新しい版として保存し、今のプロファイルにする。今のプロファイルと同じ中身なら
    /// 何もせず `false`。利用者の版でなければ `DbError::UnknownProfileVersion`。
    pub fn revert_profile(
        &self,
        user_id: i64,
        version_id: i64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        use rusqlite::OptionalExtension;
        let (interests, excludes): (String, String) = self
            .conn
            .query_row(
                "SELECT interests, excludes FROM profile_versions WHERE id = ?1 AND user_id = ?2",
                [version_id, user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(DbError::UnknownProfileVersion(version_id))?;
        let profile = crate::profile::Profile {
            interests: serde_json::from_str(&interests)?,
            exclude: serde_json::from_str(&excludes)?,
        };
        self.save_profile_version(user_id, &profile, ProfileOrigin::Revert, &[], now)
    }
}

/// 版を追記して今のプロファイルにする（`Db::save_profile_version` の中身。呼び出し側の取引の中で行う）。
pub(super) fn save_version(
    tx: &rusqlite::Connection,
    user_id: i64,
    profile: &crate::profile::Profile,
    origin: ProfileOrigin,
    evidence: &[i64],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool, DbError> {
    use rusqlite::OptionalExtension;
    let hash = crate::profile::hash(profile);
    let now = timestamp(now);
    let current: Option<Period> = tx
        .query_row(
            "SELECT id, hash, created_at, evidence FROM profile_versions
             WHERE user_id = ?1 AND retired_at IS NULL",
            [user_id],
            |r| {
                Ok(Period {
                    id: r.get(0)?,
                    hash: r.get(1)?,
                    created_at: r.get(2)?,
                    retired_at: None,
                    evidence: r.get(3)?,
                })
            },
        )
        .optional()?;
    // 時刻は、ロックを取る前に読んだもの。先に読んだ側が後から書くこともあるので、今の版より前には戻さない
    // （版の並びと、版が今だった期間が逆にならないように）
    let mut now = now;
    if let Some(current) = &current
        && current.created_at > now
    {
        now = current.created_at.clone();
    }
    if let Some(mut current) = current {
        if current.hash == hash {
            return Ok(false);
        }
        // 退く時点の一致率で固める（後で評価を付け直したり、点数が作り直されたりしても動かさない）
        current.retired_at = Some(now.clone());
        let stats = version_stats(tx, user_id, &current)?;
        tx.execute(
            "UPDATE profile_versions SET retired_at = ?2, rated = ?3, concordance = ?4
             WHERE id = ?1",
            rusqlite::params![current.id, now, stats.rated as i64, stats.concordance],
        )?;
    }
    // 待っている案は前の版を基に作って比べたものなので、新しい版を上書きしないよう置き換える
    // （案を採用したときは、その案を先に採用済みにしてから呼ぶ）
    tx.execute(
        "UPDATE profile_suggestions SET status = 'superseded', decided_at = ?2
         WHERE user_id = ?1 AND status = 'pending'",
        rusqlite::params![user_id, now],
    )?;
    let interests = serde_json::to_string(&profile.interests)?;
    let excludes = serde_json::to_string(&profile.exclude)?;
    tx.execute(
        "INSERT INTO profile_versions
           (user_id, interests, excludes, hash, origin, evidence, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            user_id,
            interests,
            excludes,
            hash,
            origin.as_str(),
            serde_json::to_string(evidence)?,
            now,
        ],
    )?;
    tx.execute(
        "INSERT INTO profiles (user_id, interests, excludes, hash, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (user_id) DO UPDATE SET
           interests = excluded.interests,
           excludes = excluded.excludes,
           hash = excluded.hash,
           updated_at = excluded.updated_at",
        rusqlite::params![user_id, interests, excludes, hash, now],
    )?;
    Ok(true)
}

/// 版が今のプロファイルだった期間と、一致率の集計に要るもの。
pub(super) struct Period {
    pub(super) id: i64,
    pub(super) hash: String,
    pub(super) created_at: String,
    /// 今の版なら `None`
    pub(super) retired_at: Option<String>,
    /// 根拠にした記事の id（JSON 配列）
    pub(super) evidence: String,
}

/// 版の期間に付けた評価（根拠にした記事を除く）を、一覧と同じ規則で選んだその版の点数で測った一致率。
/// 点数の付いていない記事は数えない。
pub(super) fn version_stats(
    conn: &rusqlite::Connection,
    user_id: i64,
    period: &Period,
) -> Result<VersionStats, DbError> {
    let sql = format!(
        "SELECT r.value, {score} FROM ratings AS r
         WHERE r.user_id = ?1 AND r.rated_at >= ?3 AND (?4 IS NULL OR r.rated_at < ?4)
           AND r.article_id NOT IN (SELECT value FROM json_each(?5))",
        score = list_score(
            "score",
            &latest_digest("id", "r.article_id", "?1"),
            "?1",
            "?2"
        ),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![
            user_id,
            period.hash,
            period.created_at,
            period.retired_at,
            period.evidence,
        ],
        |r| Ok((r.get::<_, Rating>(0)?, r.get::<_, Option<u8>>(1)?)),
    )?;
    let mut pairs = Vec::new();
    for row in rows {
        if let (rating, Some(score)) = row? {
            pairs.push((score, rating));
        }
    }
    Ok(VersionStats {
        rated: pairs.len(),
        concordance: crate::eval::concordance(&pairs),
    })
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

    fn origins(db: &Db, user: i64) -> Vec<(String, ProfileOrigin, bool)> {
        db.profile_versions(user)
            .unwrap()
            .into_iter()
            .map(|v| {
                (
                    v.profile.interests[0].topic.clone(),
                    v.origin,
                    v.retired_at.is_none(),
                )
            })
            .collect()
    }

    /// 保存するたびに版を追記し、今の版だけが退いていない。同じ中身の保存は版を増やさない。
    #[test]
    fn saving_appends_versions() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        assert!(db.profile_versions(owner).unwrap().is_empty());
        assert!(
            db.save_profile_version(
                owner,
                &profile("a"),
                ProfileOrigin::Import,
                &[],
                t("2026-10-01T00:00:00Z")
            )
            .unwrap()
        );
        assert!(
            !db.save_profile_version(
                owner,
                &profile("a"),
                ProfileOrigin::Import,
                &[],
                t("2026-10-02T00:00:00Z")
            )
            .unwrap()
        );
        assert!(
            db.save_profile_version(
                owner,
                &profile("b"),
                ProfileOrigin::Auto,
                &[],
                t("2026-10-03T00:00:00Z")
            )
            .unwrap()
        );
        assert_eq!(
            origins(&db, owner),
            [
                ("b".into(), ProfileOrigin::Auto, true),
                ("a".into(), ProfileOrigin::Import, false),
            ]
        );
        let versions = db.profile_versions(owner).unwrap();
        assert_eq!(
            versions[1].retired_at.as_deref(),
            Some("2026-10-03T00:00:00.000Z")
        );
        assert_eq!(versions[0].hash, crate::profile::hash(&profile("b")));
        let (current, hash) = db.load_profile(owner).unwrap().unwrap();
        assert_eq!((current, hash), (profile("b"), versions[0].hash.clone()));
        // `save_profile`（CLI の取り込みなど）も版を追記する
        db.save_profile(owner, &profile("c"), t("2026-10-04T00:00:00Z"))
            .unwrap();
        assert_eq!(
            origins(&db, owner)[0],
            ("c".into(), ProfileOrigin::Import, true)
        );
    }

    /// 保存の時刻が今の版より前でも（同時に保存して、先に時刻を読んだ側が後から書く）、時刻を今の版に
    /// そろえて新しい版を後ろに置く。版が退く時刻が作られた時刻より前にならない。
    #[test]
    fn keeps_versions_in_order_when_saves_race() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        db.save_profile(owner, &profile("a"), t("2026-10-02T00:00:00Z"))
            .unwrap();
        db.save_profile(owner, &profile("b"), t("2026-10-01T00:00:00Z"))
            .unwrap();
        let versions = db.profile_versions(owner).unwrap();
        assert_eq!(
            origins(&db, owner),
            [
                ("b".into(), ProfileOrigin::Import, true),
                ("a".into(), ProfileOrigin::Import, false),
            ]
        );
        assert_eq!(versions[0].created_at, "2026-10-02T00:00:00.000Z");
        assert_eq!(
            versions[1].retired_at.as_deref(),
            Some("2026-10-02T00:00:00.000Z")
        );
    }

    /// 戻すと、その版の中身が新しい版になる。履歴は一方向に伸び、戻したことも残る。
    #[test]
    fn reverting_saves_the_old_content_as_a_new_version() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let other = db.add_user("o@example.com", "O", "h").unwrap();
        for (topic, at) in [("a", "2026-10-01T00:00:00Z"), ("b", "2026-10-02T00:00:00Z")] {
            db.save_profile(owner, &profile(topic), t(at)).unwrap();
        }
        let first = db.profile_versions(owner).unwrap()[1].id;
        assert!(
            db.revert_profile(owner, first, t("2026-10-03T00:00:00Z"))
                .unwrap()
        );
        assert_eq!(
            origins(&db, owner),
            [
                ("a".into(), ProfileOrigin::Revert, true),
                ("b".into(), ProfileOrigin::Import, false),
                ("a".into(), ProfileOrigin::Import, false),
            ]
        );
        assert_eq!(db.load_profile(owner).unwrap().unwrap().0, profile("a"));
        // 今と同じ中身に戻しても版を増やさない
        assert!(
            !db.revert_profile(owner, first, t("2026-10-04T00:00:00Z"))
                .unwrap()
        );
        assert_eq!(db.profile_versions(owner).unwrap().len(), 3);
        // ほかの利用者の版や、無い版には戻せない
        assert!(matches!(
            db.revert_profile(other, first, t("2026-10-04T00:00:00Z")),
            Err(DbError::UnknownProfileVersion(id)) if id == first
        ));
        assert!(matches!(
            db.revert_profile(owner, 999, t("2026-10-04T00:00:00Z")),
            Err(DbError::UnknownProfileVersion(999))
        ));
    }

    /// 要約を付けた記事に、プロファイル `hash` の点数を付ける。
    fn scored(db: &Db, url: &str, hash: &str, score: u8) -> i64 {
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
        article
    }

    fn rate(db: &Db, article: i64, value: u8, at: &str) {
        db.rate(db.owner_id().unwrap(), article, Rating::new(value), t(at))
            .unwrap();
    }

    /// 版の一致率は、その版が今だった間に付けた評価を、一覧と同じ規則で選んだその版の点数で測る。
    /// 案の根拠にした記事は除き、版が退いた時点の値で固める（後で評価を付け直しても動かない）。
    #[test]
    fn stats_use_ratings_given_while_the_version_was_current() {
        let db = Db::open_in_memory().unwrap();
        let owner = db.owner_id().unwrap();
        let (a, b) = (profile("a"), profile("b"));
        let (ha, hb) = (crate::profile::hash(&a), crate::profile::hash(&b));
        db.save_profile(owner, &a, t("2026-10-01T00:00:00Z"))
            .unwrap();
        let liked = scored(&db, "https://e.com/liked", &ha, 80);
        let disliked = scored(&db, "https://e.com/disliked", &ha, 20);
        let unscored = page_article(&db, "https://e.com/unscored", "2026-09-30T00:00:00Z");
        rate(&db, liked, 4, "2026-10-01T01:00:00Z");
        rate(&db, disliked, 2, "2026-10-01T02:00:00Z");
        rate(&db, unscored, 5, "2026-10-01T03:00:00Z");
        // 版 a の間：点数の付いた 2 件で、評価の高い方が点数も高い
        let stats = |db: &Db| -> Vec<VersionStats> {
            db.profile_versions(owner)
                .unwrap()
                .into_iter()
                .map(|v| v.stats)
                .collect()
        };
        assert_eq!(
            stats(&db),
            [VersionStats {
                rated: 2,
                concordance: Some(1.0)
            }]
        );
        // 版 b は、評価済みの liked・disliked を根拠にした案から作った
        db.save_profile_version(
            owner,
            &b,
            ProfileOrigin::Suggest,
            &[liked, disliked],
            t("2026-10-02T00:00:00Z"),
        )
        .unwrap();
        for (article, score) in [(liked, 10), (disliked, 90)] {
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
                    profile_hash: &hb,
                    ..score_key(&db)
                },
                digest,
                score,
                None,
                t("2026-10-02T01:00:00Z"),
            )
            .unwrap();
        }
        let fresh = scored(&db, "https://e.com/fresh", &hb, 70);
        let fresh_low = scored(&db, "https://e.com/fresh-low", &hb, 60);
        rate(&db, fresh, 3, "2026-10-02T02:00:00Z");
        rate(&db, fresh_low, 1, "2026-10-02T03:00:00Z");
        // 付け直した評価は付け直した時点の版に数えるが、根拠にした記事は版 b から除く。
        // 版 a の値は退いた時点で固まっている
        rate(&db, liked, 1, "2026-10-02T04:00:00Z");
        assert_eq!(
            stats(&db),
            [
                VersionStats {
                    rated: 2,
                    concordance: Some(1.0)
                },
                VersionStats {
                    rated: 2,
                    concordance: Some(1.0)
                },
            ]
        );
        // 根拠にしていない記事は、付け直せば今の版で数える
        rate(&db, fresh_low, 4, "2026-10-02T05:00:00Z");
        assert_eq!(
            stats(&db)[0],
            VersionStats {
                rated: 2,
                concordance: Some(0.0)
            }
        );
    }
}
