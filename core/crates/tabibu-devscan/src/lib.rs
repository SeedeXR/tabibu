//! Rebuildable dev-artifact scanner — READ-ONLY.
//!
//! Walks a project tree (or a whole home) and reports build/dependency
//! directories that can be **regenerated from source** across common stacks:
//! Rust `target/`, Node `node_modules/`, Python `__pycache__`/`.venv`,
//! Gradle/Java `build`/`.gradle`, Xcode `DerivedData`, CocoaPods `Pods/`,
//! Flutter `.dart_tool/`, Terraform `.terraform/`, and so on.
//!
//! Safety invariant: a directory is only flagged when it is unambiguously an
//! artifact (e.g. `node_modules`, `.venv`) OR its parent holds a matching
//! project manifest (e.g. `target/` next to `Cargo.toml`) — so we never flag a
//! hand-authored folder that merely happens to be named `build` or `dist`.
//! Recognized artifact dirs are NOT descended into (their contents are counted
//! once, as a unit). Symlinks are never followed.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tabibu_engine::CancelToken;

/// One rebuildable artifact directory.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DevArtifact {
    pub path: PathBuf,
    /// Stable kind id, e.g. `"rust-target"`, `"node-modules"`.
    pub kind: &'static str,
    pub size_bytes: u64,
    /// How to regenerate it, shown to the user.
    pub rebuild: &'static str,
}

struct Rule {
    /// Exact directory name to match.
    name: &'static str,
    kind: &'static str,
    rebuild: &'static str,
    /// If non-empty, the artifact's PARENT must contain at least one of these
    /// files (a project manifest) — the guard against false positives for
    /// generic names like `build`/`dist`/`target`.
    markers: &'static [&'static str],
}

const RULES: &[Rule] = &[
    Rule {
        name: "node_modules",
        kind: "node-modules",
        rebuild: "npm/yarn/pnpm install",
        markers: &[],
    },
    Rule {
        name: "target",
        kind: "rust-target",
        rebuild: "cargo build",
        markers: &["Cargo.toml"],
    },
    Rule {
        name: "target",
        kind: "maven-target",
        rebuild: "mvn package",
        markers: &["pom.xml"],
    },
    Rule {
        name: ".next",
        kind: "next-build",
        rebuild: "next build",
        markers: &[],
    },
    Rule {
        name: ".nuxt",
        kind: "nuxt-build",
        rebuild: "nuxt build",
        markers: &[],
    },
    Rule {
        name: ".svelte-kit",
        kind: "sveltekit-build",
        rebuild: "vite build",
        markers: &[],
    },
    Rule {
        name: ".turbo",
        kind: "turbo-cache",
        rebuild: "regenerated on build",
        markers: &[],
    },
    Rule {
        name: ".parcel-cache",
        kind: "parcel-cache",
        rebuild: "regenerated on build",
        markers: &[],
    },
    Rule {
        name: "dist",
        kind: "dist",
        rebuild: "your build step",
        markers: &["package.json", "pyproject.toml", "setup.py"],
    },
    Rule {
        name: "build",
        kind: "build-output",
        rebuild: "your build step",
        markers: &[
            "package.json",
            "build.gradle",
            "build.gradle.kts",
            "CMakeLists.txt",
            "pubspec.yaml",
            "pom.xml",
        ],
    },
    Rule {
        name: "__pycache__",
        kind: "python-cache",
        rebuild: "regenerated on run",
        markers: &[],
    },
    Rule {
        name: ".pytest_cache",
        kind: "pytest-cache",
        rebuild: "regenerated on test",
        markers: &[],
    },
    Rule {
        name: ".mypy_cache",
        kind: "mypy-cache",
        rebuild: "regenerated on check",
        markers: &[],
    },
    Rule {
        name: ".ruff_cache",
        kind: "ruff-cache",
        rebuild: "regenerated on lint",
        markers: &[],
    },
    Rule {
        name: ".gradle",
        kind: "gradle-cache",
        rebuild: "gradle build",
        markers: &[],
    },
    Rule {
        name: ".dart_tool",
        kind: "dart-tool",
        rebuild: "dart/flutter pub get",
        markers: &[],
    },
    Rule {
        name: "Pods",
        kind: "cocoapods",
        rebuild: "pod install",
        markers: &["Podfile"],
    },
    Rule {
        name: ".venv",
        kind: "python-venv",
        rebuild: "recreate the venv",
        markers: &[],
    },
    Rule {
        name: "venv",
        kind: "python-venv",
        rebuild: "recreate the venv",
        markers: &[],
    },
    Rule {
        name: ".terraform",
        kind: "terraform",
        rebuild: "terraform init",
        markers: &[],
    },
    Rule {
        name: "DerivedData",
        kind: "xcode-derived",
        rebuild: "Xcode rebuild",
        markers: &[],
    },
];

