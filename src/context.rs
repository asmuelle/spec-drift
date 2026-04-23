use crate::domain::CodeFact;
use std::path::{Path, PathBuf};

/// Everything an analyzer needs to answer questions about the project.
///
/// Owns parsed artifacts so each file is parsed at most once per run.
#[derive(Debug, Default)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub rust_files: Vec<PathBuf>,
    pub markdown_files: Vec<PathBuf>,
    pub yaml_files: Vec<PathBuf>,
    pub makefile_files: Vec<PathBuf>,
    pub code_facts: Vec<CodeFact>,
}

impl ProjectContext {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ..Self::default()
        }
    }

    pub fn facts_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a CodeFact> + 'a {
        self.code_facts.iter().filter(move |f| f.name == name)
    }

    pub fn rel<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.root).unwrap_or(path)
    }
}
