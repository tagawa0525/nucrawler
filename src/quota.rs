//! サブスクリプションの使用量に応じた LLM 呼び出しの可否判定。
//!
//! - 5 時間枠：現地時刻の時間帯ごとに上限を変える（日中は多く、開発に使う深夜は少なく）。
//! - 週次枠：絶対上限に加え、週の経過に応じたペース配分（直線より `pace_ahead_days` 日分だけ
//!   先行まで）で、週の前半に使い切らないようにする。
//! - 1 回の実行あたりの呼び出し回数の上限。
//!
//! 使用率は直前の呼び出しで得た値（`rate_limit_event`）を使う。リセット時刻を過ぎた枠は 0 とみなす。
//!
//! Copilot（copilot-cli）は使用率が分からないので、代わりに月の消費クレジットで判定する（`with_credits`）。
//! 今月（UTC の暦月）の消費を、月のクレジット × 月の経過割合 × `pace` までに抑える。

use chrono::{DateTime, Datelike, TimeDelta, Timelike, Utc};
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
        let slot = |start, end, max_five_hour| Slot {
            start,
            end,
            max_five_hour,
        };
        Self {
            timezone_offset_hours: 9,
            slots: vec![slot(3, 8, 0.20), slot(10, 15, 0.85), slot(16, 21, 0.60)],
            default_max_five_hour: 0.20,
            weekly_max: 0.70,
            pace_ahead_days: 1.0,
            max_calls_per_run: 30,
        }
    }
}

impl QuotaConfig {
    /// 設定値の範囲を確かめる。割合は 0〜1、時差は -12〜14 時間、時間帯は `start < end <= 24`。
    pub fn validate(&self) -> Result<(), String> {
        fn ratio(name: &str, v: f64) -> Result<(), String> {
            if v.is_finite() && (0.0..=1.0).contains(&v) {
                Ok(())
            } else {
                Err(format!("quota.{name} must be between 0 and 1, got {v}"))
            }
        }
        if !(-12..=14).contains(&self.timezone_offset_hours) {
            return Err(format!(
                "quota.timezone_offset_hours must be between -12 and 14, got {}",
                self.timezone_offset_hours
            ));
        }
        ratio("default_max_five_hour", self.default_max_five_hour)?;
        ratio("weekly_max", self.weekly_max)?;
        if !(self.pace_ahead_days.is_finite() && (0.0..=7.0).contains(&self.pace_ahead_days)) {
            return Err(format!(
                "quota.pace_ahead_days must be between 0 and 7, got {}",
                self.pace_ahead_days
            ));
        }
        for s in &self.slots {
            if !(s.start < s.end && s.end <= 24) {
                return Err(format!(
                    "quota slot must satisfy start < end <= 24, got {}..{}",
                    s.start, s.end
                ));
            }
            ratio("slots.max_five_hour", s.max_five_hour)
                .map_err(|e| format!("quota slot {}..{}: {e}", s.start, s.end))?;
        }
        Ok(())
    }
}

/// Copilot の AI Credits の月の予算（`[copilot_quota]`）。月の消費を、月の経過割合 × `pace` までに抑える。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreditsConfig {
    /// プランの月のクレジット（Pro は 1500）。プランで大きく違うので既定値を置かない
    pub monthly_credits: f64,
    /// 月の経過割合のうち使ってよい割合。対話で使う Copilot の消費は見えないので、その分の余裕を残す
    #[serde(default = "default_credits_pace")]
    pub pace: f64,
}

fn default_credits_pace() -> f64 {
    0.8
}

impl CreditsConfig {
    /// 月のクレジットは正、`pace` は 0 より大きく 1 以下。
    pub fn validate(&self) -> Result<(), String> {
        if !(self.monthly_credits.is_finite() && self.monthly_credits > 0.0) {
            return Err(format!(
                "copilot_quota.monthly_credits must be positive and finite, got {}",
                self.monthly_credits
            ));
        }
        if !(self.pace.is_finite() && self.pace > 0.0 && self.pace <= 1.0) {
            return Err(format!(
                "copilot_quota.pace must be greater than 0 and at most 1, got {}",
                self.pace
            ));
        }
        Ok(())
    }

