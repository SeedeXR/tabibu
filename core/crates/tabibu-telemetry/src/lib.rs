//! tabibu-telemetry — opt-in, privacy-respecting deselection telemetry.
//!
//! Implemented in v0.1.2 (see memory/todo.md). This crate records a single,
//! deliberately coarse "false-positive" signal: when a user **unchecks** a
//! `Safe`-tier cleanup item during review — i.e. Tabibu suggested removing it
//! and the user disagreed — we want to learn *which category of suggestion*
//! users distrust, so we can improve defaults. See memory/philosophy.md
//! (honesty) and the engineering guide §8.
//!
//! # Privacy contract (non-negotiable — this is the design)
//!
//! - **Default OFF.** Nothing is recorded unless the user has explicitly
//!   enabled telemetry via [`Telemetry::set_enabled`].
//! - **Never records a path, filename, file contents, bundle id, or any
//!   user-identifying data.** A [`DeselectionEvent`] structurally *cannot*
//!   hold such data: its only fields are the scanner/category id, the tier,
//!   a coarse [`SizeBucket`], and a caller-supplied unix timestamp.
//! - **No clock access.** This is a library; nondeterministic `now()` calls
//!   are forbidden. The caller passes `ts_unix`.
//! - **Local only, no network.** Events are appended to a JSONL file inside a
//!   caller-provided directory. This crate has no network dependency.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Errors surfaced by [`Telemetry`] operations.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// An underlying filesystem operation failed.
    #[error("telemetry io error: {0}")]
    Io(#[from] std::io::Error),
    /// (De)serialization of an event or the consent file failed.
    #[error("telemetry serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// A coarse size classification. We never record exact byte counts — only
/// which order-of-magnitude bucket an item fell into — so the signal can
/// never be used to fingerprint a specific file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeBucket {
    /// `< 10 MiB`.
    #[serde(rename = "lt_10mb")]
    Lt10Mb,
    /// `[10 MiB, 100 MiB)`.
    #[serde(rename = "10_100mb")]
    Mb10To100,
    /// `[100 MiB, 1 GiB)`.
    #[serde(rename = "100mb_1gb")]
    Mb100To1Gb,
    /// `>= 1 GiB`.
    #[serde(rename = "gt_1gb")]
    Gt1Gb,
}

impl SizeBucket {
    const MB_10: u64 = 10 * 1024 * 1024;
    const MB_100: u64 = 100 * 1024 * 1024;
    const GB_1: u64 = 1024 * 1024 * 1024;

    /// Classify a byte count into its coarse bucket.
    ///
    /// Boundaries are inclusive at the lower edge of the higher bucket:
    /// `< 10 MiB`, `[10 MiB, 100 MiB)`, `[100 MiB, 1 GiB)`, `>= 1 GiB`.
    pub fn from_bytes(bytes: u64) -> SizeBucket {
        if bytes < Self::MB_10 {
            SizeBucket::Lt10Mb
        } else if bytes < Self::MB_100 {
            SizeBucket::Mb10To100
        } else if bytes < Self::GB_1 {
            SizeBucket::Mb100To1Gb
        } else {
            SizeBucket::Gt1Gb
        }
    }
}

/// A single deselection signal. This struct intentionally has **no** path,
/// filename, or content field — there is nowhere for user-identifying data to
/// live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeselectionEvent {
    /// Scanner/category id, e.g. `"user_cache"`. Not a path.
    pub category: String,
    /// Cleanup tier, e.g. `"Safe"`.
    pub tier: String,
    /// Coarse size bucket of the deselected item.
    pub size_bucket: SizeBucket,
    /// Caller-supplied unix timestamp (seconds). The library never reads the
    /// clock itself.
    pub ts_unix: u64,
}

/// On-disk consent record.
#[derive(Debug, Serialize, Deserialize)]
struct Consent {
    enabled: bool,
}

const CONSENT_FILE: &str = "telemetry-consent.json";
const EVENTS_FILE: &str = "deselections.jsonl";

/// Privacy-respecting, opt-in telemetry sink rooted at a caller-provided
/// directory.
#[derive(Debug, Clone)]
pub struct Telemetry {
    dir: PathBuf,
    enabled: bool,
}

impl Telemetry {
    /// Load consent state from `<dir>/telemetry-consent.json`.
    ///
    /// **Fails safe:** if the file is missing, unreadable, or corrupt,
    /// telemetry is treated as **disabled**. No filesystem mutation occurs.
    pub fn load(dir: &Path) -> Telemetry {
        let enabled = fs::read_to_string(dir.join(CONSENT_FILE))
            .ok()
            .and_then(|s| serde_json::from_str::<Consent>(&s).ok())
            .map(|c| c.enabled)
            .unwrap_or(false);
        Telemetry {
            dir: dir.to_path_buf(),
            enabled,
        }
    }

