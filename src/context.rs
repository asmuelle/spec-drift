use crate::domain::CodeFact;
use std::path::{Path, PathBuf};

/// Everything an analyzer needs to answer questions about the project.
///
/// Owns parsed artifacts so each file is parsed at most once per run.
#[derive(Debug, Default)]
pub struct ProjectContext {
    /// Workspace/root directory used for config, suppression, blame, and
    /// workspace-relative reporting.
    pub root: PathBuf,
    /// Directory used for package-local cargo operations. Usually the same as
    /// `root`, but set to a workspace member root when `--package` is used.
    pub analysis_root: PathBuf,
    pub rust_files: Vec<PathBuf>,
    pub markdown_files: Vec<PathBuf>,
    pub yaml_files: Vec<PathBuf>,
    pub makefile_files: Vec<PathBuf>,
    pub code_facts: Vec<CodeFact>,
}

impl ProjectContext {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            analysis_root: root.clone(),
            root,
            ..Self::default()
        }
    }

    pub fn facts_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a CodeFact> + 'a {
        self.code_facts.iter().filter(move |f| f.name == name)
    }

    pub fn rel<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.analysis_root)
            .or_else(|_| path.strip_prefix(&self.root))
            .unwrap_or(path)
    }
}
