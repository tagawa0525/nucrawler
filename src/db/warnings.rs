//! 画面に出す警告。

use super::*;

/// 画面の上部に出す警告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// 最後の取得が失敗している（最後の成功より新しい失敗がある）ソース
    SourceFailing {
        source_id: String,
        error: String,
        at: String,
    },
    /// 最後の取得で一覧・フィードが 0 件だった（selector やフィードの形が変わった疑い）
    SourceEmpty { source_id: String, at: String },
    /// 最後の取得の件数が、それまでの回の中央値より大きく減った
    SourceDropped {
        source_id: String,
        total: i64,
        median: i64,
        at: String,
    },
    /// 直近の LLM の呼び出しが失敗している
    LlmFailed { error: String, at: String },
}

/// 件数の急減の判定に使う、最新の回より前の回の数の上限（既定の 1 日 4 回で約 5 日分。週末をまたぐ）
const DROP_HISTORY: usize = 20;
/// 急減を判定するのに要る、前の回の数（約 2 日分）。足りなければ 0 件のときだけ警告する
const DROP_MIN_HISTORY: usize = 8;
/// 前の回の中央値のこの割合（1/3）を下回ったら急減とみなす。週末などの半減は拾わない
const DROP_RATIO: i64 = 3;

/// 取得の件数の異常。
#[derive(Debug, PartialEq, Eq)]
enum CountAnomaly {
    Empty,
    Dropped { median: i64 },
}

/// 最新の回の件数 `latest` を、その前の回の件数 `previous`（新しい順、最大 `DROP_HISTORY` 回）と比べる。
fn count_anomaly(latest: i64, previous: &[i64]) -> Option<CountAnomaly> {
    todo!("{latest} {previous:?} {DROP_HISTORY} {DROP_MIN_HISTORY} {DROP_RATIO}")
}

