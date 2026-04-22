use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Rule identifiers map 1:1 with the rule catalog in the README.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleId {
    SymbolAbsence,
    ConstraintViolation,
    OutdatedLogic,
    CompileFailure,
    DeprecatedUsage,
    LogicGap,
    LyingTest,
    MissingCoverage,
    GhostCommand,
    EnvMismatch,
}

impl RuleId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SymbolAbsence => "symbol_absence",
            Self::ConstraintViolation => "constraint_violation",
            Self::OutdatedLogic => "outdated_logic",
            Self::CompileFailure => "compile_failure",
            Self::DeprecatedUsage => "deprecated_usage",
            Self::LogicGap => "logic_gap",
            Self::LyingTest => "lying_test",
            Self::MissingCoverage => "missing_coverage",
            Self::GhostCommand => "ghost_command",
            Self::EnvMismatch => "env_mismatch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Notice,
    Warning,
    Critical,
}

impl Severity {
    pub fn glyph(&self) -> &'static str {
        match self {
            Self::Notice => "🟡 NOTICE",
            Self::Warning => "⚠️  WARNING",
            Self::Critical => "❌ CRITICAL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Location {
    pub file: PathBuf,
    pub line: u32,
}

impl Location {
    pub fn new(file: impl Into<PathBuf>, line: u32) -> Self {
        Self {
            file: file.into(),
            line,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecClaim {
    pub location: Location,
    pub kind: ClaimKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Symbol,
    Constraint,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeFact {
    pub location: Location,
    pub kind: FactKind,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    Function,
    Struct,
    Enum,
    Trait,
    TypeAlias,
    Module,
    Constant,
    Macro,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divergence {
    pub rule: RuleId,
    pub severity: Severity,
    pub location: Location,
    pub stated: String,
    pub reality: String,
    pub risk: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_id_round_trips_through_json() {
        let r = RuleId::SymbolAbsence;
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "\"symbol_absence\"");
        let back: RuleId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn severity_orders_notice_lt_warning_lt_critical() {
        assert!(Severity::Notice < Severity::Warning);
        assert!(Severity::Warning < Severity::Critical);
    }

    #[test]
    fn location_sorts_by_file_then_line() {
        let a = Location::new("a.rs", 10);
        let b = Location::new("a.rs", 20);
        let c = Location::new("b.rs", 1);
        let mut v = vec![c.clone(), b.clone(), a.clone()];
        v.sort();
        assert_eq!(v, vec![a, b, c]);
    }
}
