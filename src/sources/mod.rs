use crate::error::SpecDriftError;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct DiscoveredFiles {
    pub rust: Vec<PathBuf>,
    pub markdown: Vec<PathBuf>,
    pub yaml: Vec<PathBuf>,
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
            match path.extension().and_then(|e| e.to_str()) {
                Some("rs") => out.rust.push(path),
                Some("md") | Some("markdown") => out.markdown.push(path),
                Some("yaml") | Some("yml") => out.yaml.push(path),
                _ => {}
            }
        }

        out.rust.sort();
        out.markdown.sort();
        out.yaml.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn walks_tempdir_and_buckets_by_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# hi").unwrap();
        fs::write(root.join("ci.yml"), "").unwrap();
        fs::write(root.join("ignore.txt"), "").unwrap();

        let found = FsWalker::walk(root).unwrap();
        assert_eq!(found.rust.len(), 1);
        assert_eq!(found.markdown.len(), 1);
        assert_eq!(found.yaml.len(), 1);
    }
}
