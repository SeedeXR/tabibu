//! The C ABI surface of the Tabibu core — the ONLY crate that may contain
//! `unsafe`, and the only place Swift touches Rust. Design (ADR-0001):
//!
//! - **Hand-written, narrow surface**: ~10 functions, no codegen tool.
//! - **JSON payloads**: every composite type crosses as a UTF-8 JSON string.
//!   UI-rate data doesn't need a binary protocol; hot loops never cross the
//!   boundary (they run inside Rust).
//! - **Ownership rule**: Rust allocates, Rust frees. Every `*mut c_char`
//!   returned must go back through [`tabibu_string_free`]. Strings passed IN
//!   are borrowed for the call only.
//! - **Cancellation**: callers create an op handle, pass it to long calls,
//!   and may cancel from any thread.
//! - **Versioning**: Swift asserts [`tabibu_ffi_version`] at launch; bump on
//!   any breaking change. The C header (`include/tabibu_core.h`) is
//!   maintained by hand next to this file and kept in lockstep.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tabibu_engine::{CancelToken, CleanupItem, ScanCtx};

/// Bump on every breaking ABI change. Swift asserts this at launch.
pub const FFI_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Op registry: cancellation handles usable across the boundary.
// ---------------------------------------------------------------------------

static NEXT_OP: AtomicU64 = AtomicU64::new(1);
static OPS: Mutex<Option<HashMap<u64, CancelToken>>> = Mutex::new(None);

fn with_ops<R>(f: impl FnOnce(&mut HashMap<u64, CancelToken>) -> R) -> R {
    let mut guard = OPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(guard.get_or_insert_with(HashMap::new))
}

fn token_for(op: u64) -> CancelToken {
    with_ops(|m| m.get(&op).cloned()).unwrap_or_default()
}

/// Create a cancellable operation handle.
#[no_mangle]
pub extern "C" fn tabibu_op_new() -> u64 {
    let id = NEXT_OP.fetch_add(1, Ordering::Relaxed);
    with_ops(|m| m.insert(id, CancelToken::new()));
    id
}

/// Cancel the operation. Safe to call from any thread, any time.
#[no_mangle]
pub extern "C" fn tabibu_op_cancel(op: u64) {
    if let Some(t) = with_ops(|m| m.get(&op).cloned()) {
        t.cancel();
    }
}

/// Release the handle (does not cancel).
#[no_mangle]
pub extern "C" fn tabibu_op_free(op: u64) {
    with_ops(|m| {
        m.remove(&op);
    });
}

// ---------------------------------------------------------------------------
// String helpers (Rust allocates, Rust frees).
// ---------------------------------------------------------------------------

fn to_c(s: String) -> *mut c_char {
    // JSON never contains interior NULs; fall back to an error literal if so.
    CString::new(s)
        .unwrap_or_else(|_| CString::new("{\"error\":\"interior NUL\"}").expect("static"))
        .into_raw()
}

/// # Safety
/// `s` must be a pointer previously returned by this library and not yet
/// freed. Passing anything else is undefined behavior. NULL is a no-op.
#[no_mangle]
pub unsafe extern "C" fn tabibu_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: per contract, `s` came from CString::into_raw in this lib.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Borrow an incoming C string for the duration of a call.
///
/// # Safety
/// `p` must be NULL or a valid NUL-terminated UTF-8 string.
unsafe fn from_c<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    // SAFETY: per contract, `p` is valid and NUL-terminated.
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

fn err_json(msg: &str) -> *mut c_char {
    to_c(serde_json::json!({ "error": msg }).to_string())
}

#[no_mangle]
pub extern "C" fn tabibu_ffi_version() -> u32 {
    FFI_VERSION
}

// ---------------------------------------------------------------------------
// Scan context (JSON shape shared with Swift).
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct CtxJson {
    home: PathBuf,
    allowed_roots: Vec<PathBuf>,
    #[serde(default)]
    running_bundle_ids: Vec<String>,
    #[serde(default)]
    full_disk_access: bool,
}

