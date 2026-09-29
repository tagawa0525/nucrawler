//! 作業の予約。LLM を呼ぶ処理が同じ記事を同時に処理しないよう、対象を選んだら予約してから処理し、
//! 終われば外す。プロセスが落ちても、期限を過ぎた予約はほかの実行が取り直せる。

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
