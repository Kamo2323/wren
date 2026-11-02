//! Stats (spec §9), computed only from `hourly_totals`, so they survive the 72-hour sweep.
//! Everything is keyed by local calendar date, so the maths needs only "today" — clock changes
//! (BST) were already settled when each dictation's local date and hour were stored.

use chrono::{Datelike, Duration, NaiveDate};
use serde::Serialize;

use crate::constants::{STATS_WINDOW_DAYS, TYPING_WPM_DEFAULT};

/// One `hourly_totals` row.
#[derive(Debug, Clone, PartialEq)]
pub struct HourRow {
    pub date: NaiveDate,
    pub hour: u8,
    pub dictations: u64,
    pub words: u64,
    pub speaking_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayPoint {
    /// `YYYY-MM-DD`, local.
    pub date: String,
    pub words: u64,
    /// Speaking time that day (sum of `hourly_totals.speaking_ms`), for the words-per-minute trend.
    pub speaking_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub time_saved_ms_all: u64,
    /// This week = Monday to today (local).
    pub time_saved_ms_week: u64,
    pub wpm_today: Option<f32>,
    pub wpm_all: Option<f32>,
    pub streak_days: u32,
    /// Local hour (0–23) with the most words in the last 30 days.
    pub busiest_hour: Option<u8>,
    pub words_today: u64,
    pub words_week: u64,
    pub words_all: u64,
    pub dictations_all: u64,
    /// Last 30 days, oldest first, zero-filled.
    pub daily: Vec<DayPoint>,
    /// 24 entries: words per local hour over the last 30 days.
    pub hourly: Vec<u64>,
}

/// Words per minute; `None` when there is no speaking time to divide by.
fn wpm(words: u64, speaking_ms: u64) -> Option<f32> {
    (speaking_ms > 0).then(|| (words as f64 * 60_000.0 / speaking_ms as f64) as f32)
}

/// max(0, words ÷ typing_wpm − speaking minutes), in milliseconds, rounded.
fn time_saved_ms(words: u64, speaking_ms: u64, typing_wpm: u32) -> u64 {
    let typing_ms = words as f64 * 60_000.0 / typing_wpm as f64;
    (typing_ms - speaking_ms as f64).max(0.0).round() as u64
}

/// Spec §9 stats from `hourly_totals` rows. `today` is the local calendar date.
pub fn compute(rows: &[HourRow], today: NaiveDate, typing_wpm: u32) -> Stats {
    let typing_wpm = if typing_wpm == 0 { TYPING_WPM_DEFAULT } else { typing_wpm };
    let week_start = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let window_start = today - Duration::days(STATS_WINDOW_DAYS - 1);

    let (mut words_all, mut ms_all, mut dictations_all) = (0u64, 0u64, 0u64);
    let (mut words_today, mut ms_today) = (0u64, 0u64);
    let (mut words_week, mut ms_week) = (0u64, 0u64);
    let mut hourly = vec![0u64; 24];
    // per local date in the window: (words, speaking ms)
    let mut per_day: std::collections::BTreeMap<NaiveDate, (u64, u64)> = Default::default();
    let mut active_days = std::collections::HashSet::new();

    for r in rows {
        words_all += r.words;
        ms_all += r.speaking_ms;
        dictations_all += r.dictations;
        if r.dictations > 0 {
            active_days.insert(r.date);
        }
        if r.date == today {
            words_today += r.words;
            ms_today += r.speaking_ms;
        }
        if r.date >= week_start && r.date <= today {
            words_week += r.words;
            ms_week += r.speaking_ms;
        }
        if r.date >= window_start && r.date <= today {
            if let Some(slot) = hourly.get_mut(r.hour as usize) {
                *slot += r.words;
            }
            let day = per_day.entry(r.date).or_default();
            day.0 += r.words;
            day.1 += r.speaking_ms;
        }
    }

    // Day streak: consecutive local days with a dictation, ending today or yesterday.
    let yesterday = today - Duration::days(1);
    let mut streak_days = 0u32;
    let mut day = if active_days.contains(&today) { Some(today) } else { Some(yesterday) };
    while let Some(d) = day.filter(|d| active_days.contains(d)) {
        streak_days += 1;
        day = d.pred_opt();
    }

    // Busiest hour: most words in the window; the earliest hour wins a tie.
    let busiest_hour = hourly
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 0)
        .fold(None::<(usize, u64)>, |best, (h, &w)| match best {
            Some((_, bw)) if bw >= w => best,
            _ => Some((h, w)),
        })
        .map(|(h, _)| h as u8);

