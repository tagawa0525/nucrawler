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

/// 取れた予約。drop すると外す（外せなければ期限で外れる）。外すのは自分の token の行だけ。
pub struct Claim<'a> {
    db: &'a Db,
    stage: String,
    backend: String,
    model: String,
    token: String,
    ids: Vec<i64>,
}

/// 予約ごとに違う値。プロセス・予約した時刻・プロセスの中の通し番号で、ほかの予約と重ならない。
fn new_token(now: chrono::DateTime<chrono::Utc>) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{}-{seq}", std::process::id(), timestamp(now))
}

impl Claim<'_> {
    /// 予約できた記事（依頼した順）
    pub fn ids(&self) -> &[i64] {
        &self.ids
    }

    /// 予約を `now + ttl` まで延長し、延長できた（まだ自分の予約である）記事を返す。期限が切れて
    /// ほかの実行に取り直された記事は延長できない。
    pub fn renew(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        ttl: chrono::Duration,
    ) -> Result<Vec<i64>, DbError> {
        let mut stmt = self.db.conn.prepare(
            "UPDATE work_claims SET expires_at = ?1
             WHERE article_id = ?2 AND stage = ?3 AND backend = ?4 AND model = ?5 AND token = ?6",
        )?;
        let expires_at = timestamp(now + ttl);
        let mut held = Vec::new();
        for &id in &self.ids {
            let changed = stmt.execute(rusqlite::params![
                expires_at,
                id,
                self.stage,
                self.backend,
                self.model,
                self.token
            ])?;
            if changed == 1 {
                held.push(id);
            }
        }
        Ok(held)
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
               AND stage = ?2 AND backend = ?3 AND model = ?4 AND token = ?5",
            rusqlite::params![ids, self.stage, self.backend, self.model, self.token],
        ) {
            tracing::warn!(stage = %self.stage, "failed to release work claims (they expire): {e}");
        }
    }
}

