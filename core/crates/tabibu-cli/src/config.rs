//! Optional per-user config at `~/.config/tabibu/config.toml` (same dir as
//! `protected.list`). It supplies defaults for a couple of flags so you don't
//! retype them. The file is entirely optional: absent → every field `None` →
//! the built-in defaults, i.e. identical behavior to having no config.
//!
//! Precedence, per key: **explicit flag > config file > built-in default**.
//!
//! Format is a tiny TOML subset — flat `key = value` lines; `#` comments and
//! `[section]` headers are ignored — so no TOML dependency is pulled in just to
//! read two integers. Unknown keys and unparseable values are ignored (a config
//! file must never break the CLI). Protected paths are NOT configured here;
//! manage them with `tabibu protect` (one source of truth in `protected.list`).

use std::path::{Path, PathBuf};

pub const DEFAULT_DEPTH: usize = 1;
pub const DEFAULT_MIN_SIZE: u64 = 4096;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub depth: Option<usize>,
    pub min_size: Option<u64>,
}

/// `~/.config/tabibu/config.toml`.
#[must_use]
pub fn config_file(home: &Path) -> PathBuf {
    home.join(".config").join("tabibu").join("config.toml")
}

/// Load the config, or an empty (all-defaults) config if the file is missing or
/// unreadable.
#[must_use]
pub fn load(home: &Path) -> Config {
    std::fs::read_to_string(config_file(home))
        .map(|t| parse(&t))
        .unwrap_or_default()
}

fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    for line in text.lines() {
        // Drop a trailing `# ...` comment, then trim.
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let val = val.trim().trim_matches('"').trim();
        match key.trim() {
            "depth" => cfg.depth = val.parse().ok(),
            "min_size" => cfg.min_size = val.parse().ok(),
            _ => {}
        }
    }
    cfg
}

impl Config {
    /// Effective depth: explicit flag, else config, else built-in default.
    #[must_use]
    pub fn depth(&self, flag: Option<usize>) -> usize {
        flag.or(self.depth).unwrap_or(DEFAULT_DEPTH)
    }
    /// Effective min-size: explicit flag, else config, else built-in default.
    #[must_use]
    pub fn min_size(&self, flag: Option<u64>) -> u64 {
        flag.or(self.min_size).unwrap_or(DEFAULT_MIN_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_all_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = load(tmp.path());
        assert_eq!(cfg, Config::default());
        // Built-in defaults apply when nothing is set.
        assert_eq!(cfg.depth(None), DEFAULT_DEPTH);
        assert_eq!(cfg.min_size(None), DEFAULT_MIN_SIZE);
    }

    #[test]
    fn parses_flat_toml_subset() {
        let text = "\
# defaults for tabibu
[defaults]
depth = 3
min_size = 1048576   # 1 MiB
junk = \"ignored key\"
";
        let cfg = parse(text);
        assert_eq!(cfg.depth, Some(3));
        assert_eq!(cfg.min_size, Some(1_048_576));
    }

    #[test]
    fn bad_values_are_ignored_not_fatal() {
        let cfg = parse("depth = not_a_number\nmin_size =\n");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn precedence_flag_beats_config_beats_default() {
        let cfg = Config {
            depth: Some(5),
            min_size: Some(200),
        };
        assert_eq!(cfg.depth(Some(9)), 9); // explicit flag wins
        assert_eq!(cfg.depth(None), 5); // else config
        assert_eq!(Config::default().depth(None), DEFAULT_DEPTH); // else built-in
        assert_eq!(cfg.min_size(Some(10)), 10);
        assert_eq!(cfg.min_size(None), 200);
    }
}
