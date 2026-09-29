//! 画面に出す警告。

use super::*;

/// 画面に出す警告（新着の途絶えは一番下、ほかは上部）。
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
    /// 取得は成功しているのに、新着が普段の間隔よりずっと長く途絶えている（フィードの停止や
    /// 絞り込み・日付の読み取りが外れた疑い）
    SourceStale {
        source_id: String,
        /// 最新の記事の公開日から今日までの日数
        idle_days: i64,
        /// 普段の新着の間隔（日）
        typical_gap_days: i64,
    },
    /// 直近の LLM の呼び出しが失敗している。`backend` は失敗した呼び出しのバックエンド（対処の案内に使う）
    LlmFailed {
        error: String,
        backend: String,
        at: String,
    },
}

/// 件数の急減の判定に使う、最新の回より前の回の数の上限（既定の 1 日 4 回で約 5 日分。週末をまたぐ）
const DROP_HISTORY: usize = 20;
/// 急減を判定するのに要る、前の回の数（約 2 日分）。足りなければ 0 件のときだけ警告する
const DROP_MIN_HISTORY: usize = 8;
/// 前の回の中央値のこの割合（1/3）を下回ったら急減とみなす。週末などの半減は拾わない
const DROP_RATIO: i64 = 3;

/// 新着の間隔を調べる期間（最新の記事の日から遡る日数）
const STALE_WINDOW_DAYS: i64 = 60;
/// 普段の間隔を決めるのに要る、新着のあった日どうしの間隔の数。足りなければ判定しない
const STALE_MIN_GAPS: usize = 5;
/// 普段の間隔の何倍途絶えたら警告するか
const STALE_RATIO: i64 = 3;
/// 毎日更新されるソースでも、これより短い途絶えは連休などとみなして警告しない
const STALE_MIN_DAYS: i64 = 7;

/// 新着の途絶え。
#[derive(Debug, PartialEq, Eq)]
struct Stale {
    idle_days: i64,
    typical_gap_days: i64,
}

/// 新着のあった日 `days`（古い順、重複なし、最新の日から `STALE_WINDOW_DAYS` 以内）と今日を比べる。
fn stale(days: &[chrono::NaiveDate], today: chrono::NaiveDate) -> Option<Stale> {
    let mut gaps: Vec<i64> = days.windows(2).map(|w| (w[1] - w[0]).num_days()).collect();
    if gaps.len() < STALE_MIN_GAPS {
        return None;
    }
    gaps.sort_unstable();
    let n = gaps.len();
    // 中央値の 2 倍。偶数個のとき半端（x.5 日）になるので、比べるときは 2 倍のまま扱う
    let twice_median = gaps[(n - 1) / 2] + gaps[n / 2];
    let idle_days = (today - *days.last()?).num_days();
    (2 * idle_days > (2 * STALE_MIN_DAYS).max(STALE_RATIO * twice_median)).then_some(Stale {
        idle_days,
        // 表示は四捨五入
        typical_gap_days: (twice_median + 1) / 2,
    })
}

/// 取得の件数の異常。
#[derive(Debug, PartialEq, Eq)]
enum CountAnomaly {
    Empty,
    Dropped { median: i64 },
}

/// 最新の回の件数 `latest` を、その前の回の件数 `previous`（新しい順、最大 `DROP_HISTORY` 回）と比べる。
fn count_anomaly(latest: i64, previous: &[i64]) -> Option<CountAnomaly> {
    if latest == 0 {
        return Some(CountAnomaly::Empty);
    }
    if previous.len() < DROP_MIN_HISTORY {
        return None;
    }
    let mut sorted = previous.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let median = (sorted[(n - 1) / 2] + sorted[n / 2]) / 2;
    (latest * DROP_RATIO < median).then_some(CountAnomaly::Dropped { median })
}