/// Scan each root for rebuildable artifact directories, largest first.
///
/// `home` scopes the exclusions (below); pass the user's home directory. Roots
/// are canonicalized so the denylist check sees absolute paths.
#[must_use]
pub fn scan(roots: &[PathBuf], home: &Path, cancel: &CancelToken) -> Vec<DevArtifact> {
    let mut out = Vec::new();
    for root in roots {
        // Make the root absolute (lexically — no symlink resolution, so returned
        // paths aren't rewritten) so `denylist::denied`, which rejects relative
        // paths, judges children correctly. Fall back to the root as given.
        let root = std::path::absolute(root).unwrap_or_else(|_| root.clone());
        walk(&root, home, cancel, &mut out);
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
    out
}

/// Directories the scan must never descend into or flag: the user's `~/Library`
/// (app/OS-managed data — its `node_modules`/caches belong to installed apps,
/// not to source projects, and `tabibu-junk` handles Library caches), and
/// anything the engine denylist protects (Documents, Desktop, Photos, …) — which
/// `reclaim` would refuse anyway, so surfacing them is misleading and unsafe.
fn excluded(path: &Path, home: &Path) -> bool {
    path == home.join("Library")
        || is_home_dotdir(path, home)
        || tabibu_engine::denylist::denied(path, home).is_some()
}

/// A hidden directory directly under home (`~/.vscode`, `~/.npm`, `~/.nvm`,
/// `~/.gradle`, `~/.cache`, …) — tool/app-managed state, not a source project,
/// and its `node_modules`/caches belong to those tools. A project's OWN deeper
/// dot-dirs (`~/code/app/.venv`, `.next`) are unaffected — only the top level
/// of home is pruned, and an explicit `scan <that dir>` still descends it.
fn is_home_dotdir(path: &Path, home: &Path) -> bool {
    path.parent() == Some(home)
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
}

/// Match a directory `name` against the ruleset, given its `parent` (for the
/// manifest-marker check). `None` if it isn't a recognized artifact.
fn rule_for(name: &str, parent: &Path) -> Option<&'static Rule> {
    RULES.iter().find(|r| {
        r.name == name
            && (r.markers.is_empty() || r.markers.iter().any(|m| parent.join(m).exists()))
    })
}

