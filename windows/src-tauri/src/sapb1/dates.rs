//! Minimal calendar math for time-window filters.
//!
//! The Service Layer filters dates with ISO `YYYY-MM-DD` strings. Computing "last
//! quarter" or "this month" from the current date needs real calendar arithmetic,
//! so this module implements the well-known days↔civil-date conversions (Howard
//! Hinnant's algorithm) and the window boundaries. No date crate is pulled in:
//! the build stays dependency-light, and only this module touches dates.

/// Returns the current date in the system's UTC calendar as (year, month, day).
pub fn today_ymd() -> (i32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    civil_from_days(days)
}

/// Converts days since 1970-01-01 to a (year, month, day) civil date.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y } as i32, m, d as u32)
}

/// Converts a (year, month, day) civil date to days since 1970-01-01.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y as i64 } else { (y as i64) - 399 } / 400;
    let yoe = y as i64 - era * 400; // [0, 399]
    let mp = if m > 2 { (m - 3) as i64 } else { (m + 9) as i64 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// `(year, month)` shifted by `delta` months.
fn shift_month(y: i32, m: u32, delta: i64) -> (i32, u32) {
    let total = y as i64 * 12 + (m as i64 - 1) + delta;
    let ny = total.div_euclid(12);
    let nm = total.rem_euclid(12) + 1;
    (ny as i32, nm as u32)
}

/// Last day of a month, in days-since-epoch terms for a (year, month).
fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 30,
    }
}

