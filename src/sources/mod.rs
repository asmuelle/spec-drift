use crate::error::SpecDriftError;
use ignore::WalkBuilder;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default, Clone)]
pub struct DiscoveredFiles {
    pub rust: Vec<PathBuf>,
    pub markdown: Vec<PathBuf>,
    pub yaml: Vec<PathBuf>,
    pub makefiles: Vec<PathBuf>,
}

pub struct FsWalker;

impl FsWalker {
    /// Walk `root`, respect `.gitignore`, and bucket files by extension.
    pub fn walk(root: &Path) -> Result<DiscoveredFiles, SpecDriftError> {
        let mut out = DiscoveredFiles::default();

        for entry in WalkBuilder::new(root).hidden(false).build() {
            let entry = entry?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.into_path();

            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();

            // Makefile / justfile have no extension; match by filename.
            if matches!(
                file_name,
                "Makefile" | "makefile" | "GNUmakefile" | "justfile"
            ) {
                out.makefiles.push(path);
                continue;
            }

            match path.extension().and_then(|e| e.to_str()) {
                Some("rs") => out.rust.push(path),
                Some("md") | Some("markdown") => out.markdown.push(path),
                Some("yaml") | Some("yml") => out.yaml.push(path),
                Some("mk") => out.makefiles.push(path),
                _ => {}
            }
        }

        out.rust.sort();
        out.markdown.sort();
        out.yaml.sort();
        out.makefiles.sort();
        Ok(out)
    }
}

/// `GitHistory` narrows file discovery to only files that have changed relative
/// to a git ref (typically `HEAD`). Used by `--diff HEAD` so spec-drift can run
/// as a fast pre-commit check instead of scanning the whole tree.
///
/// Shelling out to `git` keeps us free of a linked libgit dependency. When git
/// is unavailable or the repo has no history, every method returns `None` and
/// the caller falls back to a full-tree walk.
pub struct GitHistory;

impl GitHistory {
    /// Return the set of files changed between `reference` and the working
    /// tree, relative to `root`. `None` means "can't answer — fall back to a
    /// full walk" (git missing, not a repo, unknown ref, etc.).
    pub fn changed_files(root: &Path, reference: &str) -> Option<HashSet<PathBuf>> {
        let out = Command::new("git")
            .current_dir(root)
            .args(["diff", "--name-only", reference])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        let set: HashSet<PathBuf> = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(|l| root.join(l))
            .collect();
        Some(set)
    }

    /// Filter every bucket in `files` to only paths present in `changed`.
    /// Invariant: buckets stay sorted.
    pub fn narrow(files: DiscoveredFiles, changed: &HashSet<PathBuf>) -> DiscoveredFiles {
        fn keep(v: Vec<PathBuf>, changed: &HashSet<PathBuf>) -> Vec<PathBuf> {
            let mut v: Vec<PathBuf> = v.into_iter().filter(|p| changed.contains(p)).collect();
            v.sort();
            v
        }
        DiscoveredFiles {
            rust: keep(files.rust, changed),
            markdown: keep(files.markdown, changed),
            yaml: keep(files.yaml, changed),
            makefiles: keep(files.makefiles, changed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn git_history_narrow_keeps_only_changed_paths() {
        let mut files = DiscoveredFiles::default();
        files.rust.push(PathBuf::from("/root/a.rs"));
        files.rust.push(PathBuf::from("/root/b.rs"));
        files.markdown.push(PathBuf::from("/root/README.md"));

        let mut changed = HashSet::new();
        changed.insert(PathBuf::from("/root/b.rs"));

        let narrowed = GitHistory::narrow(files, &changed);
        assert_eq!(narrowed.rust, vec![PathBuf::from("/root/b.rs")]);
        assert!(narrowed.markdown.is_empty());
    }

    #[test]
    fn walks_tempdir_and_buckets_by_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# hi").unwrap();
        fs::write(root.join("ci.yml"), "").unwrap();
        fs::write(root.join("Makefile"), "").unwrap();
        fs::write(root.join("justfile"), "").unwrap();
        fs::write(root.join("ignore.txt"), "").unwrap();

        let found = FsWalker::walk(root).unwrap();
        assert_eq!(found.rust.len(), 1);
        assert_eq!(found.markdown.len(), 1);
        assert_eq!(found.yaml.len(), 1);
        assert_eq!(found.makefiles.len(), 2);
    }
}