impl Db {
    /// `ids` の記事を `ttl` の間予約し、取れた記事だけを持つ予約を返す。ほかの実行が期限内の予約を
    /// 持つ記事は取れない。期限を過ぎた予約は取り直す（`now` は今の時刻）。
    pub fn claim(
        &self,
        key: ClaimKey,
        ids: &[i64],
        now: chrono::DateTime<chrono::Utc>,
        ttl: chrono::Duration,
    ) -> Result<Claim<'_>, DbError> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO work_claims (article_id, stage, backend, model, token, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (article_id, stage, backend, model) DO UPDATE
               SET token = excluded.token, expires_at = excluded.expires_at
               WHERE work_claims.expires_at <= ?7",
        )?;
        let token = new_token(now);
        let (expires_at, now) = (timestamp(now + ttl), timestamp(now));
        let mut claimed = Vec::new();
        for &id in ids {
            let changed = stmt.execute(rusqlite::params![
                id,
                key.stage,
                key.backend,
                key.model,
                token,
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
            token,
            ids: claimed,
        })
    }

    /// `select` で対象を選び、その記事を予約して、予約できたものだけを返す。選ぶ前に、今の時刻
    /// （`now`）で期限を過ぎた予約を消す。選ぶクエリは予約の有無だけを見るので、ステージの開始時の
    /// 古い時刻で選んでも、期限切れの予約で記事を取りこぼさない。選ぶのと予約するのを
    /// 1 つの書き込みトランザクション（IMMEDIATE）で行う。別々に行うと、選んでから予約するまでの間に
    /// ほかの実行がその記事を処理し終えて予約を外したとき、同じ記事をもう一度処理してしまう
    /// （処理する側は成果物を保存してから予約を外すので、トランザクションの中で選べば処理済みか予約中に見える）。
    pub fn claim_selected<T>(
        &self,
        key: ClaimKey,
        now: chrono::DateTime<chrono::Utc>,
        ttl: chrono::Duration,
        select: impl FnOnce(&Db) -> Result<Vec<T>, DbError>,
        article_id: impl Fn(&T) -> i64,
    ) -> Result<(Vec<T>, Claim<'_>), DbError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.conn.execute(
            "DELETE FROM work_claims WHERE expires_at <= ?1",
            [timestamp(now)],
        )?;
        let items = select(self)?;
        let ids: Vec<i64> = items.iter().map(&article_id).collect();
        let claim = self.claim(key, &ids, now, ttl)?;
        tx.commit()?;
        let items = items
            .into_iter()
            .filter(|item| claim.ids().contains(&article_id(item)))
            .collect();
        Ok((items, claim))
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

    /// 選んだ記事をそのまま予約する（選ぶのと予約するのを 1 つの書き込みトランザクションで行う）。
    #[test]
    fn claim_selected_claims_what_it_selects() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        let now = t("2026-09-27T00:00:00Z");
        let (items, held) = db
            .claim_selected(KEY, now, ttl(), |_| Ok(vec![a, b]), |&id| id)
            .unwrap();
        assert_eq!(items, [a, b]);
        assert_eq!(held.ids(), [a, b]);
        assert!(db.claim(KEY, &[a, b], now, ttl()).unwrap().ids().is_empty());
        drop(held);
        assert_eq!(db.claim(KEY, &[a, b], now, ttl()).unwrap().ids(), [a, b]);
    }

    /// 期限切れの予約を持っていた実行が後から外しても、取り直した実行の予約は残る。
    #[test]
    fn releasing_an_expired_claim_keeps_the_successor() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let stale = db
            .claim(KEY, &[a], t("2026-09-27T00:00:00Z"), ttl())
            .unwrap();
        let later = t("2026-09-27T00:10:00Z");
        let successor = db.claim(KEY, &[a], later, ttl()).unwrap();
        assert_eq!(successor.ids(), [a]);
        drop(stale);
        assert!(db.claim(KEY, &[a], later, ttl()).unwrap().ids().is_empty());
        drop(successor);
        assert_eq!(db.claim(KEY, &[a], later, ttl()).unwrap().ids(), [a]);
    }

    /// 選ぶ前に、今の時刻で期限を過ぎた予約を消す。選ぶクエリが古い時刻（ステージの開始時）を
    /// 使っていても、期限切れの予約で記事を取りこぼさない。
    #[test]
    fn claim_selected_drops_expired_claims_before_selecting() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let stage_start = t("2026-09-27T00:00:00Z");
        let title = ClaimKey {
            stage: "title",
            ..KEY
        };
        std::mem::forget(db.claim(title, &[a], stage_start, ttl()).unwrap());
        let (items, _held) = db
            .claim_selected(
                title,
                t("2026-09-27T00:10:00Z"),
                ttl(),
                |db| db.pending_titles(stage_start, "claude-cli", "sonnet", 10),
                |i| i.article_id,
            )
            .unwrap();
        assert_eq!(items.iter().map(|i| i.article_id).collect::<Vec<_>>(), [a]);
    }

    /// 延長できるのは自分の予約だけ。期限が切れてほかの実行に取り直された記事は、延長できない
    /// （その結果は保存しない）。
    #[test]
    fn renew_keeps_only_claims_still_held() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-26T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-26T00:00:00.000Z");
        let held = db
            .claim(KEY, &[a, b], t("2026-09-27T00:00:00Z"), ttl())
            .unwrap();
        let later = t("2026-09-27T00:10:00Z");
        let _successor = db.claim(KEY, &[b], later, ttl()).unwrap();
        assert_eq!(held.renew(later, ttl()).unwrap(), [a]);
        // 延長した予約は、延長した時刻から期限まで取られない
        let still = t("2026-09-27T00:19:59Z");
        assert!(db.claim(KEY, &[a], still, ttl()).unwrap().ids().is_empty());
    }
}
