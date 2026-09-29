//! 段階ごとの失敗と再試行、LLM の呼び出しの記録。

use super::*;

/// `attempts` 回目の失敗の後に待つ時間：1 時間から倍々で、最大 7 日。
fn backoff(attempts: i64) -> chrono::Duration {
    let hours = 1i64 << (attempts - 1).clamp(0, 16);
    chrono::Duration::hours(hours).min(chrono::Duration::days(7))
}

/// 一時的な失敗の再試行は `MAX_ATTEMPTS` 回まで。間隔は 1 時間から倍々で、最大 7 日。
pub const MAX_ATTEMPTS: i64 = 5;

/// `stage_errors` の行を特定するキー。LLM を使わないステージは backend と model を "" にする。
#[derive(Debug, Clone, Copy)]
pub struct StageKey<'a> {
    pub article_id: i64,
    pub stage: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
}

/// `llm_calls` に記録する 1 回の呼び出し。
#[derive(Debug)]
pub struct LlmCall<'a> {
    pub stage: &'a str,
    pub backend: &'a str,
    pub model: &'a str,
    pub n_items: usize,
    pub ok: bool,
    pub duration_ms: u64,
    pub error: Option<&'a str>,
    pub usage: Option<&'a crate::llm::Usage>,
}

impl Db {
    /// 失敗を記録する。`permanent` なら再試行しない（試行回数を上限にする）。
    /// そうでなければ試行回数を 1 増やし、次に試してよい時刻を指数的に先へ延ばす。
    /// 以後は再試行しない（断念した）なら `true` を返す。
    pub fn record_stage_failure(
        &self,
        key: StageKey,
        error: &str,
        now: chrono::DateTime<chrono::Utc>,
        permanent: bool,
    ) -> Result<bool, DbError> {
        use rusqlite::OptionalExtension;
        let tx = self.conn.unchecked_transaction()?;
        let previous: i64 = tx
            .query_row(
                "SELECT attempts FROM stage_errors
                 WHERE article_id = ?1 AND stage = ?2 AND backend = ?3 AND model = ?4",
                rusqlite::params![key.article_id, key.stage, key.backend, key.model],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let attempts = if permanent {
            MAX_ATTEMPTS
        } else {
            (previous + 1).min(MAX_ATTEMPTS)
        };
        tx.execute(
            "INSERT INTO stage_errors
               (article_id, stage, backend, model, attempts, last_error, next_retry_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (article_id, stage, backend, model) DO UPDATE SET
               attempts = excluded.attempts,
               last_error = excluded.last_error,
               next_retry_at = excluded.next_retry_at",
            rusqlite::params![
                key.article_id,
                key.stage,
                key.backend,
                key.model,
                attempts,
                error,
                timestamp(now + backoff(attempts)),
            ],
        )?;
        tx.commit()?;
        Ok(attempts >= MAX_ATTEMPTS)
    }

    /// 成功したら失敗の記録を消す。
    pub fn clear_stage_failure(&self, key: StageKey) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM stage_errors
             WHERE article_id = ?1 AND stage = ?2 AND backend = ?3 AND model = ?4",
            rusqlite::params![key.article_id, key.stage, key.backend, key.model],
        )?;
        Ok(())
    }

