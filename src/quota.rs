//! サブスクリプションの使用量に応じた LLM 呼び出しの可否判定。
//!
//! - 5 時間枠：現地時刻の時間帯ごとに上限を変える（日中は多く、開発に使う深夜は少なく）。
//! - 週次枠：絶対上限に加え、週の経過に応じたペース配分（直線より `pace_ahead_days` 日分だけ
//!   先行まで）で、週の前半に使い切らないようにする。
//! - 1 回の実行あたりの呼び出し回数の上限。
//!
//! 使用率は直前の呼び出しで得た値（`rate_limit_event`）を使う。リセット時刻を過ぎた枠は 0 とみなす。

use chrono::{DateTime, Timelike, Utc};
use serde::Deserialize;

use crate::llm::{RateLimit, Window};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuotaConfig {
    /// 時間帯の判定に使う UTC からの時差（JST は 9）
    pub timezone_offset_hours: i32,
    /// 時間帯ごとの 5 時間枠の上限。`start <= 時 < end`（現地時刻）
    pub slots: Vec<Slot>,
    /// どの時間帯にも当たらないときの 5 時間枠の上限
    pub default_max_five_hour: f64,
    /// 週次枠の絶対上限
    pub weekly_max: f64,
    /// 週次のペース配分で、直線的な消費より何日分先行してよいか
    pub pace_ahead_days: f64,
    /// 1 回の実行で呼び出してよい回数
    pub max_calls_per_run: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub start: u32,
    pub end: u32,
    pub max_five_hour: f64,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        todo!()
    }
}

/// 呼び出しを止める理由。
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    FiveHour {
        used: f64,
        limit: f64,
        resets_at: i64,
    },
    Weekly {
        used: f64,
        limit: f64,
    },
    WeeklyPace {
        used: f64,
        limit: f64,
    },
    MaxCalls {
        limit: u32,
    },
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stop::FiveHour {
                used,
                limit,
                resets_at,
            } => write!(
                f,
                "5-hour usage {:.0}% reached the {:.0}% limit for this time slot (resets at {resets_at})",
                used * 100.0,
                limit * 100.0
            ),
            Stop::Weekly { used, limit } => write!(
                f,
                "weekly usage {:.0}% reached the {:.0}% limit",
                used * 100.0,
                limit * 100.0
            ),
            Stop::WeeklyPace { used, limit } => write!(
                f,
                "weekly usage {:.0}% is ahead of the pace allowance {:.0}%",
                used * 100.0,
                limit * 100.0
            ),
            Stop::MaxCalls { limit } => write!(f, "reached {limit} llm calls in this run"),
        }
    }
}

/// 1 回の実行の間、使用率と呼び出し回数を追う。
#[derive(Debug)]
pub struct Quota {
    cfg: QuotaConfig,
    usage: Option<RateLimit>,
    calls: u32,
    max_calls: u32,
}

impl Quota {
    /// `usage` は直前に分かっている使用率（DB の最新の llm_calls）。`max_calls` を指定すれば
    /// 設定の `max_calls_per_run` より優先する（`crawl --max-llm-calls`）。
    pub fn new(_cfg: QuotaConfig, _usage: Option<RateLimit>, _max_calls: Option<u32>) -> Self {
        todo!()
    }

    /// 次の呼び出しをしてよいか。
    pub fn permit(&self, _now: DateTime<Utc>) -> Result<(), Stop> {
        todo!()
    }

    /// 呼び出しを 1 回行ったことと、その応答で分かった使用率を記録する。
    pub fn record_call(&mut self, _usage: Option<RateLimit>) {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JST の時刻
    fn jst(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("{s}+09:00"))
            .unwrap()
            .to_utc()
    }

    fn window(utilization: f64, resets_at: DateTime<Utc>) -> Option<Window> {
        Some(Window {
            utilization,
            resets_at: resets_at.timestamp(),
        })
    }

    fn usage(five: f64, week: f64, now: DateTime<Utc>, week_elapsed_days: f64) -> RateLimit {
        let week_reset =
            now + chrono::Duration::seconds(((7.0 - week_elapsed_days) * 86400.0) as i64);
        RateLimit {
            five_hour: window(five, now + chrono::Duration::hours(2)),
            seven_day: window(week, week_reset),
        }
    }

