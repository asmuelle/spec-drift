//! `--baseline` support.
//!
//! A baseline is a JSON array of divergences (same schema the JSON reporter
//! emits). Loading a baseline filters the active run to retain only *new*
//! divergences, letting `spec-drift` be adopted on a legacy repo without a
//! flag-day cleanup.
//!
//! Identity is `(rule, file, line, stated)` — deliberately loose on the `risk`
//! and `reality` fields so small prose edits in the analyzer don't invalidate
//! the baseline.

use crate::domain::{Divergence, RuleId};
use crate::error::SpecDriftError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity {
    pub rule: RuleId,
    pub file: PathBuf,
    pub line: u32,
    pub stated: String,
}

impl From<&Divergence> for Identity {
    fn from(d: &Divergence) -> Self {
        Self {
            rule: d.rule,
            file: d.location.file.clone(),
            line: d.location.line,
            stated: d.stated.clone(),
        }
    }
}

/// Load a baseline JSON file into a set of identities. A missing file is an
/// error — the user asked us to enforce a baseline, so silently pretending it
/// existed would hide drift.
pub fn load(path: &Path) -> Result<HashSet<Identity>, SpecDriftError> {
    let raw = std::fs::read_to_string(path).map_err(|e| SpecDriftError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let divs: Vec<Divergence> =
        serde_json::from_str(&raw).map_err(|e| SpecDriftError::Baseline {
            path: path.to_path_buf(),
            message: format!("failed to parse baseline JSON: {e}"),
        })?;
    Ok(divs.iter().map(Identity::from).collect())
}

/// Drop divergences already present in the baseline. The returned Vec
/// preserves the input order of "new" divergences.
pub fn subtract(divs: Vec<Divergence>, baseline: &HashSet<Identity>) -> Vec<Divergence> {
    divs.into_iter()
        .filter(|d| !baseline.contains(&Identity::from(d)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Location, Severity};

    fn div(rule: RuleId, file: &str, line: u32, stated: &str) -> Divergence {
        Divergence {
            rule,
            severity: Severity::Critical,
            location: Location::new(file, line),
            stated: stated.into(),
            reality: "r".into(),
            risk: "k".into(),
            attribution: None,
        }
    }

    #[test]
    fn subtract_drops_known_divergences() {
        let existing = div(RuleId::SymbolAbsence, "README.md", 10, "`x` exists");
        let fresh = div(RuleId::SymbolAbsence, "README.md", 20, "`y` exists");

        let mut baseline: HashSet<Identity> = HashSet::new();
        baseline.insert(Identity::from(&existing));

        let remaining = subtract(vec![existing, fresh.clone()], &baseline);
        assert_eq!(remaining, vec![fresh]);
    }

    #[test]
    fn load_missing_file_errors() {
        let err = load(std::path::Path::new("/nope/baseline.json")).unwrap_err();
        // Either Io (the read failed) — but definitely not silently empty.
        assert!(matches!(err, SpecDriftError::Io { .. }));
    }

    #[test]
    fn load_and_subtract_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("baseline.json");
        let d = div(RuleId::LyingTest, "src/lib.rs", 42, "`t` is negative");
        std::fs::write(&path, serde_json::to_string(&vec![d.clone()]).unwrap()).unwrap();

        let baseline = load(&path).unwrap();
        let remaining = subtract(vec![d], &baseline);
        assert!(remaining.is_empty());
    }
}
