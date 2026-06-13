use crate::{denylist, CancelToken, CleanupItem};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("scan cancelled")]
    Cancelled,
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Other(String),
}

/// Context handed to every scanner. Built once per scan by the shell/FFI
/// layer; carries everything a scanner may consult so scanners stay pure.
#[derive(Debug, Clone)]
pub struct ScanCtx {
    /// User home directory (injected, never read from env inside scanners).
    pub home: PathBuf,
    /// Roots this scan is allowed to report from. The engine enforces that
    /// every emitted item lies inside one of these and outside the denylist.
    pub allowed_roots: Vec<PathBuf>,
    /// Bundle IDs of currently running applications (running-process guard).
    pub running_bundle_ids: HashSet<String>,
    /// Whether Full Disk Access is granted; scanners degrade scope when not.
    pub full_disk_access: bool,
}

/// A read-only detector of cleanup candidates. Implementations MUST NOT
/// mutate the filesystem; mutation lives exclusively in [`crate::reclaim`].
pub trait Scanner: Send + Sync {
    /// Stable identifier (used in config, telemetry, benches).
    fn id(&self) -> &'static str;

    /// Stream items to `sink` as they are found. Check `cancel` at every
    /// directory boundary. Returning `Err(ScanError::Cancelled)` on
    /// cancellation is expected, not exceptional.
    ///
    /// # Errors
    /// `ScanError::Cancelled` when the token fires; `ScanError::Io` only when
    /// the scan root itself is unusable (per-entry errors are skipped).
    fn scan(
        &self,
        ctx: &ScanCtx,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(CleanupItem),
    ) -> Result<(), ScanError>;
}

/// Wraps a raw sink with the engine's output invariant: items violating the
/// denylist or escaping the allowed roots are dropped and counted, never
/// emitted. Every scanner runs behind this guard (see `run_scanner`).
pub struct GuardedSink<'a> {
    ctx: &'a ScanCtx,
    inner: &'a mut dyn FnMut(CleanupItem),
    pub rejected: u64,
}

impl<'a> GuardedSink<'a> {
    pub fn new(ctx: &'a ScanCtx, inner: &'a mut dyn FnMut(CleanupItem)) -> Self {
        Self {
            ctx,
            inner,
            rejected: 0,
        }
    }

    pub fn emit(&mut self, item: CleanupItem) {
        if denylist::permitted(&item.path, &self.ctx.allowed_roots, &self.ctx.home) {
            (self.inner)(item);
        } else {
            self.rejected += 1;
        }
    }
}

/// Run one scanner with the guard applied. This is the only entry point the
/// FFI layer uses; calling `Scanner::scan` directly bypasses the invariant
/// and is forbidden outside tests.
///
/// # Errors
/// Propagates the scanner's own [`ScanError`] (cancellation or root I/O
/// failure); guard rejections are counted in the `Ok` value, not errors.
pub fn run_scanner(
    scanner: &dyn Scanner,
    ctx: &ScanCtx,
    cancel: &CancelToken,
    sink: &mut dyn FnMut(CleanupItem),
) -> Result<u64, ScanError> {
    let mut guarded = GuardedSink::new(ctx, sink);
    let mut emit = |item: CleanupItem| guarded.emit(item);
    scanner.scan(ctx, cancel, &mut emit)?;
    Ok(guarded.rejected)
}