    /// 今の時点で使ってよい月の消費（クレジット）。
    fn allowance(&self, now: DateTime<Utc>) -> f64 {
        let start = month_start(now);
        let next = start
            .checked_add_months(chrono::Months::new(1))
            .expect("the next month of a valid date exists");
        let elapsed = (now - start).as_seconds_f64() / (next - start).as_seconds_f64();
        self.monthly_credits * elapsed * self.pace
    }
}

/// 月の始まり（UTC の暦月の 1 日 0 時。Copilot の AI Credits はここで戻る）。
pub fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .with_day(1)
        .expect("every month has a first day")
        .and_time(chrono::NaiveTime::MIN)
        .and_utc()
}

/// あるバックエンドの月のクレジットの予算と、今月の消費。
#[derive(Debug)]
struct Credits {
    cfg: CreditsConfig,
    /// 予算を使うバックエンド（`llm_calls` の backend）。ほかのバックエンドの消費は数えない
    backend: &'static str,
    /// 今月の消費（10^-9 クレジット単位）
    used_nano: i64,
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
    /// 後段のステージのために残した回数に達した
    Reserved {
        reserved: u32,
    },
    /// 月の消費クレジットが、月の経過に応じた許容量に達した
    MonthlyCredits {
        used: f64,
        limit: f64,
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
            Stop::MonthlyCredits { used, limit } => write!(
                f,
                "monthly AI Credits {used:.1} reached the pace allowance {limit:.1}"
            ),
            Stop::Reserved { reserved } => {
                write!(
                    f,
                    "keeping the last {reserved} llm call(s) for later stages"
                )
            }
        }
    }
}

/// 1 回の実行の間、使用率と呼び出し回数を追う。
#[derive(Debug)]
pub struct Quota {
    cfg: QuotaConfig,
    usage: Option<RateLimit>,
    /// 月の消費クレジットで判定するとき（copilot-cli）の予算
    credits: Option<Credits>,
    calls: u32,
    max_calls: u32,
}

impl Quota {
    /// `usage` は直前に分かっている使用率（DB の最新の llm_calls）。`max_calls` を指定すれば
    /// 設定の `max_calls_per_run` より優先する（`crawl --max-llm-calls`）。
    pub fn new(cfg: QuotaConfig, usage: Option<RateLimit>, max_calls: Option<u32>) -> Self {
        let max_calls = max_calls.unwrap_or(cfg.max_calls_per_run);
        Self {
            cfg,
            usage,
            credits: None,
            calls: 0,
            max_calls,
        }
    }

    /// 設定のバックエンド（`llm.backend`）に合わせて判定する。claude-cli は使用率（`[quota]`）、
    /// copilot-cli は月の消費クレジット（`[copilot_quota]`）。
    pub fn from_config(config: &crate::config::Config, max_calls: Option<u32>) -> Self {
        match config.llm.backend {
            crate::config::LlmBackend::ClaudeCli => {
                Self::new(config.quota.clone(), None, max_calls)
            }
            crate::config::LlmBackend::CopilotCli => Self::with_credits(
                config.quota.clone(),
                config.copilot_quota.clone().expect(
                    "[copilot_quota] is required for copilot-cli when the config is loaded",
                ),
                crate::llm::copilot_cli::BACKEND,
                max_calls,
            ),
        }
    }

    /// `backend` の AI Credits の月の予算で判定する。呼び出し回数の上限は `cfg` の `max_calls_per_run`
    /// （`max_calls` を指定すればそちら）を使う。使用率（5 時間枠・週次枠）は見ない。
    pub fn with_credits(
        cfg: QuotaConfig,
        credits: CreditsConfig,
        backend: &'static str,
        max_calls: Option<u32>,
    ) -> Self {
        Self {
            credits: Some(Credits {
                cfg: credits,
                backend,
                used_nano: 0,
            }),
            ..Self::new(cfg, None, max_calls)
        }
    }

