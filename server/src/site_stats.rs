//! Aggregate, cookie-free counts for the public website.
//!
//! The website does not load a third-party analytics script and does not set
//! cookies. These counters only answer "how many times was a page opened or a
//! download requested per day"; they deliberately store no IP address, user
//! agent, referrer or any other visitor identifier. Rows are one per day and
//! metric, so the table grows by a handful of rows per day.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, params};

pub const HOME_VIEW: &str = "home_view";
pub const PRIVACY_VIEW: &str = "privacy_view";
pub const DOWNLOAD_CLICK_MACOS: &str = "download_click_macos";
pub const DOWNLOAD_CLICK_WINDOWS: &str = "download_click_windows";
pub const DOWNLOAD_FILE_MACOS: &str = "download_file_macos";
pub const DOWNLOAD_FILE_WINDOWS: &str = "download_file_windows";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DailyCounts {
    pub home_views: u64,
    pub privacy_views: u64,
    pub download_clicks_macos: u64,
    pub download_clicks_windows: u64,
    pub downloads_macos: u64,
    pub downloads_windows: u64,
}

impl DailyCounts {
    pub fn download_clicks(self) -> u64 {
        self.download_clicks_macos + self.download_clicks_windows
    }

    pub fn downloads(self) -> u64 {
        self.downloads_macos + self.downloads_windows
    }
}

impl std::ops::Add for DailyCounts {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            home_views: self.home_views + other.home_views,
            privacy_views: self.privacy_views + other.privacy_views,
            download_clicks_macos: self.download_clicks_macos + other.download_clicks_macos,
            download_clicks_windows: self.download_clicks_windows + other.download_clicks_windows,
            downloads_macos: self.downloads_macos + other.downloads_macos,
            downloads_windows: self.downloads_windows + other.downloads_windows,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRow {
    pub day: String,
    pub counts: DailyCounts,
}

/// The operator-facing day boundary follows the dashboard time zone.
pub fn day_for(now_ms: i64, time_zone: Tz) -> String {
    DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch"))
        .with_timezone(&time_zone)
        .format("%Y-%m-%d")
        .to_string()
}

pub fn record(conn: &Connection, day: &str, metric: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO site_daily_counts (day, metric, count) VALUES (?1, ?2, 1)
         ON CONFLICT(day, metric) DO UPDATE SET count = count + 1",
        params![day, metric],
    )?;
    Ok(())
}

/// Returns one row per day, oldest first, including days with no requests.
/// `days` is clamped to a sane range so a bad caller cannot make a huge page.
pub fn report(
    conn: &Connection,
    now_ms: i64,
    time_zone: Tz,
    days: usize,
) -> rusqlite::Result<Vec<DayRow>> {
    let days = days.clamp(1, 366);
    let today = DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch"))
        .with_timezone(&time_zone)
        .date_naive();
    let requested_first = today - Duration::days(i64::try_from(days).unwrap_or(366) - 1);
    // Do not pad the table with weeks of zeros before the feature was first
    // deployed on this server. Start at the earliest stored day, or today when
    // nothing has been recorded yet.
    let earliest: Option<String> = conn
        .query_row("SELECT MIN(day) FROM site_daily_counts", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten();
    let first = earliest
        .and_then(|day| chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").ok())
        .map_or(today, |day| day.max(requested_first));
    let first_day = first.format("%Y-%m-%d").to_string();

    let mut statement = conn.prepare(
        "SELECT day,
                COALESCE(SUM(CASE WHEN metric = ?2 THEN count END), 0),
                COALESCE(SUM(CASE WHEN metric = ?3 THEN count END), 0),
                COALESCE(SUM(CASE WHEN metric = ?4 THEN count END), 0),
                COALESCE(SUM(CASE WHEN metric = ?5 THEN count END), 0),
                COALESCE(SUM(CASE WHEN metric = ?6 THEN count END), 0),
                COALESCE(SUM(CASE WHEN metric = ?7 THEN count END), 0)
         FROM site_daily_counts
         WHERE day >= ?1
         GROUP BY day",
    )?;
    let mut rows = statement.query(params![
        first_day,
        HOME_VIEW,
        PRIVACY_VIEW,
        DOWNLOAD_CLICK_MACOS,
        DOWNLOAD_CLICK_WINDOWS,
        DOWNLOAD_FILE_MACOS,
        DOWNLOAD_FILE_WINDOWS
    ])?;
    let mut stored = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let day: String = row.get(0)?;
        let counts = DailyCounts {
            home_views: count(row.get(1)?),
            privacy_views: count(row.get(2)?),
            download_clicks_macos: count(row.get(3)?),
            download_clicks_windows: count(row.get(4)?),
            downloads_macos: count(row.get(5)?),
            downloads_windows: count(row.get(6)?),
        };
        stored.insert(day, counts);
    }

    let length = usize::try_from((today - first).num_days() + 1).unwrap_or(1);
    let mut result = Vec::with_capacity(length);
    for offset in 0..length {
        let date = first + Duration::days(i64::try_from(offset).unwrap_or(0));
        let day = date.format("%Y-%m-%d").to_string();
        result.push(DayRow {
            counts: stored.get(&day).copied().unwrap_or_default(),
            day,
        });
    }
    Ok(result)
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn day(offset_days: i64) -> String {
        (chrono::NaiveDate::from_ymd_opt(2026, 11, 1).expect("date") + Duration::days(offset_days))
            .format("%Y-%m-%d")
            .to_string()
    }

    #[test]
    fn counts_are_aggregated_by_day_and_metric() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = db::open(&dir.path().join("stats.sqlite")).expect("open");
        for _ in 0..3 {
            record(&opened.conn, &day(0), HOME_VIEW).expect("home");
        }
        record(&opened.conn, &day(0), DOWNLOAD_CLICK_MACOS).expect("click");
        record(&opened.conn, &day(1), DOWNLOAD_FILE_WINDOWS).expect("download");

        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 3)
            .expect("date")
            .and_hms_opt(12, 0, 0)
            .expect("time")
            .and_utc()
            .timestamp_millis();
        let rows = report(&opened.conn, now, crate::DEFAULT_TIME_ZONE, 3).expect("report");
        assert_eq!(rows.len(), 3);
        let by_day = |name: &str| rows.iter().find(|row| row.day == name).expect("day").counts;
        assert_eq!(by_day(&day(0)).home_views, 3);
        assert_eq!(by_day(&day(0)).download_clicks_macos, 1);
        assert_eq!(by_day(&day(1)).downloads_windows, 1);
        assert_eq!(by_day(&day(2)), DailyCounts::default());
    }

    #[test]
    fn an_empty_report_starts_at_today_instead_of_padding_zeros() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = db::open(&dir.path().join("stats.sqlite")).expect("open");
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 19)
            .expect("date")
            .and_hms_opt(12, 0, 0)
            .expect("time")
            .and_utc()
            .timestamp_millis();
        let rows = report(&opened.conn, now, crate::DEFAULT_TIME_ZONE, 30).expect("report");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].day, "2026-11-19");
        assert_eq!(rows[0].counts, DailyCounts::default());
    }

    #[test]
    fn day_boundary_uses_the_configured_time_zone() {
        // 2026-11-19 00:30 Beijing is still 2026-11-18 in UTC.
        let ms = 1_795_019_400_000;
        assert_eq!(day_for(ms, crate::DEFAULT_TIME_ZONE), "2026-11-19");
        assert_eq!(day_for(ms, chrono_tz::UTC), "2026-11-18");
    }
}