fn walk(dir: &Path, home: &Path, cancel: &CancelToken, out: &mut Vec<DevArtifact>) {
    if cancel.is_cancelled() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if cancel.is_cancelled() {
            return;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let path = entry.path();
        if excluded(&path, home) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(rule) = rule_for(&name, dir) {
            // A "venv"/".venv" is only a real virtualenv if it holds pyvenv.cfg
            // (PEP 405) — otherwise a hand-named folder called "venv" would be
            // flagged. If it isn't one, fall through and treat it as a normal dir.
            if rule.kind != "python-venv" || path.join("pyvenv.cfg").exists() {
                let size = tabibu_walk::dir_size(&path, cancel).unwrap_or(0);
                out.push(DevArtifact {
                    path,
                    kind: rule.kind,
                    size_bytes: size,
                    rebuild: rule.rebuild,
                });
                // Prune: an artifact dir is counted as a unit, not descended into.
                continue;
            }
        }
        walk(&path, home, cancel, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn finds_unambiguous_and_manifest_gated_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Rust project: target/ next to Cargo.toml → flagged.
        touch(&root.join("proj/Cargo.toml"));
        touch(&root.join("proj/target/debug/app"));
        // Node: node_modules is unambiguous → flagged.
        touch(&root.join("web/package.json"));
        touch(&root.join("web/node_modules/left-pad/index.js"));
        // A hand-made "target" with NO Cargo.toml sibling → NOT flagged.
        touch(&root.join("notes/target/keep.txt"));

        let arts = scan(&[root.to_path_buf()], root, &CancelToken::new());
        let kinds: Vec<_> = arts.iter().map(|a| a.kind).collect();
        assert!(
            kinds.contains(&"rust-target"),
            "target next to Cargo.toml is flagged"
        );
        assert!(
            kinds.contains(&"node-modules"),
            "node_modules is always flagged"
        );
        assert!(
            !arts.iter().any(|a| a.path.starts_with(root.join("notes"))),
            "a 'target' with no manifest sibling must NOT be flagged"
        );
    }

    #[test]
    fn prunes_into_artifact_dirs_and_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A node_modules that itself contains a nested node_modules — must be
        // reported ONCE (parent), not descended into.
        touch(&root.join("app/package.json"));
        touch(&root.join("app/node_modules/pkg/node_modules/dep/index.js"));

        let arts = scan(&[root.to_path_buf()], root, &CancelToken::new());
        let nm: Vec<_> = arts.iter().filter(|a| a.kind == "node-modules").collect();
        assert_eq!(
            nm.len(),
            1,
            "the outer node_modules is counted once, not descended"
        );
        assert_eq!(nm[0].path, root.join("app/node_modules"));
        // Read-only: everything still there.
        assert!(root
            .join("app/node_modules/pkg/node_modules/dep/index.js")
            .exists());
    }

    #[test]
    fn empty_tree_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(&[dir.path().to_path_buf()], dir.path(), &CancelToken::new()).is_empty());
    }

    #[test]
    fn excludes_library_and_denylisted_trees() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // A real project artifact in an ordinary place → flagged.
        touch(&home.join("code/app/package.json"));
        touch(&home.join("code/app/node_modules/x/i.js"));
        // App-managed node_modules under ~/Library → must be skipped.
        touch(&home.join("Library/Application Support/Code/ext/node_modules/y/i.js"));
        // Project under ~/Documents (engine denylist / reclaim refuses) → skipped.
        touch(&home.join("Documents/proj/node_modules/z/i.js"));
        // Tool-managed top-level dot-dir (~/.vscode/…/node_modules) → skipped.
        touch(&home.join(".vscode/extensions/e/node_modules/w/i.js"));
        // But a project's OWN deeper .venv is still found.
        touch(&home.join("code/api/.venv/pyvenv.cfg"));

        let arts = scan(&[home.to_path_buf()], home, &CancelToken::new());
        let paths: Vec<_> = arts.iter().map(|a| a.path.clone()).collect();
        assert!(paths.iter().any(|p| p.ends_with("code/app/node_modules")));
        assert!(
            paths.iter().any(|p| p.ends_with("code/api/.venv")),
            "a project's own deeper .venv is still found"
        );
        assert!(
            !paths
                .iter()
                .any(|p| p.to_string_lossy().contains("/.vscode/")),
            "top-level ~/.vscode (tool-managed) must be excluded"
        );
        assert!(
            !paths
                .iter()
                .any(|p| p.to_string_lossy().contains("/Library/")),
            "~/Library trees must be excluded"
        );
        assert!(
            !paths
                .iter()
                .any(|p| p.to_string_lossy().contains("/Documents/")),
            "denylisted (Documents) trees must be excluded"
        );
    }

    #[test]
    fn maven_target_is_labeled_maven_not_rust() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("svc/pom.xml"));
        touch(&root.join("svc/target/classes/App.class"));
        let arts = scan(&[root.to_path_buf()], root, &CancelToken::new());
        let t = arts
            .iter()
            .find(|a| a.path.ends_with("svc/target"))
            .unwrap();
        assert_eq!(t.kind, "maven-target");
        assert_eq!(t.rebuild, "mvn package");
    }

    #[test]
    fn venv_needs_pyvenv_cfg_to_be_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A hand-named "venv" folder (no pyvenv.cfg) → NOT a virtualenv.
        touch(&root.join("notes/venv/reading.md"));
        // A real virtualenv → flagged.
        touch(&root.join("proj/venv/pyvenv.cfg"));
        touch(&root.join("proj/venv/lib/python/site.py"));

        let arts = scan(&[root.to_path_buf()], root, &CancelToken::new());
        assert!(
            arts.iter()
                .any(|a| a.path.ends_with("proj/venv") && a.kind == "python-venv"),
            "a real venv (has pyvenv.cfg) is flagged"
        );
        assert!(
            !arts.iter().any(|a| a.path.ends_with("notes/venv")),
            "a folder named venv without pyvenv.cfg is NOT flagged"
        );
    }
}