    /// 月の消費クレジットで判定するなら、その予算を使うバックエンド（`with_credits`）。
    pub fn credits_backend(&self) -> Option<&'static str> {
        self.credits.as_ref().map(|c| c.backend)
    }

    /// ほかの実行を含めた今月の消費（10^-9 クレジット単位、DB の llm_calls の合計）を取り込む。
    /// 月の消費クレジットで判定しないときは何もしない。
    pub fn observe_credits(&mut self, used_nano: i64) {
        if let Some(credits) = &mut self.credits {
            credits.used_nano = used_nano;
        }
    }

    /// `backend` で次の呼び出しをしてよいか。月の予算のバックエンドなら月の消費クレジットで、
    /// それ以外は使用率で判定する。呼び出し回数の上限はバックエンドをまたいで数える。
    pub fn permit(&self, backend: &str, now: DateTime<Utc>) -> Result<(), Stop> {
        if self.calls >= self.max_calls {
            return Err(Stop::MaxCalls {
                limit: self.max_calls,
            });
        }
        if let Some(credits) = self.credits.as_ref().filter(|c| c.backend == backend) {
            let used = credits.used_nano as f64 / 1e9;
            let limit = credits.cfg.allowance(now);
            if used >= limit {
                return Err(Stop::MonthlyCredits { used, limit });
            }
            return Ok(());
        }
        let usage = self.usage.unwrap_or_default();
        if let Some(w) = active(usage.five_hour, now) {
            let limit = self.five_hour_limit(now);
            if w.utilization >= limit {
                return Err(Stop::FiveHour {
                    used: w.utilization,
                    limit,
                    resets_at: w.resets_at,
                });
            }
        }
        if let Some(w) = active(usage.seven_day, now) {
            let used = w.utilization;
            if used >= self.cfg.weekly_max {
                return Err(Stop::Weekly {
                    used,
                    limit: self.cfg.weekly_max,
                });
            }
            let limit = self.pace_limit(w, now);
            if used >= limit {
                return Err(Stop::WeeklyPace { used, limit });
            }
        }
        Ok(())
    }

    /// `permit` に加え、残りの呼び出し回数が `reserve` 以下なら止める。前段のステージが
    /// 回数を使い切って、後段（採点）がいつまでも実行されない状態を防ぐ。
    pub fn permit_reserving(
        &self,
        backend: &str,
        now: DateTime<Utc>,
        reserve: u32,
    ) -> Result<(), Stop> {
        self.permit(backend, now)?;
        if self.max_calls.saturating_sub(self.calls) <= reserve {
            return Err(Stop::Reserved { reserved: reserve });
        }
        Ok(())
    }

    /// 呼び出しを 1 回行ったことと、その応答で分かった使用率を記録する。
    pub fn record_call(&mut self, usage: Option<RateLimit>) {
        self.start_call();
        self.observe(usage);
    }

    /// 呼び出しを始めたことを数える。ステージの中で呼び出しを並行させるので、応答を待たずに
    /// 始めた時点で数え、並行して判定するほかの呼び出しにも上限が効くようにする。
    pub fn start_call(&mut self) {
        self.calls += 1;
    }

    /// ほかの実行を含めて分かった最新の使用率（DB の最新の llm_calls）を取り込む。LLM を呼ぶ実行は
    /// 並行して動くので、判定の前に読んで、ほかの実行の呼び出しも判定に入れる。
    /// 並行した呼び出しの結果は順が前後するので、同じ枠の中では使用率を下げない（`RateLimit::merge`）。
    pub fn observe(&mut self, usage: Option<RateLimit>) {
        if let Some(new) = usage {
            self.usage = Some(self.usage.unwrap_or_default().merge(new));
        }
    }

    /// 現地時刻の時間帯に応じた 5 時間枠の上限。
    fn five_hour_limit(&self, now: DateTime<Utc>) -> f64 {
        let offset = chrono::FixedOffset::east_opt(self.cfg.timezone_offset_hours * 3600)
            .expect("timezone offset is validated when the config is loaded");
        let hour = now.with_timezone(&offset).hour();
        self.cfg
            .slots
            .iter()
            .find(|s| s.start <= hour && hour < s.end)
            .map_or(self.cfg.default_max_five_hour, |s| s.max_five_hour)
    }

    /// 週の経過率に `pace_ahead_days` 日分を足した割合まで、絶対上限を配分する。
    fn pace_limit(&self, week: Window, now: DateTime<Utc>) -> f64 {
        const WEEK: TimeDelta = TimeDelta::weeks(1);
        // 表せないほど遠いリセット時刻は、週の始まりとみなして最も控えめに配分する
        let elapsed = DateTime::from_timestamp(week.resets_at, 0)
            .and_then(|resets_at| resets_at.checked_sub_signed(WEEK))
            .map_or(0.0, |started| {
                (now - started).as_seconds_f64() / WEEK.as_seconds_f64()
            })
            .clamp(0.0, 1.0);
        let ahead = self.cfg.pace_ahead_days / 7.0;
        self.cfg.weekly_max * (elapsed + ahead).min(1.0)
    }
}

