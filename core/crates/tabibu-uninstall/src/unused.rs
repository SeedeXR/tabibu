//! Unused apps: bundles not opened for a long time (per Spotlight metadata).

use crate::fsutil::size_of;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tabibu_engine::{CancelToken, Category, CleanupItem, SafetyTier, ScanCtx, ScanError, Scanner};

/// An app is reported only after this much time without being opened.
const UNUSED_AFTER: Duration = Duration::from_secs(180 * 24 * 60 * 60);

const SECS_PER_DAY: i64 = 86_400;

/// Flags apps not opened for more than 180 days (or never opened, when the
/// bundle itself is older than 180 days). Last-used dates are supplied by
/// the shell (Spotlight / [`last_used`]) — this scanner never guesses.
///
/// Apple apps (`com.apple.*`), Tabibu itself, and currently running apps are
/// never emitted. Every hit is [`SafetyTier::Risky`].
#[derive(Debug)]
pub struct UnusedAppScanner {
    apps: Vec<(PathBuf, String, Option<SystemTime>)>,
}

impl UnusedAppScanner {
    /// `apps` is `(app path, bundle id, last-used date)`; `None` means
    /// Spotlight has no record of the app ever being opened.
    #[must_use]
    pub fn new(apps: Vec<(PathBuf, String, Option<SystemTime>)>) -> Self {
        Self { apps }
    }
}

impl Scanner for UnusedAppScanner {
    fn id(&self) -> &'static str {
        "unused_app"
    }

    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError> {
        let now = SystemTime::now();
        for (path, bundle_id, used) in &self.apps {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            if bundle_id.starts_with("com.apple.")
                || is_tabibu(path, bundle_id)
                || ctx.running_bundle_ids.contains(bundle_id)
            {
                continue;
            }
            let created = fs::metadata(path).ok().and_then(|meta| meta.created().ok());
            if let Some(reason) = unused_reason(*used, created, now) {
                let size = size_of(path);
                sink(CleanupItem::new(
                    path.clone(),
                    Category::UnusedApp,
                    size,
                    SafetyTier::Risky,
                    reason,
                ));
            }
        }
        Ok(())
    }
}

