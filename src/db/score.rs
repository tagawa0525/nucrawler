//! 採点のキーと、点数の登録（点数は embedding のステージが `embed_scores` で保存する。ここの登録はテスト用）。

#[cfg(test)]
use super::*;

/// 採点の対象を特定するキー（誰の・どのプロファイルで・どのモデルと版の式またはプロンプトで）。
#[derive(Debug, Clone, Copy)]
pub struct ScoreKey<'a> {
    pub user_id: i64,
    pub profile_hash: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
    pub prompt_version: i64,
}

/// 採点が当たったプロファイルの語。
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ScoreMatches<'a> {
    /// 当たった関心分野（interest の topic）
    pub interests: &'a [String],
    /// 当たった推薦しない話題（exclude）
    pub excludes: &'a [String],
}

#[cfg(test)]
impl Db {
    /// 当たった語の無い採点を登録する（`insert_score_with_matches`）。
    pub fn insert_score(
        &self,
        key: ScoreKey,
        artifact_id: i64,
        score: u8,
        reason: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        self.insert_score_with_matches(
            key,
            artifact_id,
            score,
            reason,
            ScoreMatches::default(),
            now,
        )
    }

    /// 採点と、当たった関心分野・推薦しない話題を同じトランザクションで登録する。
    pub fn insert_score_with_matches(
        &self,
        key: ScoreKey,
        artifact_id: i64,
        score: u8,
        reason: Option<&str>,
        matches: ScoreMatches<'_>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO scores
               (user_id, artifact_id, profile_hash, backend, model, prompt_version, score, reason,
                created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                key.user_id,
                artifact_id,
                key.profile_hash,
                key.backend,
                key.model,
                key.prompt_version,
                score,
                reason,
                timestamp(now),
            ],
        )?;
        let score_id = tx.last_insert_rowid();
        for (kind, topics) in [
            ("interest", matches.interests),
            ("exclude", matches.excludes),
        ] {
            for topic in topics {
                tx.execute(
                    "INSERT INTO score_matches (score_id, kind, topic) VALUES (?1, ?2, ?3)",
                    rusqlite::params![score_id, kind, topic],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    #[test]
    fn stores_matches_with_the_score() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let d = add_digest(&db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        db.insert_score_with_matches(
            key,
            d,
            80,
            Some("r"),
            ScoreMatches {
                interests: &["規制・審査".into(), "燃料".into()],
                excludes: &["核融合".into()],
            },
            t("2026-09-27T00:00:00Z"),
        )
        .unwrap();
        let rows = || {
            db.query_strings("SELECT kind || ':' || topic FROM score_matches ORDER BY kind, topic")
                .unwrap()
        };
        assert_eq!(
            rows(),
            ["exclude:核融合", "interest:燃料", "interest:規制・審査"]
        );
        // 採点が消えれば当たった語も消える
        db.conn().execute("DELETE FROM scores", []).unwrap();
        assert!(rows().is_empty());
    }

    #[test]
    fn score_is_limited_to_0_through_100() {
        let db = Db::open_in_memory().unwrap();
        let key = score_key(&db);
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let d = add_digest(&db, a, "sonnet", "題", true, "2026-09-26T01:00:00Z");
        assert!(
            db.insert_score(key, d, 101, None, t("2026-09-27T00:00:00Z"))
                .is_err()
        );
    }
}