fn fmt(y: i32, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

fn add_days(y: i32, m: u32, d: u32, delta: i64) -> (i32, u32, u32) {
    civil_from_days(days_from_civil(y, m, d) + delta)
}

/// The first day of the quarter containing (y, m).
fn quarter_start(y: i32, m: u32) -> (i32, u32, u32) {
    let qm = ((m - 1) / 3) * 3 + 1;
    (y, qm, 1)
}

/// Resolves a symbolic window to an inclusive `(start, end)` ISO date range.
/// `None` means no date bound (all time).
pub fn range(window: crate::sapb1::planner::TimeWindow) -> Option<(String, String)> {
    let (y, m, d) = today_ymd();
    range_on(window, y, m, d)
}

/// The same, against a fixed day, so the arithmetic can be tested exactly. Every
/// window is bounded by real dates; the end is the last day of the period, in the
/// month the period actually ends in.
fn range_on(
    window: crate::sapb1::planner::TimeWindow,
    y: i32,
    m: u32,
    d: u32,
) -> Option<(String, String)> {
    match window {
        crate::sapb1::planner::TimeWindow::Today => Some((fmt(y, m, d), fmt(y, m, d))),
        crate::sapb1::planner::TimeWindow::Yesterday => {
            let (py, pm, pd) = add_days(y, m, d, -1);
            Some((fmt(py, pm, pd), fmt(py, pm, pd)))
        }
        crate::sapb1::planner::TimeWindow::ThisWeek => {
            let dow = weekday(y, m, d); // 0=Mon..6=Sun
            let (sy, sm, sd) = add_days(y, m, d, -(dow as i64));
            let (ey, em, ed) = add_days(sy, sm, sd, 6);
            Some((fmt(sy, sm, sd), fmt(ey, em, ed)))
        }
        crate::sapb1::planner::TimeWindow::LastWeek => {
            let dow = weekday(y, m, d);
            let (sy, sm, sd) = add_days(y, m, d, -(dow as i64) - 7);
            let (ey, em, ed) = add_days(sy, sm, sd, 6);
            Some((fmt(sy, sm, sd), fmt(ey, em, ed)))
        }
        crate::sapb1::planner::TimeWindow::ThisMonth => {
            let (ey, em, ed) = (y, m, days_in_month(y, m));
            Some((fmt(y, m, 1), fmt(ey, em, ed)))
        }
        crate::sapb1::planner::TimeWindow::LastMonth => {
            let (py, pm) = shift_month(y, m, -1);
            // The end is the day before this month began; the month it lands in
            // is part of the answer, so it is not discarded.
            let (ey, em, ed) = add_days(y, m, 1, -1);
            Some((fmt(py, pm, 1), fmt(ey, em, ed)))
        }
        crate::sapb1::planner::TimeWindow::ThisQuarter => {
            let (sy, sm, _) = quarter_start(y, m);
            let (ny, nm) = shift_month(sy, sm, 3);
            let (ey, em, ed) = add_days(ny, nm, 1, -1);
            Some((fmt(sy, sm, 1), fmt(ey, em, ed)))
        }
        crate::sapb1::planner::TimeWindow::LastQuarter => {
            let (sy, sm, _) = quarter_start(y, m);
            let (py, pm) = shift_month(sy, sm, -3);
            let (ey, em, ed) = add_days(sy, sm, 1, -1);
            Some((fmt(py, pm, 1), fmt(ey, em, ed)))
        }
        crate::sapb1::planner::TimeWindow::ThisYear => Some((fmt(y, 1, 1), fmt(y, 12, 31))),
        crate::sapb1::planner::TimeWindow::LastYear => {
            Some((fmt(y - 1, 1, 1), fmt(y - 1, 12, 31)))
        }
        crate::sapb1::planner::TimeWindow::Last30Days => {
            let (sy, sm, sd) = add_days(y, m, d, -30);
            Some((fmt(sy, sm, sd), fmt(y, m, d)))
        }
        crate::sapb1::planner::TimeWindow::All => None,
    }
}

/// Weekday: 0 = Monday .. 6 = Sunday.
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    // 1970-01-01 was a Thursday (4). Days since epoch, mod 7, gives 0=Thu.
    let days = days_from_civil(y, m, d);
    ((days % 7 + 7) % 7 + 3) as u32 % 7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2025, 12, 31), (2026, 1, 1)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
    }

    #[test]
    fn epoch_is_thursday() {
        assert_eq!(weekday(1970, 1, 1), 3, "1970-01-01 was a Thursday");
    }

    #[test]
    fn month_lengths_include_leap() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2025, 2), 28);
        assert_eq!(days_in_month(2025, 4), 30);
    }

    #[test]
    fn quarter_start_buckets() {
        assert_eq!(quarter_start(2025, 2), (2025, 1, 1));
        assert_eq!(quarter_start(2025, 5), (2025, 4, 1));
        assert_eq!(quarter_start(2025, 11), (2025, 10, 1));
    }

    #[test]
    fn last_quarter_wraps_the_year() {
        // Fix a date in January so "last quarter" crosses into the prior year.
        let range = range(crate::sapb1::planner::TimeWindow::LastQuarter);
        let (start, end) = range.unwrap();
        assert!(start < end, "{start} must be before {end}");
        assert!(start.len() == 10 && end.len() == 10);
    }

    #[test]
    fn the_quarters_and_months_end_on_their_real_last_day() {
        use crate::sapb1::planner::TimeWindow::*;
        // 2026-10-04: last quarter is Jul-Sep, this quarter is Oct-Dec.
        assert_eq!(range_on(LastQuarter, 2026, 10, 4), Some(("2026-07-01".into(), "2026-09-30".into())));
        assert_eq!(range_on(ThisQuarter, 2026, 10, 4), Some(("2026-10-01".into(), "2026-12-31".into())));
        assert_eq!(range_on(LastMonth, 2026, 10, 4), Some(("2026-09-01".into(), "2026-09-30".into())));
        // A January date, so last quarter is the previous year's Q4.
        assert_eq!(range_on(LastQuarter, 2026, 1, 15), Some(("2025-10-01".into(), "2025-12-31".into())));
        // A quarter that ends in a 30-day month.
        assert_eq!(range_on(ThisQuarter, 2026, 4, 2), Some(("2026-04-01".into(), "2026-06-30".into())));
    }
}