impl Db {
    /// 取得に失敗し続けているソース、`since` 以降の最後の取得で件数が 0 件か急減したソース、
    /// `since` 以降の直近の LLM の失敗。
    pub fn warnings(&self, since: chrono::DateTime<chrono::Utc>) -> Result<Vec<Warning>, DbError> {
        use rusqlite::OptionalExtension;
        // 取得に成功するとエラーは消えるので、残っているエラーは今も失敗しているもの
        let mut stmt = self.conn.prepare(
            "SELECT source_id, last_error, last_error_at FROM source_state
             WHERE last_error IS NOT NULL ORDER BY source_id",
        )?;
        let mut warnings = stmt
            .query_map([], |r| {
                Ok(Warning::SourceFailing {
                    source_id: r.get(0)?,
                    error: r.get(1)?,
                    at: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let latest: Option<(bool, Option<String>, String)> = self
            .conn
            .query_row(
                "SELECT ok, error, at FROM llm_calls WHERE at >= ?1
                 ORDER BY at DESC, id DESC LIMIT 1",
                [timestamp(since)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((false, Some(error), at)) = latest {
            warnings.push(Warning::LlmFailed { error, at });
        }
        Ok(warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    #[test]
    fn warnings_report_failing_sources_and_recent_llm_errors() {
        let db = Db::open_in_memory().unwrap();
        let counts = FetchCounts {
            total: 5,
            matched: 5,
            ..FetchCounts::default()
        };
        let ok = |id| {
            db.record_source_success(id, &counts, t("2026-09-27T00:00:00Z"))
                .unwrap()
        };
        ok("ok");
        db.record_source_failure("recovered", "old").unwrap();
        ok("recovered");
        db.record_source_failure("nei", "HTTP 403").unwrap();
        db.record_llm_call(
            &LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 5,
                ok: false,
                duration_ms: 1,
                error: Some("Not logged in"),
                rate_limit: None,
            },
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        let warnings = db.warnings(t("2026-09-26T00:00:00Z")).unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            matches!(&warnings[0], Warning::SourceFailing { source_id, error, .. }
            if source_id == "nei" && error == "HTTP 403")
        );
        assert!(
            matches!(&warnings[1], Warning::LlmFailed { error, .. } if error == "Not logged in")
        );
        // 失敗の後に成功した呼び出しがあれば、LLM の警告は出さない
        db.record_llm_call(
            &LlmCall {
                stage: "digest",
                backend: "claude-cli",
                model: "sonnet",
                n_items: 5,
                ok: true,
                duration_ms: 1,
                error: None,
                rate_limit: None,
            },
            t("2026-09-27T02:00:00Z"),
        )
        .unwrap();
        assert_eq!(db.warnings(t("2026-09-26T00:00:00Z")).unwrap().len(), 1);
    }

    #[test]
    fn count_anomaly_flags_empty_lists_and_sharp_drops() {
        use CountAnomaly::*;
        // 0 件は履歴が無くても警告する
        assert_eq!(count_anomaly(0, &[]), Some(Empty));
        assert_eq!(count_anomaly(0, &[10; 10]), Some(Empty));
        // 履歴が足りなければ急減は判定しない
        assert_eq!(count_anomaly(1, &[10; 7]), None);
        // 中央値の 1/3 を下回ったら急減。半減程度は拾わない
        assert_eq!(count_anomaly(5, &[10; 8]), None);
        assert_eq!(count_anomaly(4, &[10; 8]), None);
        assert_eq!(count_anomaly(3, &[10; 8]), Some(Dropped { median: 10 }));
        // 偶数個の中央値は中央の 2 つの平均（8 と 10 → 9）
        let even = [16, 2, 14, 4, 12, 6, 10, 8];
        assert_eq!(count_anomaly(2, &even), Some(Dropped { median: 9 }));
        assert_eq!(count_anomaly(3, &even), None);
        // 一時的な増減に引きずられない
        assert_eq!(count_anomaly(5, &[10, 10, 10, 10, 10, 200, 300, 400]), None);
        // ずっと 0 件だったソースで件数が出れば正常
        assert_eq!(count_anomaly(5, &[0; 8]), None);
    }

    /// 取得の回を古い順に記録する。最後の回が `end` の時刻になるよう、1 時間おきに並べる。
    fn runs(db: &Db, source_id: &str, totals: &[usize], end: &str) {
        let n = totals.len() as i64;
        for (i, &total) in totals.iter().enumerate() {
            let at = t(end) - chrono::Duration::hours(n - 1 - i as i64);
            let counts = FetchCounts {
                total,
                matched: total,
                ..FetchCounts::default()
            };
            db.record_source_success(source_id, &counts, at).unwrap();
        }
    }

    #[test]
    fn warnings_report_empty_and_dropped_sources() {
        let db = Db::open_in_memory().unwrap();
        let end = "2026-09-27T00:00:00Z";
        runs(&db, "steady", &[[10; 9].as_slice(), &[4]].concat(), end);
        runs(&db, "dropped", &[[30; 9].as_slice(), &[5]].concat(), end);
        runs(&db, "empty", &[0], end);
        // 最新の回が `since` より前（設定から消した、無効にした）なら出さない
        runs(&db, "gone", &[0], "2026-09-20T00:00:00Z");
        // 比べるのは直前の DROP_HISTORY 回だけ。もっと古い回の大きな件数は使わない
        runs(
            &db,
            "window",
            &[[100; 30].as_slice(), &[10; 20], &[4]].concat(),
            end,
        );
        let warnings = db.warnings(t("2026-09-26T00:00:00Z")).unwrap();
        assert_eq!(
            warnings,
            [
                Warning::SourceDropped {
                    source_id: "dropped".into(),
                    total: 5,
                    median: 30,
                    at: "2026-09-27T00:00:00.000Z".into(),
                },
                Warning::SourceEmpty {
                    source_id: "empty".into(),
                    at: "2026-09-27T00:00:00.000Z".into(),
                },
            ]
        );
    }

    /// 「最新の呼び出し」は記録の順ではなく、呼び出した時刻で決める。
    #[test]
    fn warnings_use_call_time_not_insertion_order() {
        let db = Db::open_in_memory().unwrap();
        let call = |ok: bool| LlmCall {
            stage: "digest",
            backend: "claude-cli",
            model: "sonnet",
            n_items: 1,
            ok,
            duration_ms: 1,
            error: (!ok).then_some("Not logged in"),
            rate_limit: None,
        };
        db.record_llm_call(&call(false), t("2026-09-27T02:00:00Z"))
            .unwrap();
        // 古い成功が後から記録された
        db.record_llm_call(&call(true), t("2026-09-27T01:00:00Z"))
            .unwrap();
        let warnings = db.warnings(t("2026-09-26T00:00:00Z")).unwrap();
        assert!(
            matches!(&warnings[..], [Warning::LlmFailed { .. }]),
            "{warnings:?}"
        );
    }
}