    /// 本文（body か fulltext）が無く、`cutoff` 以降に公開（無ければ取得）され、再試行待ちでも
    /// 断念済みでもない記事を、新しい順に最大 `limit` 件返す。
    pub fn pending_extract(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<PendingPage>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.source_id, a.url FROM articles AS a
             WHERE coalesce(a.published_at, a.fetched_at) >= ?1
               AND NOT EXISTS (
                 SELECT 1 FROM contents AS c
                 WHERE c.article_id = a.id AND c.kind IN ('body', 'fulltext'))
               AND NOT EXISTS (
                 SELECT 1 FROM stage_errors AS e
                 WHERE e.article_id = a.id AND e.stage = 'extract'
                   AND e.backend = '' AND e.model = ''
                   AND (e.attempts >= ?2 OR e.next_retry_at > ?3))
             ORDER BY coalesce(a.published_at, a.fetched_at) DESC, a.id DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                timestamp(cutoff),
                MAX_ATTEMPTS,
                timestamp(now),
                // 負の LIMIT は SQLite では無制限になるので、桁あふれさせずに丸める
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            |r| {
                Ok(PendingPage {
                    article_id: r.get(0)?,
                    source_id: r.get(1)?,
                    url: r.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn record_llm_call(
        &self,
        call: &LlmCall,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DbError> {
        let rate_limit = call
            .usage
            .and_then(crate::llm::Usage::rate_limit)
            .map(|r| serde_json::to_string(&r))
            .transpose()?;
        self.conn.execute(
            "INSERT INTO llm_calls
               (at, stage, backend, model, n_items, ok, duration_ms, error, rate_limit)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                timestamp(at),
                call.stage,
                call.backend,
                call.model,
                i64::try_from(call.n_items).unwrap_or(i64::MAX),
                call.ok,
                i64::try_from(call.duration_ms).unwrap_or(i64::MAX),
                call.error,
                rate_limit,
            ],
        )?;
        Ok(())
    }

    /// 最後に記録された使用率（無ければ `None`）。
    /// ほかの実行を含めた最新の使用率。呼び出しは並行して終わる順が前後するので、最後の行ではなく、
    /// 枠ごとにリセット時刻が最も新しい枠の最も高い使用率を使う（`RateLimit::merge`）。週次枠より
    /// 古い観測は期限を過ぎているので、`now` から 8 日分だけを見る。
    pub fn latest_rate_limit(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<crate::llm::RateLimit>, DbError> {
        let since = timestamp(now - chrono::Duration::days(8));
        let mut stmt = self.conn.prepare(
            "SELECT rate_limit FROM llm_calls WHERE rate_limit IS NOT NULL AND at >= ?1",
        )?;
        let mut latest: Option<crate::llm::RateLimit> = None;
        for json in stmt.query_map([since], |r| r.get::<_, String>(0))? {
            let observed: crate::llm::RateLimit = serde_json::from_str(&json?)?;
            latest = Some(latest.map_or(observed, |l| l.merge(observed)));
        }
        Ok(latest)
    }

    /// `since` 以降に、そのステージの LLM の呼び出しが成功したか。
    pub fn llm_succeeded_since(
        &self,
        stage: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM llm_calls WHERE stage = ?1 AND ok = 1 AND at >= ?2)",
            [stage, &timestamp(since)],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    fn pending_ids(db: &Db, now: &str) -> Vec<i64> {
        db.pending_extract(t("2026-09-10T00:00:00Z"), t(now), 10)
            .unwrap()
            .into_iter()
            .map(|p| p.article_id)
            .collect()
    }

    #[test]
    fn pending_extract_selects_recent_articles_without_body_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let old = page_article(&db, "https://e.com/old", "2026-09-01T00:00:00.000Z");
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-25T00:00:00.000Z");
        let with_body = page_article(&db, "https://e.com/c", "2026-09-26T00:00:00.000Z");
        db.insert_content(with_body, ContentKind::Body, ContentOrigin::Feed, "x")
            .unwrap();
        let with_lead = page_article(&db, "https://e.com/d", "2026-09-24T00:00:00.000Z");
        db.insert_content(with_lead, ContentKind::Lead, ContentOrigin::Feed, "x")
            .unwrap();
        let _ = old;
        assert_eq!(pending_ids(&db, "2026-09-27T00:00:00Z"), [b, with_lead, a]);
        let limited = db
            .pending_extract(t("2026-09-10T00:00:00Z"), t("2026-09-27T00:00:00Z"), 1)
            .unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].url, "https://e.com/b");
    }

    #[test]
    fn transient_failures_back_off_exponentially_then_give_up() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let key = StageKey {
            article_id: a,
            stage: "extract",
            backend: "",
            model: "",
        };
        db.record_stage_failure(key, "HTTP 500", t("2026-09-27T00:00:00Z"), false)
            .unwrap();
        // 1 回目の失敗後は 1 時間待つ
        assert!(pending_ids(&db, "2026-09-27T00:59:00Z").is_empty());
        assert_eq!(pending_ids(&db, "2026-09-27T01:00:00Z"), [a]);
        // 2 回目は 2 時間
        db.record_stage_failure(key, "HTTP 500", t("2026-09-27T01:00:00Z"), false)
            .unwrap();
        assert!(pending_ids(&db, "2026-09-27T02:59:00Z").is_empty());
        assert_eq!(pending_ids(&db, "2026-09-27T03:00:00Z"), [a]);
        // 上限に達したら断念する
        for _ in 2..MAX_ATTEMPTS {
            db.record_stage_failure(key, "HTTP 500", t("2026-09-27T03:00:00Z"), false)
                .unwrap();
        }
        assert!(pending_ids(&db, "2026-12-31T00:00:00Z").is_empty());
        let err: String = db
            .conn()
            .query_row("SELECT last_error FROM stage_errors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(err, "HTTP 500");
    }

    #[test]
    fn permanent_failure_is_not_retried_and_success_clears() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let b = page_article(&db, "https://e.com/b", "2026-09-21T00:00:00.000Z");
        let key = |article_id| StageKey {
            article_id,
            stage: "extract",
            backend: "",
            model: "",
        };
        db.record_stage_failure(key(a), "robots", t("2026-09-27T00:00:00Z"), true)
            .unwrap();
        db.record_stage_failure(key(b), "HTTP 500", t("2026-09-27T00:00:00Z"), false)
            .unwrap();
        db.clear_stage_failure(key(b)).unwrap();
        assert_eq!(pending_ids(&db, "2026-12-31T00:00:00Z"), [b]);
    }

    #[test]
    fn failures_of_other_stages_do_not_block_extract() {
        let db = Db::open_in_memory().unwrap();
        let a = page_article(&db, "https://e.com/a", "2026-09-20T00:00:00.000Z");
        let key = StageKey {
            article_id: a,
            stage: "digest",
            backend: "claude-cli",
            model: "sonnet",
        };
        db.record_stage_failure(key, "x", t("2026-09-27T00:00:00Z"), true)
            .unwrap();
        assert_eq!(pending_ids(&db, "2026-09-27T00:00:00Z"), [a]);
    }

    #[test]
    fn records_llm_calls_with_rate_limit() {
        let db = Db::open_in_memory().unwrap();
        let rate = crate::llm::RateLimit {
            five_hour: Some(crate::llm::Window {
                utilization: 0.5,
                resets_at: 1790457000,
            }),
            seven_day: None,
        };
        db.record_llm_call(
            &LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 5,
                ok: true,
                duration_ms: 1234,
                error: None,
                usage: Some(&crate::llm::Usage::Subscription(rate)),
            },
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        db.record_llm_call(
            &LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 5,
                ok: false,
                duration_ms: 10,
                error: Some("timeout"),
                usage: None,
            },
            t("2026-09-27T01:05:00Z"),
        )
        .unwrap();
        let rows = db
            .query_strings(
                "SELECT at || '|' || stage || '|' || n_items || '|' || ok || '|' || coalesce(error, '-')
                        || '|' || coalesce(json_extract(rate_limit, '$.five_hour.utilization'), '-')
                 FROM llm_calls ORDER BY id",
            )
            .unwrap();
        assert_eq!(
            rows,
            [
                "2026-09-27T01:00:00.000Z|digest|5|1|-|0.5",
                "2026-09-27T01:05:00.000Z|digest|5|0|timeout|-",
            ]
        );
    }