impl Db {
    /// 取得に失敗し続けているソース、`since` 以降の最後の取得で件数が 0 件か急減したソース、
    /// 新着が普段より長く途絶えているソース、`since` 以降の直近の LLM の失敗。
    /// `now` は新着の途絶えを測る基準。
    pub fn warnings(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Warning>, DbError> {
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
        let counts = self.fetch_count_warnings(since)?;
        // 一覧が空・急減のソースは、新着の途絶えもその結果なので重ねて出さない
        let flagged: Vec<String> = counts
            .iter()
            .filter_map(|w| match w {
                Warning::SourceEmpty { source_id, .. }
                | Warning::SourceDropped { source_id, .. } => Some(source_id.clone()),
                _ => None,
            })
            .collect();
        warnings.extend(counts);
        warnings.extend(self.stale_warnings(since, now)?.into_iter().filter(
            |w| !matches!(w, Warning::SourceStale { source_id, .. } if flagged.contains(source_id)),
        ));
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
            warnings.push(Warning::LlmFailed {
                error,
                backend: String::new(),
                at,
            });
        }
        Ok(warnings)
    }

    /// 最後の取得が `since` 以降に成功し、今は失敗していないソースについて、新着の途絶えの警告
    /// （source_id 順）。
    /// 新着の日は一覧と同じ日時（公開日時、無ければ取得日時）の UTC の日付で数える。
    fn stale_warnings(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Warning>, DbError> {
        // 対象のソースごとに、最新の記事の日時を索引（articles_by_source_at）で 1 件だけ読む
        let mut latest = self.conn.prepare(
            "SELECT st.source_id,
                    (SELECT coalesce(a.published_at, a.fetched_at) FROM articles AS a
                     WHERE a.source_id = st.source_id
                     ORDER BY coalesce(a.published_at, a.fetched_at) DESC LIMIT 1)
             FROM source_state AS st
             -- 失敗中のソースは取得失敗の警告だけを出す
             WHERE st.last_success_at >= ?1 AND st.last_error IS NULL
             ORDER BY st.source_id",
        )?;
        let sources = latest
            .query_map([timestamp(since)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        // 最新の日から STALE_WINDOW_DAYS 日遡った日以降の、新着のあった日（索引の範囲で読む）
        let mut window = self.conn.prepare(
            "SELECT DISTINCT substr(coalesce(published_at, fetched_at), 1, 10) AS day
             FROM articles
             WHERE source_id = ?1 AND coalesce(published_at, fetched_at) >= ?2
             ORDER BY day",
        )?;
        // 公開日時は取得時に日時として解釈して書くので、読めなければ DB が壊れている
        let unexpected = |source_id: &str, value: &str| {
            DbError::UnexpectedValue(format!("article date {value:?} of source {source_id:?}"))
        };
        let today = now.date_naive();
        let mut warnings = Vec::new();
        for (source_id, last) in sources {
            let Some(last) = last else { continue };
            let last_day = chrono::DateTime::parse_from_rfc3339(&last)
                .map_err(|_| unexpected(&source_id, &last))?
                .date_naive();
            // 数えるのは日付なので、期間も日付で区切る（その日の早い時刻の記事も含める）
            let first_day = last_day - chrono::Duration::days(STALE_WINDOW_DAYS);
            let days = window
                .query_map(
                    rusqlite::params![source_id, first_day.format("%Y-%m-%d").to_string()],
                    |r| r.get::<_, String>(0),
                )?
                .map(|day| {
                    let day = day?;
                    chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d")
                        .map_err(|_| unexpected(&source_id, &day))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(Stale {
                idle_days,
                typical_gap_days,
            }) = stale(&days, today)
            {
                warnings.push(Warning::SourceStale {
                    source_id,
                    idle_days,
                    typical_gap_days,
                });
            }
        }
        Ok(warnings)
    }

    /// 最後の取得が `since` 以降のソースについて、その回の件数を前の回と比べた警告（source_id 順）。
    fn fetch_count_warnings(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Warning>, DbError> {
        let mut stmt = self.conn.prepare(
            // 成功と件数は同じ時刻で一緒に記録するので、最後の成功が `since` 以降のソースが対象。
            // 履歴全体に順位を付けず、索引（source_id, fetched_at）でソースごとに新しい回だけを読む
            "SELECT st.source_id, r.fetched_at, r.total
             FROM source_state AS st
             JOIN fetch_runs AS r ON r.id IN (
               SELECT id FROM fetch_runs
               WHERE source_id = st.source_id
               ORDER BY fetched_at DESC, id DESC LIMIT ?1)
             WHERE st.last_success_at >= ?2
             ORDER BY st.source_id, r.fetched_at DESC, r.id DESC",
        )?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    i64::try_from(DROP_HISTORY + 1).unwrap_or(i64::MAX),
                    timestamp(since)
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut warnings = Vec::new();
        // 行はソースごとに新しい順に並んでいるので、先頭が最新の回
        for runs in rows.chunk_by(|a, b| a.0 == b.0) {
            let (source_id, at, total) = &runs[0];
            let previous: Vec<i64> = runs[1..].iter().map(|r| r.2).collect();
            let (source_id, at, total) = (source_id.clone(), at.clone(), *total);
            match count_anomaly(total, &previous) {
                Some(CountAnomaly::Empty) => warnings.push(Warning::SourceEmpty { source_id, at }),
                Some(CountAnomaly::Dropped { median }) => warnings.push(Warning::SourceDropped {
                    source_id,
                    total,
                    median,
                    at,
                }),
                None => {}
            }
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
                usage: None,
            },
            t("2026-09-27T01:00:00Z"),
        )
        .unwrap();
        let warnings = db
            .warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z"))
            .unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            matches!(&warnings[0], Warning::SourceFailing { source_id, error, .. }
            if source_id == "nei" && error == "HTTP 403")
        );
        assert!(
            matches!(&warnings[1], Warning::LlmFailed { error, backend, .. }
                if error == "Not logged in" && backend == "claude-cli"),
            "{warnings:?}"
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
                usage: None,
            },
            t("2026-09-27T02:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            db.warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z"))
                .unwrap()
                .len(),
            1
        );
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
        let warnings = db
            .warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z"))
            .unwrap();
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

    #[test]
    fn stale_compares_idle_days_with_the_usual_gap() {
        let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let every = |start: &str, step: i64, n: i64| -> Vec<_> {
            (0..n)
                .map(|i| d(start) + chrono::Duration::days(i * step))
                .collect()
        };
        // 毎日（間隔 1 日）更新されるソースは、7 日を超えて途絶えたら警告する
        let daily = every("2026-09-01", 1, 10); // 最新は 09-10
        assert_eq!(stale(&daily, d("2026-09-17")), None);
        assert_eq!(
            stale(&daily, d("2026-09-18")),
            Some(Stale {
                idle_days: 8,
                typical_gap_days: 1
            })
        );
        // 週 1 回のソースは 3 週（21 日）を超えるまで待つ
        let weekly = every("2026-07-01", 7, 8); // 最新は 08-19
        assert_eq!(stale(&weekly, d("2026-09-09")), None);
        assert_eq!(
            stale(&weekly, d("2026-09-10")),
            Some(Stale {
                idle_days: 22,
                typical_gap_days: 7
            })
        );
        // 間隔が 5 個に満たなければ普段の間隔が分からないので判定しない
        assert_eq!(stale(&every("2026-09-01", 1, 5), d("2026-12-01")), None);
        assert_eq!(stale(&[], d("2026-12-01")), None);
        // 間隔は中央値で決める（偶数個なら中央の 2 つの平均：2 と 4 → 3）
        // 間隔は 1, 2, 4, 10, 1, 5 日。並べると 1, 1, 2, 4, 5, 10 で、閾値は 3 × 3 = 9 日
        let mixed = [
            d("2026-09-01"),
            d("2026-09-02"),
            d("2026-09-04"),
            d("2026-09-08"),
            d("2026-09-18"),
            d("2026-09-19"),
            d("2026-09-24"),
        ];
        assert_eq!(stale(&mixed, d("2026-10-03")), None);
        // 中央値が半端（3 と 4 → 3.5 日）なら閾値は 10.5 日。切り捨てて早く警告しない。表示は四捨五入
        let halves = [
            d("2026-09-01"),
            d("2026-09-04"),
            d("2026-09-08"),
            d("2026-09-11"),
            d("2026-09-15"),
            d("2026-09-18"),
            d("2026-09-22"),
        ];
        assert_eq!(stale(&halves, d("2026-10-02")), None);
        assert_eq!(
            stale(&halves, d("2026-10-03")),
            Some(Stale {
                idle_days: 11,
                typical_gap_days: 4
            })
        );
        assert_eq!(
            stale(&mixed, d("2026-10-04")),
            Some(Stale {
                idle_days: 10,
                typical_gap_days: 3
            })
        );
    }

    #[test]
    fn warnings_report_stale_sources() {
        let db = Db::open_in_memory().unwrap();
        let counts = FetchCounts {
            total: 5,
            matched: 5,
            ..FetchCounts::default()
        };
        // 取得は今も成功している
        let fetched = |id| {
            db.record_source_success(id, &counts, t("2026-09-27T00:00:00Z"))
                .unwrap()
        };
        let publish = |id: &str, day: &str| {
            db.insert_article(&NewArticle {
                source_id: id,
                published_at: Some(&format!("{day}T03:00:00.000Z")),
                ..article(&format!("https://e.com/{id}/{day}"))
            })
            .unwrap();
        };
        // 毎日あった新着が 09-10 で途絶えた
        for day in 1..=10 {
            publish("quiet", &format!("2026-09-{day:02}"));
        }
        fetched("quiet");
        // 毎日の新着が続いている
        for day in 17..=26 {
            publish("busy", &format!("2026-09-{day:02}"));
        }
        fetched("busy");
        // 途絶えているが、最近は取得していない（無効にした、設定から消した）
        for day in 1..=10 {
            publish("off", &format!("2026-09-{day:02}"));
        }
        db.record_source_success("off", &counts, t("2026-09-20T00:00:00Z"))
            .unwrap();
        let warnings = db
            .warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z"))
            .unwrap();
        assert_eq!(
            warnings,
            [Warning::SourceStale {
                source_id: "quiet".into(),
                idle_days: 17,
                typical_gap_days: 1,
            }]
        );
    }

    /// 取得に失敗中のソースは、取得失敗の警告だけを出す（途絶えはその結果なので重ねない）。
    #[test]
    fn failing_sources_are_not_also_reported_as_stale() {
        let db = Db::open_in_memory().unwrap();
        for day in 1..=10 {
            db.insert_article(&NewArticle {
                source_id: "down",
                published_at: Some(&format!("2026-09-{day:02}T03:00:00.000Z")),
                ..article(&format!("https://e.com/{day}"))
            })
            .unwrap();
        }
        let counts = FetchCounts {
            total: 5,
            matched: 5,
            ..FetchCounts::default()
        };
        db.record_source_success("down", &counts, t("2026-09-27T00:00:00Z"))
            .unwrap();
        db.record_source_failure("down", "HTTP 503").unwrap();
        let warnings = db
            .warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z"))
            .unwrap();
        assert!(
            matches!(&warnings[..], [Warning::SourceFailing { .. }]),
            "{warnings:?}"
        );
    }

    /// 期間は日付で区切る。最新の日から 60 日前の日は、時刻によらず含める。
    #[test]
    fn stale_window_starts_at_a_calendar_day() {
        let db = Db::open_in_memory().unwrap();
        let publish = |at: &str| {
            db.insert_article(&NewArticle {
                source_id: "s",
                published_at: Some(at),
                ..article(&format!("https://e.com/{at}"))
            })
            .unwrap();
        };
        // 60 日前（07-12）の早い時刻の 1 件と、その後ほぼ 5 日おきの 4 件。最新は 09-10 の遅い時刻
        publish("2026-07-12T01:00:00.000Z");
        for day in ["2026-08-22", "2026-08-27", "2026-09-01", "2026-09-05"] {
            publish(&format!("{day}T12:00:00.000Z"));
        }
        publish("2026-09-10T23:00:00.000Z");
        let counts = FetchCounts {
            total: 5,
            matched: 5,
            ..FetchCounts::default()
        };
        db.record_source_success("s", &counts, t("2026-09-27T00:00:00Z"))
            .unwrap();
        // 07-12 を含めて初めて間隔が 5 個（41, 5, 5, 4, 5 日。中央値 5 日、閾値 15 日）そろい、
        // 17 日の途絶えを警告できる
        let warnings = db
            .warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z"))
            .unwrap();
        assert_eq!(
            warnings,
            [Warning::SourceStale {
                source_id: "s".into(),
                idle_days: 17,
                typical_gap_days: 5,
            }]
        );
    }

    /// 公開日時は取得時に日時として解釈して書くので、日付として読めない値は壊れた DB。黙って捨てない。
    #[test]
    fn stale_check_rejects_unreadable_dates() {
        let db = Db::open_in_memory().unwrap();
        for at in ["2026-09-10T03:00:00.000Z", "2026-09-0xT03:00:00.000Z"] {
            db.insert_article(&NewArticle {
                source_id: "s",
                published_at: Some(at),
                ..article(&format!("https://e.com/{at}"))
            })
            .unwrap();
        }
        let counts = FetchCounts {
            total: 5,
            matched: 5,
            ..FetchCounts::default()
        };
        db.record_source_success("s", &counts, t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert!(matches!(
            db.warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z")),
            Err(DbError::UnexpectedValue(_))
        ));
    }

    /// 壊れた値が最新の記事の日時になっていても、黙って判定を飛ばさない。
    #[test]
    fn stale_check_rejects_an_unreadable_latest_date() {
        let db = Db::open_in_memory().unwrap();
        // 文字列としては日付より後ろに並ぶので、最新の記事の日時として選ばれる
        for at in ["2026-09-10T03:00:00.000Z", "not a date"] {
            db.insert_article(&NewArticle {
                source_id: "s",
                published_at: Some(at),
                ..article(&format!("https://e.com/{at}"))
            })
            .unwrap();
        }
        let counts = FetchCounts {
            total: 5,
            matched: 5,
            ..FetchCounts::default()
        };
        db.record_source_success("s", &counts, t("2026-09-27T00:00:00Z"))
            .unwrap();
        assert!(matches!(
            db.warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z")),
            Err(DbError::UnexpectedValue(_))
        ));
    }

    /// 一覧が 0 件のソースは、その警告だけを出す（新着の途絶えは同じ原因の結果なので重ねない）。
    #[test]
    fn empty_sources_are_not_also_reported_as_stale() {
        let db = Db::open_in_memory().unwrap();
        for day in 1..=10 {
            db.insert_article(&NewArticle {
                source_id: "broken",
                published_at: Some(&format!("2026-09-{day:02}T03:00:00.000Z")),
                ..article(&format!("https://e.com/{day}"))
            })
            .unwrap();
        }
        db.record_source_success("broken", &FetchCounts::default(), t("2026-09-27T00:00:00Z"))
            .unwrap();
        let warnings = db
            .warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z"))
            .unwrap();
        assert!(
            matches!(&warnings[..], [Warning::SourceEmpty { .. }]),
            "{warnings:?}"
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
            usage: None,
        };
        db.record_llm_call(&call(false), t("2026-09-27T02:00:00Z"))
            .unwrap();
        // 古い成功が後から記録された
        db.record_llm_call(&call(true), t("2026-09-27T01:00:00Z"))
            .unwrap();
        let warnings = db
            .warnings(t("2026-09-26T00:00:00Z"), t("2026-09-27T12:00:00Z"))
            .unwrap();
        assert!(
            matches!(&warnings[..], [Warning::LlmFailed { .. }]),
            "{warnings:?}"
        );
    }
}