    /// Whether recording is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Persist consent (creating the directory if needed, via atomic
    /// tmp+rename).
    ///
    /// **Withdrawal:** turning telemetry **off** also deletes any existing
    /// events file. Disabling is a withdrawal of consent, so the previously
    /// collected data is removed — we do not keep data the user asked us to
    /// stop collecting.
    pub fn set_enabled(&mut self, on: bool) -> Result<(), TelemetryError> {
        fs::create_dir_all(&self.dir)?;

        let consent = Consent { enabled: on };
        let json = serde_json::to_string(&consent)?;

        let tmp = self.dir.join(format!("{CONSENT_FILE}.tmp"));
        let final_path = self.dir.join(CONSENT_FILE);
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &final_path)?;

        if !on {
            // Honor withdrawal: drop everything we previously collected.
            self.clear()?;
        }

        self.enabled = on;
        Ok(())
    }

    /// Record one deselection event.
    ///
    /// If telemetry is **disabled**, this does nothing and returns
    /// `Ok(false)` (a no-op is not an error). If enabled, the event is
    /// appended as one JSON line to `<dir>/deselections.jsonl` and returns
    /// `Ok(true)`.
    pub fn record(&self, event: &DeselectionEvent) -> Result<bool, TelemetryError> {
        if !self.enabled {
            return Ok(false);
        }
        fs::create_dir_all(&self.dir)?;

        let mut line = serde_json::to_string(event)?;
        line.push('\n');

        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(EVENTS_FILE))?;
        f.write_all(line.as_bytes())?;
        Ok(true)
    }

    /// Read back all recorded events (for a future "see what's collected"
    /// transparency UI). Returns an empty vec if no events file exists.
    pub fn export(&self) -> Result<Vec<DeselectionEvent>, TelemetryError> {
        let path = self.dir.join(EVENTS_FILE);
        let contents = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let mut events = Vec::new();
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            events.push(serde_json::from_str(line)?);
        }
        Ok(events)
    }

    /// Delete the events file. A no-op if it does not exist.
    pub fn clear(&self) -> Result<(), TelemetryError> {
        match fs::remove_file(self.dir.join(EVENTS_FILE)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn sample_event() -> DeselectionEvent {
        DeselectionEvent {
            category: "user_cache".to_string(),
            tier: "Safe".to_string(),
            size_bucket: SizeBucket::Mb10To100,
            ts_unix: 1_700_000_000,
        }
    }

    #[test]
    fn default_is_disabled_and_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let t = Telemetry::load(dir.path());
        assert!(!t.is_enabled(), "telemetry must default to OFF");

        let wrote = t.record(&sample_event()).unwrap();
        assert!(!wrote, "disabled record() must return Ok(false)");

        // Nothing must have been written.
        assert!(
            !dir.path().join(EVENTS_FILE).exists(),
            "disabled telemetry must not create an events file"
        );
        assert!(t.export().unwrap().is_empty());
    }

    #[test]
    fn enable_then_record_and_export_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Telemetry::load(dir.path());
        t.set_enabled(true).unwrap();
        assert!(t.is_enabled());

        // Persisted consent survives a reload.
        let reloaded = Telemetry::load(dir.path());
        assert!(reloaded.is_enabled());

        let ev = sample_event();
        let wrote = t.record(&ev).unwrap();
        assert!(wrote, "enabled record() must return Ok(true)");

        let back = t.export().unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0], ev, "exported event must be identical");
    }

    #[test]
    fn privacy_event_has_only_safe_keys_and_no_path_data() {
        let ev = DeselectionEvent {
            category: "user_cache".to_string(),
            tier: "Safe".to_string(),
            size_bucket: SizeBucket::Gt1Gb,
            ts_unix: 1_700_000_000,
        };
        let json = serde_json::to_string(&ev).unwrap();

        // Exactly these top-level keys, nothing more.
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let keys: BTreeSet<String> = value.as_object().unwrap().keys().cloned().collect();
        let expected: BTreeSet<String> = ["category", "tier", "size_bucket", "ts_unix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            keys, expected,
            "serialized keys must be exactly the safe set"
        );

        // The signal we DO carry is present.
        assert!(json.contains("user_cache"));
        assert!(json.contains("Safe"));
        assert!(json.contains("gt_1gb"));
        assert!(json.contains("1700000000"));

        // There must be no path-like / filename-like data anywhere.
        assert!(!json.contains('/'), "no path separators allowed");
        assert!(!json.contains('\\'), "no path separators allowed");
        assert!(!json.contains(".app"), "no bundle ids / app paths allowed");
        for forbidden in ["path", "file", "name", "bundle"] {
            assert!(
                !json.contains(forbidden),
                "serialized event must not contain a `{forbidden}` field"
            );
        }
    }

    #[test]
    fn withdrawal_deletes_events_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Telemetry::load(dir.path());
        t.set_enabled(true).unwrap();
        t.record(&sample_event()).unwrap();
        assert!(dir.path().join(EVENTS_FILE).exists());

        t.set_enabled(false).unwrap();
        assert!(!t.is_enabled());
        assert!(
            !dir.path().join(EVENTS_FILE).exists(),
            "turning telemetry off must delete previously collected events"
        );
        // And it stays off across reloads.
        assert!(!Telemetry::load(dir.path()).is_enabled());
    }

    #[test]
    fn corrupt_consent_is_treated_as_disabled() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(CONSENT_FILE), "{not valid json").unwrap();
        let t = Telemetry::load(dir.path());
        assert!(!t.is_enabled(), "corrupt consent must fail safe to OFF");

        // Missing-but-different shape also fails safe.
        fs::write(dir.path().join(CONSENT_FILE), "{\"other\":1}").unwrap();
        assert!(!Telemetry::load(dir.path()).is_enabled());
    }

    #[test]
    fn multiple_records_append_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Telemetry::load(dir.path());
        t.set_enabled(true).unwrap();

        let first = DeselectionEvent {
            category: "browser_cache".to_string(),
            tier: "Safe".to_string(),
            size_bucket: SizeBucket::Lt10Mb,
            ts_unix: 1,
        };
        let second = DeselectionEvent {
            category: "logs".to_string(),
            tier: "Safe".to_string(),
            size_bucket: SizeBucket::Mb100To1Gb,
            ts_unix: 2,
        };

        assert!(t.record(&first).unwrap());
        assert!(t.record(&second).unwrap());

        let back = t.export().unwrap();
        assert_eq!(
            back,
            vec![first, second],
            "events must be appended in order"
        );
    }

    #[test]
    fn clear_removes_events_but_keeps_consent() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Telemetry::load(dir.path());
        t.set_enabled(true).unwrap();
        t.record(&sample_event()).unwrap();

        t.clear().unwrap();
        assert!(t.export().unwrap().is_empty());
        // clear() is not a withdrawal — consent stays enabled.
        assert!(Telemetry::load(dir.path()).is_enabled());
        // clear() on a missing file is a no-op.
        t.clear().unwrap();
    }

    #[test]
    fn size_bucket_boundaries() {
        let mb10 = 10 * 1024 * 1024;
        let mb100 = 100 * 1024 * 1024;
        let gb1 = 1024 * 1024 * 1024;

        // Below / at / above each boundary.
        assert_eq!(SizeBucket::from_bytes(0), SizeBucket::Lt10Mb);
        assert_eq!(SizeBucket::from_bytes(9 * 1024 * 1024), SizeBucket::Lt10Mb);
        assert_eq!(SizeBucket::from_bytes(mb10 - 1), SizeBucket::Lt10Mb);

        assert_eq!(SizeBucket::from_bytes(mb10), SizeBucket::Mb10To100);
        assert_eq!(SizeBucket::from_bytes(mb100 - 1), SizeBucket::Mb10To100);

        assert_eq!(SizeBucket::from_bytes(mb100), SizeBucket::Mb100To1Gb);
        assert_eq!(SizeBucket::from_bytes(gb1 - 1), SizeBucket::Mb100To1Gb);

        assert_eq!(SizeBucket::from_bytes(gb1), SizeBucket::Gt1Gb);
        assert_eq!(SizeBucket::from_bytes(u64::MAX), SizeBucket::Gt1Gb);
    }

    #[test]
    fn size_bucket_serializes_snake_case() {
        let cases = [
            (SizeBucket::Lt10Mb, "\"lt_10mb\""),
            (SizeBucket::Mb10To100, "\"10_100mb\""),
            (SizeBucket::Mb100To1Gb, "\"100mb_1gb\""),
            (SizeBucket::Gt1Gb, "\"gt_1gb\""),
        ];
        for (bucket, expected) in cases {
            assert_eq!(serde_json::to_string(&bucket).unwrap(), expected);
            let back: SizeBucket = serde_json::from_str(expected).unwrap();
            assert_eq!(back, bucket);
        }
    }
}
