//! Core of domestique, shared by the command line and the desktop app.

pub mod config;
pub mod fit;
pub mod garmin;
pub mod mywhoosh;
pub mod sync;

/// Send + Sync so errors can cross from a worker thread to the UI.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Unix time of a UTC "YYYY-MM-DDTHH:MM:SS..." or "YYYY-MM-DD HH:MM:SS".
pub fn parse_utc(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    // Days since 1970-01-01, Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?)
}

/// "YYYY-MM-DD HH:MM UTC" of a Unix time.
pub fn utc_string(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        secs / 3600,
        secs % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        // The ride's FIT start_time, 1161110723 s after the FIT epoch (Unix 631065600).
        let start = 1_161_110_723 + 631_065_600;
        assert_eq!(parse_utc("2026-10-16T18:45:23.000Z"), Some(start)); // MyWhoosh
        assert_eq!(parse_utc("2026-10-16 18:45:23"), Some(start)); // Garmin
        assert_eq!(parse_utc("2024-02-29 00:00:00"), Some(1_709_164_800));
        assert_eq!(utc_string(start), "2026-10-16 18:45 UTC");
        assert_eq!(
            utc_string(1_709_164_800 + 13 * 3600 + 5 * 60),
            "2024-02-29 13:05 UTC"
        );
    }
}
