//! Salama VPN — server profiles + connection state.
//!
//! A user can register MULTIPLE salama-web servers (their own, or someone
//! else's) by URL and switch between them; one is `active`. We persist only the
//! server list (id + label + URL) — never the admin password (used transiently
//! to provision) — plus the provisioned client config lives in a 0600 file per
//! server, keyed by id. The tunnel engine is the bundled `tabibu-wg`
//! (boringtun-cli); bring-up/teardown orchestration lives in `commands.rs`.
//!
//! The pure list operations (`add`/`remove`/`set_active`/`upsert`) are
//! unit-tested; persistence is a thin global + JSON, mirroring `alerts.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};

/// One registered salama-web server.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VpnServer {
    /// Stable id (also names the provisioned config file `<id>.conf`).
    pub id: String,
    /// Human label shown in the picker.
    pub name: String,
    /// Base URL of the salama-web admin (e.g. `https://vpn.example.com`).
    pub url: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct VpnConfig {
    #[serde(default)]
    pub servers: Vec<VpnServer>,
    /// Id of the active server, if any.
    #[serde(default)]
    pub active: Option<String>,
}

// ---- pure list operations (unit-tested; no I/O, no global) ----

/// Add or update a server by id (upsert), keeping the list unique by id. A new
/// server becomes active if none was set.
pub fn upsert(cfg: &mut VpnConfig, server: VpnServer) {
    if let Some(existing) = cfg.servers.iter_mut().find(|s| s.id == server.id) {
        existing.name = server.name;
        existing.url = server.url;
    } else {
        cfg.servers.push(server);
    }
    if cfg.active.is_none() {
        cfg.active = cfg.servers.first().map(|s| s.id.clone());
    }
}

/// Remove a server by id. If it was active, the active pointer falls back to the
/// first remaining server (or `None`).
pub fn remove(cfg: &mut VpnConfig, id: &str) {
    cfg.servers.retain(|s| s.id != id);
    if cfg.active.as_deref() == Some(id) {
        cfg.active = cfg.servers.first().map(|s| s.id.clone());
    }
}

/// Select the active server; ignored if the id isn't registered.
pub fn set_active(cfg: &mut VpnConfig, id: &str) {
    if cfg.servers.iter().any(|s| s.id == id) {
        cfg.active = Some(id.to_owned());
    }
}

// ---- persistence (thin global + JSON, mirrors alerts.rs) ----

fn cell() -> &'static Mutex<VpnConfig> {
    static C: OnceLock<Mutex<VpnConfig>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(VpnConfig::default()))
}

fn lock() -> std::sync::MutexGuard<'static, VpnConfig> {
    cell().lock().unwrap_or_else(PoisonError::into_inner)
}

fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("vpn.json")
}

fn read_config(config_dir: &Path) -> VpnConfig {
    std::fs::read_to_string(config_path(config_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(config_dir: &Path, cfg: &VpnConfig) {
    let _ = std::fs::create_dir_all(config_dir);
    if let Ok(text) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(config_path(config_dir), text);
    }
}

/// Load persisted servers into the global (call once at startup).
pub fn load(config_dir: &Path) {
    *lock() = read_config(config_dir);
}

/// A clone of the current config (for the UI).
#[must_use]
pub fn snapshot() -> VpnConfig {
    lock().clone()
}

/// Apply a mutation and persist (write under the lock, so concurrent writers
/// can't reorder the on-disk state — same rule as `alerts::update`).
pub fn update(config_dir: &Path, f: impl FnOnce(&mut VpnConfig)) -> VpnConfig {
    let mut g = lock();
    f(&mut g);
    write_config(config_dir, &g);
    g.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(id: &str) -> VpnServer {
        VpnServer {
            id: id.into(),
            name: id.into(),
            url: format!("https://{id}.example"),
        }
    }

    #[test]
    fn upsert_adds_updates_and_sets_first_active() {
        let mut c = VpnConfig::default();
        upsert(&mut c, srv("a"));
        assert_eq!(c.servers.len(), 1);
        assert_eq!(
            c.active.as_deref(),
            Some("a"),
            "first server becomes active"
        );
        // Update in place (no dup), active unchanged.
        upsert(
            &mut c,
            VpnServer {
                id: "a".into(),
                name: "renamed".into(),
                url: "https://x".into(),
            },
        );
        assert_eq!(c.servers.len(), 1);
        assert_eq!(c.servers[0].name, "renamed");
        assert_eq!(c.servers[0].url, "https://x");
        // Second server does NOT steal active.
        upsert(&mut c, srv("b"));
        assert_eq!(c.servers.len(), 2);
        assert_eq!(c.active.as_deref(), Some("a"));
    }

    #[test]
    fn set_active_only_for_known_ids() {
        let mut c = VpnConfig::default();
        upsert(&mut c, srv("a"));
        upsert(&mut c, srv("b"));
        set_active(&mut c, "b");
        assert_eq!(c.active.as_deref(), Some("b"));
        set_active(&mut c, "ghost"); // unknown → ignored
        assert_eq!(c.active.as_deref(), Some("b"));
    }

    #[test]
    fn remove_reassigns_active() {
        let mut c = VpnConfig::default();
        upsert(&mut c, srv("a"));
        upsert(&mut c, srv("b"));
        set_active(&mut c, "b");
        remove(&mut c, "b"); // removing the active one falls back
        assert_eq!(c.servers.len(), 1);
        assert_eq!(c.active.as_deref(), Some("a"));
        remove(&mut c, "a"); // last one gone → no active
        assert!(c.servers.is_empty());
        assert_eq!(c.active, None);
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = VpnConfig::default();
        upsert(&mut c, srv("home"));
        write_config(dir.path(), &c);
        let back = read_config(dir.path());
        assert_eq!(back.servers, c.servers);
        assert_eq!(back.active, c.active);
    }
}