impl From<CtxJson> for ScanCtx {
    fn from(c: CtxJson) -> Self {
        ScanCtx {
            home: c.home,
            allowed_roots: c.allowed_roots,
            running_bundle_ids: c.running_bundle_ids.into_iter().collect(),
            full_disk_access: c.full_disk_access,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ScanConfig {
    #[serde(flatten)]
    ctx: CtxJson,
    /// Scanner ids to run; empty = all junk scanners.
    #[serde(default)]
    scanners: Vec<String>,
}

type ItemCallback = Option<extern "C" fn(item_json: *const c_char, user_data: *mut c_void)>;
type DoneCallback = Option<extern "C" fn(report_json: *const c_char, user_data: *mut c_void)>;

/// Raw `user_data` made `Send` so the scan thread can carry it. The contract
/// that the pointer outlives the scan belongs to the caller (Swift retains
/// its context until the done callback fires).
struct SendPtr(*mut c_void);
// SAFETY: the pointer is never dereferenced by Rust, only passed back to the
// caller's callbacks, which the caller promises are thread-safe.
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

fn emit_str(cb: ItemCallback, s: &str, ud: &SendPtr) {
    if let Some(f) = cb {
        if let Ok(c) = CString::new(s) {
            f(c.as_ptr(), ud.0);
        }
    }
}

/// Start a streaming scan on a background thread. Items arrive on that
/// thread as JSON `CleanupItem`s; `on_done` receives a scan summary.
/// Returns 1 on success, 0 on bad input (no thread started).
///
/// # Safety
/// `config_json` must be a valid NUL-terminated UTF-8 string. `user_data`
/// must remain valid until `on_done` fires; callbacks must be thread-safe.
#[no_mangle]
pub unsafe extern "C" fn tabibu_scan_start(
    config_json: *const c_char,
    op: u64,
    on_item: ItemCallback,
    on_done: DoneCallback,
    user_data: *mut c_void,
) -> u32 {
    // SAFETY: caller contract.
    let Some(cfg) = (unsafe { from_c(config_json) }) else {
        return 0;
    };
    let Ok(config) = serde_json::from_str::<ScanConfig>(cfg) else {
        return 0;
    };
    let ud = SendPtr(user_data);
    let cancel = token_for(op);

    std::thread::spawn(move || {
        let ctx: ScanCtx = config.ctx.into();
        let wanted = config.scanners;
        // Full registry: junk + heuristic malware scanners. An empty
        // `scanners` filter means "all junk scanners" (malware stays opt-in
        // so Smart Scan and the Security view stay distinct in the UI).
        let scanners: Vec<Box<dyn tabibu_engine::Scanner>> = tabibu_junk::scanners()
            .into_iter()
            .chain(tabibu_malware::scanners())
            .filter(|s| {
                if wanted.is_empty() {
                    !matches!(s.id(), "adware_heuristics" | "rogue_profiles")
                } else {
                    wanted.iter().any(|w| w == s.id())
                }
            })
            .collect();

        let report = tabibu_engine::smart_scan(&scanners, &ctx, &cancel, &|item: CleanupItem| {
            if let Ok(json) = serde_json::to_string(&item) {
                emit_str(on_item, &json, &ud);
            }
        });

        let summary = serde_json::json!({
            "cancelled": report.cancelled,
            "scanners": report.outcomes.iter().map(|o| serde_json::json!({
                "id": o.scanner_id,
                "items": o.items_emitted,
                "guard_rejected": o.guard_rejected,
                "error": o.error,
            })).collect::<Vec<_>>(),
        });
        if let (Some(f), Ok(c)) = (on_done, CString::new(summary.to_string())) {
            f(c.as_ptr(), ud.0);
        }
    });
    1
}

/// Reclaim selected items (synchronous — call from a background queue).
/// Input: JSON array of `CleanupItem`. Returns a JSON report; free with
/// [`tabibu_string_free`].
///
/// # Safety
/// All pointers must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn tabibu_reclaim(
    items_json: *const c_char,
    ctx_json: *const c_char,
    undo_dir: *const c_char,
) -> *mut c_char {
    // SAFETY: caller contract.
    let (Some(items), Some(ctx), Some(undo)) = (
        unsafe { from_c(items_json) },
        unsafe { from_c(ctx_json) },
        unsafe { from_c(undo_dir) },
    ) else {
        return err_json("null or non-UTF-8 argument");
    };
    let Ok(items) = serde_json::from_str::<Vec<CleanupItem>>(items) else {
        return err_json("bad items JSON");
    };
    let Ok(ctx) = serde_json::from_str::<CtxJson>(ctx) else {
        return err_json("bad ctx JSON");
    };
    match tabibu_engine::reclaim(&ctx.into(), &items, std::path::Path::new(undo)) {
        Ok(r) => to_c(
            serde_json::json!({
                "reclaimed_bytes": r.reclaimed_bytes,
                "succeeded": r.succeeded,
                "failed": r.failed,
                "manifest_path": r.manifest_path,
                "outcomes": r.outcomes.iter().map(|o| serde_json::json!({
                    "path": o.path, "reclaimed_bytes": o.reclaimed_bytes, "error": o.error,
                })).collect::<Vec<_>>(),
            })
            .to_string(),
        ),
        Err(e) => err_json(&e.to_string()),
    }
}

/// Build a size tree for the space map. `max_depth < 0` means unlimited.
/// Returns JSON `DirNode`; free with [`tabibu_string_free`].
///
/// # Safety
/// `root` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn tabibu_size_tree(
    root: *const c_char,
    max_depth: i64,
    op: u64,
) -> *mut c_char {
    // SAFETY: caller contract.
    let Some(root) = (unsafe { from_c(root) }) else {
        return err_json("null root");
    };
    let depth = usize::try_from(max_depth).ok();
    match tabibu_walk::size_tree(std::path::Path::new(root), &token_for(op), depth) {
        Ok(tree) => match serde_json::to_string(&tree) {
            Ok(s) => to_c(s),
            Err(e) => err_json(&e.to_string()),
        },
        Err(e) => err_json(&e.to_string()),
    }
}

/// Find duplicate files under the given roots (JSON array of paths).
/// Streams each confirmed group via `on_group` and returns the full list.
///
/// # Safety
/// As [`tabibu_scan_start`]: `roots_json` valid UTF-8, callbacks thread-safe,
/// `user_data` valid for the duration of this (synchronous) call.
#[no_mangle]
pub unsafe extern "C" fn tabibu_dupes_find(
    roots_json: *const c_char,
    min_size: u64,
    op: u64,
    on_group: ItemCallback,
    user_data: *mut c_void,
) -> *mut c_char {
    // SAFETY: caller contract.
    let Some(roots) = (unsafe { from_c(roots_json) }) else {
        return err_json("null roots");
    };
    let Ok(roots) = serde_json::from_str::<Vec<PathBuf>>(roots) else {
        return err_json("bad roots JSON");
    };
    let cancel = token_for(op);
    let ud = SendPtr(user_data);

    let mut files = Vec::new();
    for r in &roots {
        match tabibu_dupes::collect_candidates(r, min_size, &cancel) {
            Ok(mut f) => files.append(&mut f),
            Err(e) => return err_json(&e.to_string()),
        }
    }
    let cfg = tabibu_dupes::DupeConfig { min_size };
    let stream = |g: &tabibu_dupes::DuplicateGroup| {
        if let Ok(json) = serde_json::to_string(g) {
            emit_str(on_group, &json, &ud);
        }
    };
    match tabibu_dupes::find_duplicates(&files, &cfg, &cancel, &stream) {
        Ok(groups) => match serde_json::to_string(&groups) {
            Ok(s) => to_c(s),
            Err(e) => err_json(&e.to_string()),
        },
        Err(e) => err_json(&e.to_string()),
    }
}

/// Remnant hunt for an app being uninstalled. Returns JSON `CleanupItem[]`.
///
/// # Safety
/// All pointers must be valid NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn tabibu_find_remnants(
    bundle_id: *const c_char,
    app_name: *const c_char,
    ctx_json: *const c_char,
) -> *mut c_char {
    // SAFETY: caller contract.
    let (Some(bid), Some(name), Some(ctx)) = (
        unsafe { from_c(bundle_id) },
        unsafe { from_c(app_name) },
        unsafe { from_c(ctx_json) },
    ) else {
        return err_json("null or non-UTF-8 argument");
    };
    let Ok(ctx) = serde_json::from_str::<CtxJson>(ctx) else {
        return err_json("bad ctx JSON");
    };
    let items = tabibu_uninstall::find_remnants(bid, name, &ctx.into());
    match serde_json::to_string(&items) {
        Ok(s) => to_c(s),
        Err(e) => err_json(&e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Monitor sampling (stateful — CPU deltas need a persistent System).
// ---------------------------------------------------------------------------

static SAMPLER: Mutex<Option<tabibu_monitor::Sampler>> = Mutex::new(None);

/// Sample system + top-N processes. `by_cpu` false = order by memory.
/// Returns JSON `SystemSample`; free with [`tabibu_string_free`].
#[no_mangle]
pub extern "C" fn tabibu_monitor_sample(top_n: u32, by_cpu: bool) -> *mut c_char {
    let mut guard = SAMPLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sampler = guard.get_or_insert_with(tabibu_monitor::Sampler::new);
    let by = if by_cpu {
        tabibu_monitor::TopBy::Cpu
    } else {
        tabibu_monitor::TopBy::Memory
    };
    let snap = sampler.sample(top_n as usize, by);
    match serde_json::to_string(&snap) {
        Ok(s) => to_c(s),
        Err(e) => err_json(&e.to_string()),
    }
}
