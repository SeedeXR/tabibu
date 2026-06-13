//! FFI round-trip tests: drive the C ABI exactly the way Swift will —
//! C strings in, JSON out, callbacks with `user_data`, ownership rules obeyed.

use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::sync::mpsc::Sender;
use tabibu_ffi::{
    tabibu_dupes_find, tabibu_ffi_version, tabibu_op_cancel, tabibu_op_free, tabibu_op_new,
    tabibu_reclaim, tabibu_scan_start, tabibu_size_tree, tabibu_string_free, FFI_VERSION,
};

struct CallbackBox {
    items: Sender<String>,
    done: Sender<String>,
}

extern "C" fn on_item(json: *const c_char, ud: *mut c_void) {
    let b = unsafe { &*ud.cast::<CallbackBox>() };
    let s = unsafe { CStr::from_ptr(json) }
        .to_string_lossy()
        .into_owned();
    let _ = b.items.send(s);
}

extern "C" fn on_done(json: *const c_char, ud: *mut c_void) {
    let b = unsafe { &*ud.cast::<CallbackBox>() };
    let s = unsafe { CStr::from_ptr(json) }
        .to_string_lossy()
        .into_owned();
    let _ = b.done.send(s);
}

fn take_string(p: *mut c_char) -> String {
    assert!(!p.is_null());
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { tabibu_string_free(p) };
    s
}

#[test]
fn version_matches() {
    assert_eq!(tabibu_ffi_version(), FFI_VERSION);
}

#[test]
fn scan_streams_items_and_reports() {
    // Fixture home with junk for the trash scanner.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".Trash")).unwrap();
    fs::write(home.join(".Trash/old.zip"), vec![0u8; 2048]).unwrap();

    let config = serde_json::json!({
        "home": home,
        "allowed_roots": [home.join(".Trash")],
        "running_bundle_ids": [],
        "full_disk_access": true,
        "scanners": ["trash"],
    });
    let cfg = CString::new(config.to_string()).unwrap();

    let (item_tx, item_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let mut cbs = CallbackBox {
        items: item_tx,
        done: done_tx,
    };

    let op = tabibu_op_new();
    let started = unsafe {
        tabibu_scan_start(
            cfg.as_ptr(),
            op,
            Some(on_item),
            Some(on_done),
            std::ptr::from_mut(&mut cbs).cast(),
        )
    };
    assert_eq!(started, 1);

    let done = done_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let report: serde_json::Value = serde_json::from_str(&done).unwrap();
    assert_eq!(report["cancelled"], false);
    assert_eq!(report["scanners"][0]["id"], "trash");

    let items: Vec<String> = item_rx.try_iter().collect();
    assert_eq!(items.len(), 1, "one trash entry expected");
    let item: serde_json::Value = serde_json::from_str(&items[0]).unwrap();
    assert_eq!(item["size_bytes"], 2048);
    tabibu_op_free(op);
}

#[test]
fn reclaim_via_ffi_measures_and_undoes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let caches = home.join("Library/Caches");
    fs::create_dir_all(&caches).unwrap();
    fs::write(caches.join("junk.bin"), vec![0u8; 1024]).unwrap();

    let ctx = serde_json::json!({
        "home": home, "allowed_roots": [caches], "full_disk_access": true,
    });
    let items = serde_json::json!([{
        "path": caches.join("junk.bin"), "category": "Temp", "size_bytes": 1024,
        "tier": "Safe", "reason": "test", "selected": true, "action": "Delete",
    }]);
    let undo = tmp.path().join("undo");

    let ctx_c = CString::new(ctx.to_string()).unwrap();
    let items_c = CString::new(items.to_string()).unwrap();
    let undo_c = CString::new(undo.to_string_lossy().into_owned()).unwrap();
    let out = unsafe { tabibu_reclaim(items_c.as_ptr(), ctx_c.as_ptr(), undo_c.as_ptr()) };
    let report: serde_json::Value = serde_json::from_str(&take_string(out)).unwrap();

    assert_eq!(report["succeeded"], 1);
    assert_eq!(report["reclaimed_bytes"], 1024);
    assert!(!caches.join("junk.bin").exists());
    let manifest = report["manifest_path"].as_str().unwrap();
    assert!(std::path::Path::new(manifest).exists());
}

#[test]
fn size_tree_and_cancellation() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("a/b")).unwrap();
    fs::write(tmp.path().join("a/b/f.bin"), vec![0u8; 512]).unwrap();

    let root = CString::new(tmp.path().to_string_lossy().into_owned()).unwrap();
    let op = tabibu_op_new();
    let out = unsafe { tabibu_size_tree(root.as_ptr(), -1, op) };
    let tree: serde_json::Value = serde_json::from_str(&take_string(out)).unwrap();
    assert_eq!(tree["size_bytes"], 512);

    // Cancelled op fails honestly.
    tabibu_op_cancel(op);
    let out = unsafe { tabibu_size_tree(root.as_ptr(), -1, op) };
    let v: serde_json::Value = serde_json::from_str(&take_string(out)).unwrap();
    assert!(v["error"].is_string());
    tabibu_op_free(op);
}

#[test]
fn dupes_via_ffi() {
    let tmp = tempfile::tempdir().unwrap();
    let content = vec![7u8; 8192];
    fs::write(tmp.path().join("a.bin"), &content).unwrap();
    fs::write(tmp.path().join("b.bin"), &content).unwrap();
    fs::write(tmp.path().join("c.bin"), vec![9u8; 8192]).unwrap();

    let roots = CString::new(serde_json::json!([tmp.path()]).to_string()).unwrap();
    let op = tabibu_op_new();
    let out = unsafe { tabibu_dupes_find(roots.as_ptr(), 4096, op, None, std::ptr::null_mut()) };
    let groups: serde_json::Value = serde_json::from_str(&take_string(out)).unwrap();
    assert_eq!(groups.as_array().unwrap().len(), 1);
    assert_eq!(groups[0]["paths"].as_array().unwrap().len(), 2);
    tabibu_op_free(op);
}

#[test]
fn null_inputs_fail_gracefully() {
    let out = unsafe { tabibu_reclaim(std::ptr::null(), std::ptr::null(), std::ptr::null()) };
    let v: serde_json::Value = serde_json::from_str(&take_string(out)).unwrap();
    assert!(v["error"].is_string());
    let started =
        unsafe { tabibu_scan_start(std::ptr::null(), 0, None, None, std::ptr::null_mut()) };
    assert_eq!(started, 0);
    unsafe { tabibu_string_free(std::ptr::null_mut()) }; // no-op, no crash
}