    #[test]
    fn latest_rate_limit_skips_calls_without_usage() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(
            db.latest_rate_limit(t("2026-09-27T04:00:00Z")).unwrap(),
            None
        );
        fn call(usage: Option<&crate::llm::Usage>) -> LlmCall<'_> {
            LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 1,
                ok: usage.is_some(),
                duration_ms: 1,
                error: None,
                usage,
            }
        }
        let older = crate::llm::RateLimit {
            five_hour: Some(crate::llm::Window {
                utilization: 0.2,
                resets_at: 1,
            }),
            seven_day: None,
        };
        let newer = crate::llm::RateLimit {
            five_hour: Some(crate::llm::Window {
                utilization: 0.4,
                resets_at: 2,
            }),
            seven_day: None,
        };
        db.record_llm_call(
            &call(Some(&crate::llm::Usage::Subscription(older))),
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        db.record_llm_call(
            &call(Some(&crate::llm::Usage::Subscription(newer))),
            t("2026-09-27T02:00:00Z"),
        )
        .unwrap();
        db.record_llm_call(&call(None), t("2026-09-27T03:00:00Z"))
            .unwrap();
        assert_eq!(
            db.latest_rate_limit(t("2026-09-27T04:00:00Z")).unwrap(),
            Some(newer)
        );
    }

    /// 呼び出しは並行して終わる順が前後するので、最後の行ではなく、枠ごとにリセット時刻が最も新しい枠の
    /// 最も高い使用率を使う（同じ枠の中で使用率は下がらない）。
    #[test]
    fn latest_rate_limit_keeps_the_highest_usage_of_the_newest_window() {
        let db = Db::open_in_memory().unwrap();
        let window = |utilization, resets_at| {
            Some(crate::llm::Window {
                utilization,
                resets_at,
            })
        };
        let record = |rate: crate::llm::RateLimit, at: &str| {
            db.record_llm_call(
                &LlmCall {
                    stage: "digest",
                    backend: "claude-cli",
                    model: "sonnet",
                    n_items: 1,
                    ok: true,
                    duration_ms: 1,
                    error: None,
                    usage: Some(&crate::llm::Usage::Subscription(rate)),
                },
                t(at),
            )
            .unwrap();
        };
        let now = t("2026-09-27T04:00:00Z");
        record(
            crate::llm::RateLimit {
                five_hour: window(0.6, 100),
                seven_day: window(0.3, 1000),
            },
            "2026-09-27T01:00:00Z",
        );
        // 先に始めた呼び出しが後から終わり、古い（低い）使用率を記録する
        record(
            crate::llm::RateLimit {
                five_hour: window(0.4, 100),
                seven_day: window(0.35, 1000),
            },
            "2026-09-27T02:00:00Z",
        );
        assert_eq!(
            db.latest_rate_limit(now).unwrap(),
            Some(crate::llm::RateLimit {
                five_hour: window(0.6, 100),
                seven_day: window(0.35, 1000),
            })
        );
        // 新しい枠になれば、使用率が低くてもそちらを使う
        record(
            crate::llm::RateLimit {
                five_hour: window(0.1, 200),
                seven_day: None,
            },
            "2026-09-27T03:00:00Z",
        );
        assert_eq!(
            db.latest_rate_limit(now).unwrap(),
            Some(crate::llm::RateLimit {
                five_hour: window(0.1, 200),
                seven_day: window(0.35, 1000),
            })
        );
    }

    #[test]
    fn llm_succeeded_since_ignores_failures_and_other_stages() {
        let db = Db::open_in_memory().unwrap();
        let call = |stage: &'static str, ok: bool| LlmCall {
            stage,
            backend: "fake",
            model: "sonnet",
            n_items: 1,
            ok,
            duration_ms: 1,
            error: None,
            usage: None,
        };
        db.record_llm_call(&call("tidy", true), t("2026-09-20T00:00:00Z"))
            .unwrap();
        db.record_llm_call(&call("tidy", false), t("2026-09-26T00:00:00Z"))
            .unwrap();
        db.record_llm_call(&call("digest", true), t("2026-09-26T00:00:00Z"))
            .unwrap();
        assert!(
            db.llm_succeeded_since("tidy", t("2026-09-20T00:00:00Z"))
                .unwrap()
        );
        assert!(
            !db.llm_succeeded_since("tidy", t("2026-09-21T00:00:00Z"))
                .unwrap()
        );
    }
}
