//! Workspace member discovery via `cargo metadata`.
//!
//! Single-crate projects just produce one entry with the crate root at the
//! project root. Virtual workspaces (`[workspace]` at the root, sources in
//! `crates/*`) produce one entry per member. Anything that `cargo metadata`
//! can't answer (no Cargo.toml, cargo missing) yields an empty list and the
//! caller falls back to "treat the whole tree as one unit".

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One member of a cargo workspace (or the single package in a non-workspace
/// project).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    /// Directory containing the package's `Cargo.toml`. Every file under this
    /// path belongs to this package.
    pub root: PathBuf,
}

#[derive(Deserialize)]
struct CargoMetadata {
    #[serde(default)]
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    manifest_path: String,
}

/// Load workspace members from `cargo metadata`. Returns an empty vec when
/// cargo is unavailable, not a Rust project, or the metadata JSON is malformed
/// — callers treat that as "unknown, don't filter."
pub fn load(manifest_dir: &Path) -> Vec<Package> {
    let out = Command::new("cargo")
        .current_dir(manifest_dir)
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(md) = serde_json::from_slice::<CargoMetadata>(&out.stdout) else {
        return Vec::new();
    };
    md.packages
        .into_iter()
        .filter_map(|p| {
            let root = Path::new(&p.manifest_path).parent()?.to_path_buf();
            Some(Package { name: p.name, root })
        })
        .collect()
}

/// Find a package by name. Returns `Err` with a helpful message listing the
/// known members when the name doesn't match.
pub fn find<'a>(packages: &'a [Package], name: &str) -> Result<&'a Package, String> {
    packages.iter().find(|p| p.name == name).ok_or_else(|| {
        let known: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!(
            "--package `{name}`: not a workspace member. Known: {}",
            if known.is_empty() {
                "(none — not a cargo project?)".to_string()
            } else {
                known.join(", ")
            }
        )
    })
}

/// Retain only the files under `pkg.root`.
pub fn narrow_paths(paths: Vec<PathBuf>, pkg: &Package) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|p| p.starts_with(&pkg.root))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_metadata_into_packages() {
        let json = r#"{"packages": [
            {"name": "alpha", "manifest_path": "/repo/crates/alpha/Cargo.toml"},
            {"name": "beta",  "manifest_path": "/repo/crates/beta/Cargo.toml"}
        ]}"#;
        let md: CargoMetadata = serde_json::from_str(json).unwrap();
        let packages: Vec<Package> = md
            .packages
            .into_iter()
            .filter_map(|p| {
                let root = std::path::Path::new(&p.manifest_path)
                    .parent()?
                    .to_path_buf();
                Some(Package { name: p.name, root })
            })
            .collect();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "alpha");
        assert_eq!(packages[0].root, PathBuf::from("/repo/crates/alpha"));
        assert_eq!(packages[1].root, PathBuf::from("/repo/crates/beta"));
    }

    #[test]
    fn malformed_metadata_returns_empty() {
        let json = r#"{"packages": "not-an-array"}"#;
        assert!(serde_json::from_str::<CargoMetadata>(json).is_err());
        let json = r#"{}"#;
        let md: CargoMetadata = serde_json::from_str(json).unwrap();
        assert!(md.packages.is_empty());
    }

    #[test]
    fn find_returns_helpful_error_on_unknown_name() {
        let packages = vec![Package {
            name: "alpha".into(),
            root: PathBuf::from("/r/alpha"),
        }];
        let err = find(&packages, "beta").unwrap_err();
        assert!(err.contains("beta"));
        assert!(err.contains("alpha"));
    }

    #[test]
    fn find_returns_no_cargo_hint_when_empty() {
        let err = find(&[], "anything").unwrap_err();
        assert!(err.contains("not a cargo project"));
    }

    #[test]
    fn narrow_paths_drops_non_members() {
        let pkg = Package {
            name: "alpha".into(),
            root: PathBuf::from("/r/alpha"),
        };
        let paths = vec![
            PathBuf::from("/r/alpha/src/lib.rs"),
            PathBuf::from("/r/beta/src/lib.rs"),
            PathBuf::from("/r/alpha/README.md"),
        ];
        let out = narrow_paths(paths, &pkg);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|p| p.starts_with("/r/alpha")));
    }
}