/// リセット時刻を過ぎた枠は、使い切った値が残っていても 0 とみなす（`None`）。
fn active(window: Option<Window>, now: DateTime<Utc>) -> Option<Window> {
    window.filter(|w| w.resets_at > now.timestamp())
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
                quota(usage(0.5, 0.1, now, 5.0))
                    .permit("claude-cli", now)
                    .is_ok(),
                allowed,
                "{time}"
            );
        }
        let now = jst("2026-09-28T03:10:00");
        assert!(matches!(
            quota(usage(0.5, 0.1, now, 5.0)).permit("claude-cli", now),
            Err(Stop::FiveHour { limit, .. }) if limit == 0.20
        ));
    }

    const NANO: i64 = 1_000_000_000;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    fn credits(monthly_credits: f64) -> CreditsConfig {
        CreditsConfig {
            monthly_credits,
            pace: 0.8,
        }
    }

    /// 月の半ばなら、月のクレジット × 0.5 × pace まで使える。月の長さによらない。
    #[test]
    fn credits_follow_the_month_pace() {
        for now in [
            "2026-09-16T00:00:00Z", // 30 日の月の 15 日経過
            "2027-02-15T00:00:00Z", // 28 日の月の 14 日経過
            "2026-12-16T12:00:00Z", // 31 日の月の 15.5 日経過
        ] {
            let now = utc(now);
            let mut q =
                Quota::with_credits(QuotaConfig::default(), credits(1000.0), "copilot-cli", None);
            assert_eq!(q.credits_backend(), Some("copilot-cli"));
            q.observe_credits(399 * NANO);
            assert!(q.permit("copilot-cli", now).is_ok(), "{now}");
            q.observe_credits(400 * NANO);
            let err = q.permit("copilot-cli", now).unwrap_err();
            assert!(
                matches!(err, Stop::MonthlyCredits { used, limit }
                    if used == 400.0 && (limit - 400.0).abs() < 1e-9),
                "{now}: {err}"
            );
        }
    }

    #[test]
    fn month_starts_on_the_first_day_in_utc() {
        assert_eq!(
            month_start(utc("2026-09-30T20:00:00Z")),
            utc("2026-09-01T00:00:00Z")
        );
        assert_eq!(
            month_start(utc("2026-10-01T00:00:00Z")),
            utc("2026-10-01T00:00:00Z")
        );
        // JST の 10 月 1 日 8 時は、UTC ではまだ 9 月
        assert_eq!(
            month_start(jst("2026-10-01T08:00:00")),
            utc("2026-09-01T00:00:00Z")
        );
    }

    #[test]
    fn follows_the_configured_backend() {
        let mut config = crate::config::Config::default();
        assert_eq!(Quota::from_config(&config, None).credits_backend(), None);
        config.llm.backend = crate::config::LlmBackend::CopilotCli;
        config.copilot_quota = Some(credits(1000.0));
        let q = Quota::from_config(&config, Some(2));
        assert_eq!(q.credits_backend(), Some("copilot-cli"));
        assert_eq!(q.max_calls, 2);
    }

    /// クレジットで判定するときは、Claude の使用率を見ない。
    #[test]
    fn credits_ignore_subscription_usage() {
        let now = utc("2026-09-16T00:00:00Z");
        let mut q =
            Quota::with_credits(QuotaConfig::default(), credits(1000.0), "copilot-cli", None);
        q.observe(Some(usage(1.0, 1.0, now, 6.9)));
        q.observe_credits(0);
        assert!(q.permit("copilot-cli", now).is_ok());
        assert_eq!(
            Quota::new(QuotaConfig::default(), None, None).credits_backend(),
            None
        );
    }

    #[test]
    fn credits_still_limit_calls_per_run() {
        let now = utc("2026-09-16T00:00:00Z");
        let mut q = Quota::with_credits(
            QuotaConfig::default(),
            credits(1000.0),
            "copilot-cli",
            Some(1),
        );
        q.observe_credits(0);
        assert!(q.permit("copilot-cli", now).is_ok());
        q.record_call(None);
        assert_eq!(
            q.permit("copilot-cli", now),
            Err(Stop::MaxCalls { limit: 1 })
        );
    }

    #[test]
    fn expired_windows_count_as_unused() {
        let now = jst("2026-09-28T22:00:00");
        let u = RateLimit {
            five_hour: window(0.99, now - chrono::Duration::minutes(1)),
            seven_day: window(0.99, now - chrono::Duration::minutes(1)),
        };
        assert!(quota(u).permit("claude-cli", now).is_ok());
    }

    #[test]
    fn unknown_usage_is_allowed() {
        let q = Quota::new(QuotaConfig::default(), None, None);
        assert!(q.permit("claude-cli", jst("2026-09-28T22:00:00")).is_ok());
    }

    #[test]
    fn weekly_absolute_limit() {
        let now = jst("2026-09-28T10:30:00");
        let err = quota(usage(0.1, 0.71, now, 6.9))
            .permit("claude-cli", now)
            .unwrap_err();
        assert!(matches!(err, Stop::Weekly { .. }), "{err}");
    }

    #[test]
    fn weekly_pace_allows_one_day_ahead_of_linear() {
        let now = jst("2026-09-28T10:30:00");
        // 経過 0.7 日（10%）：許容は 70% × (0.1 + 1/7) ≈ 17%
        assert!(
            quota(usage(0.1, 0.15, now, 0.7))
                .permit("claude-cli", now)
                .is_ok()
        );
        let err = quota(usage(0.1, 0.20, now, 0.7))
            .permit("claude-cli", now)
            .unwrap_err();
        assert!(matches!(err, Stop::WeeklyPace { .. }), "{err}");
        // 週の始めでも 1 日分は使える
        assert!(
            quota(usage(0.1, 0.05, now, 0.0))
                .permit("claude-cli", now)
                .is_ok()
        );
    }

    /// 応答に含まれない枠は、それまでの値を残す。
    #[test]
    fn record_call_keeps_windows_missing_from_response() {
        let now = jst("2026-09-28T10:30:00");
        let mut q = Quota::new(
            QuotaConfig::default(),
            Some(usage(0.1, 0.71, now, 6.9)),
            None,
        );
        q.record_call(Some(RateLimit {
            five_hour: window(0.2, now + chrono::Duration::hours(2)),
            seven_day: None,
        }));
        assert!(matches!(
            q.permit("claude-cli", now),
            Err(Stop::Weekly { .. })
        ));
    }

    #[test]
    fn reserving_keeps_calls_for_later_stages() {
        let now = jst("2026-09-28T10:30:00");
        let mut q = Quota::new(QuotaConfig::default(), None, Some(3));
        assert!(q.permit_reserving("claude-cli", now, 1).is_ok());
        q.record_call(None);
        assert!(q.permit_reserving("claude-cli", now, 1).is_ok());
        q.record_call(None);
        assert_eq!(
            q.permit_reserving("claude-cli", now, 1),
            Err(Stop::Reserved { reserved: 1 })
        );
        // 後段は残した回数を使える
        assert!(q.permit("claude-cli", now).is_ok());
        // ほかの理由（5 時間枠など）で止まるときは、そちらを返す
        let q = Quota::new(
            QuotaConfig::default(),
            Some(usage(0.9, 0.1, now, 5.0)),
            Some(3),
        );
        assert!(matches!(
            q.permit_reserving("claude-cli", now, 1),
            Err(Stop::FiveHour { .. })
        ));
    }

    #[test]
    fn stops_after_max_calls_and_observes_new_usage() {
        let now = jst("2026-09-28T10:30:00");
        let mut q = Quota::new(
            QuotaConfig::default(),
            Some(usage(0.1, 0.1, now, 5.0)),
            Some(3),
        );
        q.record_call(None);
        assert!(q.permit("claude-cli", now).is_ok());
        // 応答で使用率が上がれば、それに従って止まる
        q.record_call(Some(usage(0.9, 0.1, now, 5.0)));
        assert!(matches!(
            q.permit("claude-cli", now),
            Err(Stop::FiveHour { .. })
        ));
        let mut q = Quota::new(
            QuotaConfig::default(),
            Some(usage(0.1, 0.1, now, 5.0)),
            Some(1),
        );
        q.record_call(Some(usage(0.1, 0.1, now, 5.0)));
        assert_eq!(
            q.permit("claude-cli", now),
            Err(Stop::MaxCalls { limit: 1 })
        );
    }

    /// ほかの実行が記録した使用率を取り込めば、それで判定する。
    #[test]
    fn observed_usage_from_other_runs_counts() {
        let now = jst("2026-09-28T11:00:00");
        let mut q = Quota::new(QuotaConfig::default(), None, None);
        assert!(q.permit("claude-cli", now).is_ok());
        q.observe(Some(usage(0.9, 0.1, now, 3.0)));
        assert!(
            matches!(q.permit("claude-cli", now), Err(Stop::FiveHour { .. })),
            "{:?}",
            q.permit("claude-cli", now)
        );
        // 取り込めるものが無ければ、それまでの値を残す
        q.observe(None);
        assert!(q.permit("claude-cli", now).is_err());
    }

    /// 並行した呼び出しの結果は順が前後するので、同じ枠（リセット時刻が同じ）の中では使用率を
    /// 下げない。新しい枠なら置き換え、古い枠は無視する。
    #[test]
    fn observed_usage_never_drops_within_a_window() {
        let now = jst("2026-09-28T11:00:00");
        let resets = now + chrono::Duration::hours(2);
        let five = |utilization, resets_at| RateLimit {
            five_hour: window(utilization, resets_at),
            seven_day: None,
        };
        let mut q = Quota::new(QuotaConfig::default(), Some(five(0.9, resets)), None);
        q.observe(Some(five(0.2, resets)));
        assert!(
            q.permit("claude-cli", now).is_err(),
            "a stale lower reading is ignored"
        );
        q.observe(Some(five(0.1, resets - chrono::Duration::hours(5))));
        assert!(
            q.permit("claude-cli", now).is_err(),
            "an older window is ignored"
        );
        q.record_call(Some(five(0.3, resets)));
        assert!(
            q.permit("claude-cli", now).is_err(),
            "own stale response is ignored too"
        );
        q.observe(Some(five(0.1, resets + chrono::Duration::hours(5))));
        assert!(
            q.permit("claude-cli", now).is_ok(),
            "a newer window replaces it"
        );
    }
}
