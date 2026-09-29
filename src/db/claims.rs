//! 作業の予約。LLM を呼ぶ処理が同じ記事を同時に処理しないよう、対象を選んだら予約してから処理し、
//! 終われば外す。プロセスが落ちても、期限を過ぎた予約はほかの実行が取り直せる。

use super::*;

/// 予約のキー（記事を除く）。`stage_errors` と同じく、ステージ・backend・model ごとに分ける。
#[derive(Debug, Clone, Copy)]
pub struct ClaimKey<'a> {
    pub stage: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
}

/// 取れた予約。drop すると外す（外せなければ期限で外れる）。
pub struct Claim<'a> {
    db: &'a Db,
    stage: String,
    backend: String,
    model: String,
    ids: Vec<i64>,
}

impl Claim<'_> {
    /// 予約できた記事（依頼した順）
    pub fn ids(&self) -> &[i64] {
        &self.ids
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if self.ids.is_empty() {
            return;
        }
        let ids = serde_json::to_string(&self.ids).expect("ids serialize");
        if let Err(e) = self.db.conn.execute(
            "DELETE FROM work_claims
             WHERE article_id IN (SELECT value FROM json_each(?1))
               AND stage = ?2 AND backend = ?3 AND model = ?4",
            rusqlite::params![ids, self.stage, self.backend, self.model],
        ) {
            tracing::warn!(stage = %self.stage, "failed to release work claims (they expire): {e}");
        }
    }
}

impl Db {
    /// `ids` の記事を `ttl` の間予約し、取れた記事だけを持つ予約を返す。ほかの実行が期限内の予約を
    /// 持つ記事は取れない。期限を過ぎた予約は取り直す。
    pub fn claim(
        &self,
        key: ClaimKey,
        ids: &[i64],
        now: chrono::DateTime<chrono::Utc>,
        ttl: chrono::Duration,
    ) -> Result<Claim<'_>, DbError> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO work_claims (article_id, stage, backend, model, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (article_id, stage, backend, model) DO UPDATE
               SET expires_at = excluded.expires_at
               WHERE work_claims.expires_at <= ?6",
        )?;
        let (expires_at, now) = (timestamp(now + ttl), timestamp(now));
        let mut claimed = Vec::new();
        for &id in ids {
            let changed = stmt.execute(rusqlite::params![
                id,
                key.stage,
                key.backend,
                key.model,
                expires_at,
                now
            ])?;
            if changed == 1 {
                claimed.push(id);
            }
        }
        Ok(Claim {
            db: self,
            stage: key.stage.to_string(),
            backend: key.backend.to_string(),
            model: key.model.to_string(),
            ids: claimed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    const KEY: ClaimKey = ClaimKey {
        stage: "digest",
        backend: "claude-cli",
        model: "sonnet",
    };

    fn ttl() -> chrono::Duration {
        chrono::Duration::minutes(10)
    }

    #[test]
    fn claims_only_unclaimed_articles_and_releases_on_drop() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        let c = page_article(&db, "https://e.com/c", "2026-09-26T00:00:00.000Z");
        let now = t("2026-09-27T00:00:00Z");
        let first = db.claim(KEY, &[a, b], now, ttl()).unwrap();
        assert_eq!(first.ids(), [a, b]);
        let second = db.claim(KEY, &[b, c], now, ttl()).unwrap();
        assert_eq!(second.ids(), [c]);
        drop(first);
        assert_eq!(db.claim(KEY, &[a, b], now, ttl()).unwrap().ids(), [a, b]);
        drop(second);
        assert_eq!(db.query_i64("SELECT count(*) FROM work_claims").unwrap(), 0);
    }

    /// 期限を過ぎた予約（落ちたプロセスのもの）は取り直せる。
    #[test]
    fn expired_claims_can_be_taken_over() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let stale = db
            .claim(KEY, &[a], t("2026-09-27T00:00:00Z"), ttl())
            .unwrap();
        std::mem::forget(stale);
        let still = db
            .claim(KEY, &[a], t("2026-09-27T00:09:59Z"), ttl())
            .unwrap();
        assert!(still.ids().is_empty());
        let later = db
            .claim(KEY, &[a], t("2026-09-27T00:10:00Z"), ttl())
            .unwrap();
        assert_eq!(later.ids(), [a]);
    }

    /// 予約はステージ・モデルごと（同じ記事でも別のモデルなら別の成果物になる）。
    #[test]
    fn claims_are_per_stage_and_model() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let now = t("2026-09-27T00:00:00Z");
        let _held = db.claim(KEY, &[a], now, ttl()).unwrap();
        for other in [
            ClaimKey {
                model: "opus",
                ..KEY
            },
            ClaimKey {
                stage: "translate",
                ..KEY
            },
        ] {
            assert_eq!(db.claim(other, &[a], now, ttl()).unwrap().ids(), [a]);
        }
    }

    #[test]
    fn claims_disappear_with_the_article() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let held = db
            .claim(KEY, &[a], t("2026-09-27T00:00:00Z"), ttl())
            .unwrap();
        db.conn()
            .execute("DELETE FROM articles WHERE id = ?1", [a])
            .unwrap();
        drop(held);
        assert_eq!(db.query_i64("SELECT count(*) FROM work_claims").unwrap(), 0);
    }
}