    fn quota(u: RateLimit) -> Quota {
        Quota::new(QuotaConfig::default(), Some(u), None)
    }

    #[test]
    fn defaults_match_the_agreed_schedule() {
        let c = QuotaConfig::default();
        assert_eq!(c.timezone_offset_hours, 9);
        assert_eq!(
            c.slots,
            [
                Slot {
                    start: 3,
                    end: 8,
                    max_five_hour: 0.20
                },
                Slot {
                    start: 10,
                    end: 15,
                    max_five_hour: 0.85
                },
                Slot {
                    start: 16,
                    end: 21,
                    max_five_hour: 0.60
                },
            ]
        );
        assert_eq!(c.default_max_five_hour, 0.20);
        assert_eq!(c.weekly_max, 0.70);
        assert_eq!(c.pace_ahead_days, 1.0);
        assert_eq!(c.max_calls_per_run, 30);
    }

    #[test]
    fn five_hour_limit_depends_on_local_time_slot() {
        // 週次は十分に余裕があり、5 時間枠を 50% 使った状態
        for (time, allowed) in [
            ("2026-09-28T10:30:00", true),  // 10-15 時は 85%
            ("2026-09-28T16:00:00", true),  // 16-21 時は 60%
            ("2026-09-28T03:10:00", false), // 3-8 時は 20%
            ("2026-09-28T22:00:00", false), // それ以外は既定の 20%
        ] {
            let now = jst(time);
            assert_eq!(
                quota(usage(0.5, 0.1, now, 5.0)).permit(now).is_ok(),
                allowed,
                "{time}"
            );
        }
        let now = jst("2026-09-28T03:10:00");
        assert!(matches!(
            quota(usage(0.5, 0.1, now, 5.0)).permit(now),
            Err(Stop::FiveHour { limit, .. }) if limit == 0.20
        ));
    }

    #[test]
    fn expired_windows_count_as_unused() {
        let now = jst("2026-09-28T22:00:00");
        let u = RateLimit {
            five_hour: window(0.99, now - chrono::Duration::minutes(1)),
            seven_day: window(0.99, now - chrono::Duration::minutes(1)),
        };
        assert!(quota(u).permit(now).is_ok());
    }

    #[test]
    fn unknown_usage_is_allowed() {
        let q = Quota::new(QuotaConfig::default(), None, None);
        assert!(q.permit(jst("2026-09-28T22:00:00")).is_ok());
    }

    #[test]
    fn weekly_absolute_limit() {
        let now = jst("2026-09-28T10:30:00");
        let err = quota(usage(0.1, 0.71, now, 6.9)).permit(now).unwrap_err();
        assert!(matches!(err, Stop::Weekly { .. }), "{err}");
    }

    #[test]
    fn weekly_pace_allows_one_day_ahead_of_linear() {
        let now = jst("2026-09-28T10:30:00");
        // 経過 0.7 日（10%）：許容は 70% × (0.1 + 1/7) ≈ 17%
        assert!(quota(usage(0.1, 0.15, now, 0.7)).permit(now).is_ok());
        let err = quota(usage(0.1, 0.20, now, 0.7)).permit(now).unwrap_err();
        assert!(matches!(err, Stop::WeeklyPace { .. }), "{err}");
        // 週の始めでも 1 日分は使える
        assert!(quota(usage(0.1, 0.05, now, 0.0)).permit(now).is_ok());
    }

    #[test]
    fn stops_after_max_calls_and_observes_new_usage() {
        let now = jst("2026-09-28T10:30:00");
        let mut q = Quota::new(
            QuotaConfig::default(),
            Some(usage(0.1, 0.1, now, 5.0)),
            Some(2),
        );
        q.record_call(None);
        assert!(q.permit(now).is_ok());
        // 応答で使用率が上がれば、それに従って止まる
        q.record_call(Some(usage(0.9, 0.1, now, 5.0)));
        assert!(matches!(q.permit(now), Err(Stop::FiveHour { .. })));
        let mut q = Quota::new(
            QuotaConfig::default(),
            Some(usage(0.1, 0.1, now, 5.0)),
            Some(1),
        );
        q.record_call(Some(usage(0.1, 0.1, now, 5.0)));
        assert_eq!(q.permit(now), Err(Stop::MaxCalls { limit: 1 }));
    }
}