/// Last-used date of a file per Spotlight, by shelling out to
/// `/usr/bin/mdls -raw -name kMDItemLastUsedDate <path>`.
///
/// Returns `None` on `(null)`, a non-zero exit, or any parse failure —
/// never guesses. Intended for the shell layer; scanners receive the result
/// pre-resolved so scanning stays hermetic.
#[must_use]
pub fn last_used(path: &Path) -> Option<SystemTime> {
    let output = Command::new("/usr/bin/mdls")
        .args(["-raw", "-name", "kMDItemLastUsedDate"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_mdls_date(std::str::from_utf8(&output.stdout).ok()?)
}

/// Decide whether an app counts as unused; returns the user-facing reason.
/// Pure so the time arithmetic is unit-testable.
fn unused_reason(
    last_used: Option<SystemTime>,
    created: Option<SystemTime>,
    now: SystemTime,
) -> Option<String> {
    if let Some(used) = last_used {
        let idle = now.duration_since(used).ok()?;
        (idle > UNUSED_AFTER)
            .then(|| format!("Last opened {} — over 180 days ago", format_date(used)))
    } else {
        // Never opened: only report once the bundle itself is old enough.
        // No creation date → no verdict (conservative).
        let age = now.duration_since(created?).ok()?;
        (age > UNUSED_AFTER).then(|| "Never opened since installation over 180 days ago".to_owned())
    }
}

fn is_tabibu(path: &Path, bundle_id: &str) -> bool {
    let by_name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("tabibu"));
    let by_id = bundle_id
        .split('.')
        .next_back()
        .is_some_and(|segment| segment.eq_ignore_ascii_case("tabibu"));
    by_name || by_id
}

/// Parse mdls raw output like `2024-05-01 12:00:00 +0000` (an optional
/// fractional-second part is ignored). Returns `None` for `(null)` or
/// anything that does not parse exactly.
fn parse_mdls_date(raw: &str) -> Option<SystemTime> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "(null)" {
        return None;
    }
    let mut fields = trimmed.split_whitespace();
    let (date, time, zone) = (fields.next()?, fields.next()?, fields.next()?);
    if fields.next().is_some() {
        return None;
    }
    let (year, month, day) = parse_date(date)?;
    let (hour, minute, second) = parse_time(time)?;
    let zone_offset = parse_zone(zone)?;
    let secs =
        days_from_civil(year, month, day) * SECS_PER_DAY + hour * 3600 + minute * 60 + second
            - zone_offset;
    let secs = u64::try_from(secs).ok()?; // pre-1970 → None
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

fn parse_date(text: &str) -> Option<(i64, i64, i64)> {
    let mut parts = text.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let valid = parts.next().is_none()
        && (1..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=31).contains(&day);
    valid.then_some((year, month, day))
}

fn parse_time(text: &str) -> Option<(i64, i64, i64)> {
    let whole = text.split('.').next()?; // drop fractional seconds
    let mut parts = whole.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;
    let valid = parts.next().is_none()
        && (0..=23).contains(&hour)
        && (0..=59).contains(&minute)
        && (0..=60).contains(&second); // leap second tolerated
    valid.then_some((hour, minute, second))
}

/// Zone like `+0000` / `-0700`, returned as an offset in seconds.
fn parse_zone(text: &str) -> Option<i64> {
    let (sign, digits) = if let Some(rest) = text.strip_prefix('+') {
        (1, rest)
    } else if let Some(rest) = text.strip_prefix('-') {
        (-1, rest)
    } else {
        return None;
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    if hours > 14 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let year_of_era = year - era * 400;
    let month_shifted = (month + 9) % 12;
    let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`]: `(year, month, day)`.
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_shifted + 2) / 5 + 1;
    let month = if month_shifted < 10 {
        month_shifted + 3
    } else {
        month_shifted - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn format_date(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::{format_date, parse_mdls_date, unused_reason, UNUSED_AFTER};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn at(epoch_secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(epoch_secs)
    }

    #[test]
    fn parses_canonical_mdls_output() {
        // 2024-05-01 00:00:00 UTC == 1_714_521_600; +12h.
        assert_eq!(
            parse_mdls_date("2024-05-01 12:00:00 +0000"),
            Some(at(1_714_564_800))
        );
        // Trailing newline as produced by mdls.
        assert_eq!(
            parse_mdls_date("2024-05-01 12:00:00 +0000\n"),
            Some(at(1_714_564_800))
        );
        // Fractional seconds tolerated.
        assert_eq!(
            parse_mdls_date("2024-05-01 12:00:00.500 +0000"),
            Some(at(1_714_564_800))
        );
    }

    #[test]
    fn applies_zone_offsets() {
        assert_eq!(
            parse_mdls_date("2024-05-01 12:00:00 +0200"),
            Some(at(1_714_557_600))
        );
        assert_eq!(
            parse_mdls_date("2024-05-01 12:00:00 -0500"),
            Some(at(1_714_582_800))
        );
    }

    #[test]
    fn rejects_null_and_garbage() {
        assert_eq!(parse_mdls_date("(null)"), None);
        assert_eq!(parse_mdls_date(""), None);
        assert_eq!(parse_mdls_date("yesterday"), None);
        assert_eq!(parse_mdls_date("2024-13-01 12:00:00 +0000"), None); // month
        assert_eq!(parse_mdls_date("2024-05-01 25:00:00 +0000"), None); // hour
        assert_eq!(parse_mdls_date("2024-05-01 12:00:00 0000"), None); // no sign
        assert_eq!(parse_mdls_date("2024-05-01 12:00:00 +0000 extra"), None);
        assert_eq!(parse_mdls_date("1960-05-01 12:00:00 +0000"), None); // pre-epoch
    }

    #[test]
    fn formats_dates() {
        assert_eq!(format_date(at(1_714_564_800)), "2024-05-01");
        assert_eq!(format_date(at(0)), "1970-01-01");
    }

    #[test]
    fn verdicts_with_injected_dates() {
        let now = at(2_000_000_000);
        let old = now - UNUSED_AFTER - Duration::from_secs(86_400);
        let recent = now - Duration::from_secs(86_400);

        let reason = unused_reason(Some(old), None, now).expect("old app flagged");
        assert!(reason.contains("over 180 days"));
        assert!(
            reason.contains(&format_date(old)),
            "reason includes the date"
        );

        assert_eq!(unused_reason(Some(recent), None, now), None);

        let never = unused_reason(None, Some(old), now).expect("never-opened old app flagged");
        assert!(never.contains("Never opened"));

        assert_eq!(unused_reason(None, Some(recent), now), None);
        assert_eq!(
            unused_reason(None, None, now),
            None,
            "no creation date → no verdict"
        );
        // Clock skew (used in the future) must not flag anything.
        assert_eq!(
            unused_reason(Some(now + Duration::from_secs(60)), None, now),
            None
        );
    }
}
