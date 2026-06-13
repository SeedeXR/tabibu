/* tabibu_core.h — hand-maintained C ABI of libtabibu_ffi (ADR-0001).
 * Keep in lockstep with crates/tabibu-ffi/src/lib.rs; bump TABIBU_FFI_VERSION
 * there on any breaking change and assert it from Swift at launch.
 *
 * Ownership: every char* RETURNED by this library must be released with
 * tabibu_string_free. Strings passed IN are borrowed for the call only.
 * All composite payloads are UTF-8 JSON.
 */
#ifndef TABIBU_CORE_H
#define TABIBU_CORE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ABI version this header describes. */
#define TABIBU_FFI_VERSION_EXPECTED 1u
uint32_t tabibu_ffi_version(void);

/* Cancellable operation handles. */
uint64_t tabibu_op_new(void);
void tabibu_op_cancel(uint64_t op);
void tabibu_op_free(uint64_t op);

/* Release a string returned by this library. NULL is a no-op. */
void tabibu_string_free(char *s);

/* Streaming callbacks: json is valid only for the duration of the call;
 * copy it. Called on a background thread. */
typedef void (*tabibu_json_cb)(const char *json, void *user_data);

/* Start a scan. config_json:
 *   { "home": "/Users/x", "allowed_roots": ["..."],
 *     "running_bundle_ids": ["..."], "full_disk_access": true,
 *     "scanners": ["trash","user_cache","dev_cache","temp","log"] }
 * Items stream to on_item as CleanupItem JSON; on_done gets a summary.
 * user_data must stay valid until on_done fires. Returns 1/0. */
uint32_t tabibu_scan_start(const char *config_json, uint64_t op,
                           tabibu_json_cb on_item, tabibu_json_cb on_done,
                           void *user_data);

/* Reclaim selected items (synchronous; call off the main thread).
 * items_json: CleanupItem[].  Returns a report JSON. */
char *tabibu_reclaim(const char *items_json, const char *ctx_json,
                     const char *undo_dir);

/* Size tree for the space map. max_depth < 0 = unlimited. Returns DirNode
 * JSON. */
char *tabibu_size_tree(const char *root, int64_t max_depth, uint64_t op);

/* Duplicate finder. roots_json: ["path", ...]. Streams DuplicateGroup JSON
 * per confirmed group (on_group may be NULL); returns the full group list. */
char *tabibu_dupes_find(const char *roots_json, uint64_t min_size,
                        uint64_t op, tabibu_json_cb on_group,
                        void *user_data);

/* Uninstall remnant hunt. Returns CleanupItem[] JSON. */
char *tabibu_find_remnants(const char *bundle_id, const char *app_name,
                           const char *ctx_json);

/* System + top-N process sample. Returns SystemSample JSON. */
char *tabibu_monitor_sample(uint32_t top_n, bool by_cpu);

#ifdef __cplusplus
}
#endif
#endif /* TABIBU_CORE_H */