    let daily = (0..STATS_WINDOW_DAYS)
        .rev()
        .map(|back| {
            let date = today - Duration::days(back);
            let (words, speaking_ms) = per_day.get(&date).copied().unwrap_or((0, 0));
            DayPoint { date: date.format("%Y-%m-%d").to_string(), words, speaking_ms }
        })
        .collect();

    Stats {
        time_saved_ms_all: time_saved_ms(words_all, ms_all, typing_wpm),
        time_saved_ms_week: time_saved_ms(words_week, ms_week, typing_wpm),
        wpm_today: wpm(words_today, ms_today),
        wpm_all: wpm(words_all, ms_all),
        streak_days,
        busiest_hour,
        words_today,
        words_week,
        words_all,
        dictations_all,
        daily,
        hourly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    fn row(date: &str, hour: u8, dictations: u64, words: u64, speaking_ms: u64) -> HourRow {
        HourRow { date: d(date), hour, dictations, words, speaking_ms }
    }

    /// Hand-computed fixture. Today is Wednesday 4 Nov 2026, so "this week" is 2–4 Nov.
    fn fixture() -> Vec<HourRow> {
        vec![
            row("2026-11-04", 9, 2, 120, 60_000),
            row("2026-11-04", 14, 1, 30, 30_000),
            row("2026-11-03", 9, 1, 52, 20_000),
            row("2026-11-01", 20, 1, 260, 120_000), // Sunday: last week
            row("2026-09-01", 3, 5, 1_000, 600_000), // older than 30 days
        ]
    }

    #[test]
    fn totals_wpm_and_time_saved_match_hand_computed_values() {
        let s = compute(&fixture(), d("2026-11-04"), 52);
        assert_eq!(s.words_all, 1_462);
        assert_eq!(s.dictations_all, 10);
        assert_eq!(s.words_today, 150);
        assert_eq!(s.words_week, 202);
        // today: 150 words in 1.5 speaking minutes
        assert_eq!(s.wpm_today, Some(100.0));
        // all: 1462 words / (830 000 ms = 13.8333 min) = 105.687
        assert!((s.wpm_all.unwrap() - 105.687).abs() < 0.01, "{:?}", s.wpm_all);
        // all: 1462 / 52 min = 1 686 923.08 ms typing − 830 000 ms speaking = 856 923 ms
        assert_eq!(s.time_saved_ms_all, 856_923);
        // week: 202 / 52 min = 233 076.92 ms − 110 000 ms = 123 077 ms
        assert_eq!(s.time_saved_ms_week, 123_077);
    }

    #[test]
    fn streak_busiest_hour_and_series() {
        let s = compute(&fixture(), d("2026-11-04"), 52);
        // 4th and 3rd have dictations, 2nd does not.
        assert_eq!(s.streak_days, 2);
        // In the last 30 days: 09h = 172, 14h = 30, 20h = 260. The old 03h (1000 words) is outside.
        assert_eq!(s.busiest_hour, Some(20));
        assert_eq!(s.hourly.len(), 24);
        assert_eq!((s.hourly[9], s.hourly[14], s.hourly[20], s.hourly[3]), (172, 30, 260, 0));
        assert_eq!(s.hourly.iter().sum::<u64>(), 462);

        assert_eq!(s.daily.len(), 30);
        assert_eq!(s.daily.first().unwrap().date, "2026-10-06");
        assert_eq!(s.daily.last().unwrap(), &DayPoint { date: "2026-11-04".into(), words: 150, speaking_ms: 90_000 });
        let words = |date: &str| s.daily.iter().find(|p| p.date == date).unwrap().words;
        assert_eq!((words("2026-11-03"), words("2026-11-02"), words("2026-11-01")), (52, 0, 260));
        assert_eq!(s.daily.iter().map(|p| p.words).sum::<u64>(), 462);
    }

    #[test]
    fn daily_points_carry_speaking_time_per_local_date() {
        let s = compute(&fixture(), d("2026-11-04"), 52);
        let day = |date: &str| s.daily.iter().find(|p| p.date == date).unwrap().clone();
        // 4 Nov: two hours summed (60 000 + 30 000); 3 Nov: one hour; 2 Nov: nothing spoken.
        assert_eq!(day("2026-11-04").speaking_ms, 90_000);
        assert_eq!(day("2026-11-03").speaking_ms, 20_000);
        assert_eq!(day("2026-11-02").speaking_ms, 0);
        assert_eq!(day("2026-11-01").speaking_ms, 120_000);
        // the 1 Sep row is outside the 30-day window, so the series only sums the window
        assert_eq!(s.daily.iter().map(|p| p.speaking_ms).sum::<u64>(), 230_000);
        // serialised for the UI as camelCase
        let json = serde_json::to_string(&day("2026-11-03")).unwrap();
        assert_eq!(json, r#"{"date":"2026-11-03","words":52,"speakingMs":20000}"#);
    }

    #[test]
    fn streak_may_end_yesterday_but_not_earlier() {
        let rows = vec![row("2026-11-02", 23, 1, 5, 1_000), row("2026-11-03", 0, 1, 5, 1_000)];
        assert_eq!(compute(&rows, d("2026-11-03"), 52).streak_days, 2);
        assert_eq!(compute(&rows, d("2026-11-04"), 52).streak_days, 2);
        assert_eq!(compute(&rows, d("2026-11-05"), 52).streak_days, 0);
    }

    #[test]
    fn streak_crosses_month_and_year_ends() {
        let rows = vec![
            row("2026-12-31", 12, 1, 1, 1_000),
            row("2027-01-01", 12, 1, 1, 1_000),
            row("2027-01-02", 12, 1, 1, 1_000),
        ];
        assert_eq!(compute(&rows, d("2027-01-02"), 52).streak_days, 3);
    }

    #[test]
    fn time_saved_never_goes_negative() {
        let rows = vec![row("2026-11-04", 10, 1, 10, 600_000)];
        let s = compute(&rows, d("2026-11-04"), 52);
        assert_eq!((s.time_saved_ms_all, s.time_saved_ms_week), (0, 0));
    }

    #[test]
    fn week_starts_on_monday() {
        // Monday 2 Nov: the Sunday before is last week.
        let rows = vec![row("2026-11-01", 10, 1, 10, 1_000), row("2026-11-02", 10, 1, 7, 1_000)];
        assert_eq!(compute(&rows, d("2026-11-02"), 52).words_week, 7);
    }

    #[test]
    fn a_zero_typing_speed_falls_back_to_the_default() {
        let rows = vec![row("2026-11-04", 10, 1, 52, 0)];
        let s = compute(&rows, d("2026-11-04"), 0);
        assert_eq!(s.time_saved_ms_all, 60_000);
    }

    #[test]
    fn no_data_gives_empty_but_well_formed_stats() {
        let s = compute(&[], d("2026-11-04"), 52);
        assert_eq!((s.words_all, s.dictations_all, s.streak_days), (0, 0, 0));
        assert_eq!((s.wpm_today, s.wpm_all, s.busiest_hour), (None, None, None));
        assert_eq!(s.daily.len(), 30);
        assert!(s.daily.iter().all(|p| p.words == 0));
        assert_eq!(s.hourly, vec![0; 24]);
    }
}
