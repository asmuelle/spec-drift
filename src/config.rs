//! `spec-drift.toml` configuration.
//!
//! The config is intentionally small. It supports the subset of the README-
//! documented config that materially changes analyzer output:
//!
//! - `[severity]` — override the severity of a rule by ID.
//! - `[ignore]` — silence rules, paths, or symbol patterns globally.
//!
//! Unknown keys are rejected so typos don't silently fail open.

use crate::domain::{Divergence, RuleId, Severity};
use crate::error::SpecDriftError;
use globset::{Glob, GlobMatcher, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A user-declared structural constraint applied by `ConstraintAnalyzer`.
#[derive(Debug, Clone)]
pub struct ConstraintRule {
    pub name: String,
    pub glob: GlobMatcher,
    pub return_type: Option<String>,
}

/// Parsed `spec-drift.toml`.
#[derive(Debug, Default, Clone)]
pub struct Config {
    pub severities: HashMap<RuleId, Severity>,
    pub ignored_rules: Vec<RuleId>,
    pub ignored_paths: GlobSet,
    pub ignored_symbols: GlobSet,
    pub constraint_rules: Vec<ConstraintRule>,
    pub llm: LlmConfig,
}

/// `[llm]` block — always off by default. `--no-llm` on the CLI wins unconditionally.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub enabled: bool,
    pub provider: LlmProvider,
    pub model: String,
    pub max_calls: u32,
    pub timeout_s: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: LlmProvider::Anthropic,
            model: "claude-sonnet-4-6".to_string(),
            max_calls: 50,
            timeout_s: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Anthropic,
    OpenAi,
    Local,
}

/// Whether a config path was user-specified or auto-discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Explicit,
    Discovered,
}

impl Config {
    /// Load a config from `path`. When `source` is [`ConfigSource::Discovered`],
    /// a missing file silently returns the default config. When `Explicit`, a
    /// missing file is an error — the user asked for a specific config that
    /// doesn't exist.
    pub fn load(path: &Path, source: ConfigSource) -> Result<Self, SpecDriftError> {
        if !path.exists() {
            return match source {
                ConfigSource::Discovered => Ok(Self::default()),
                ConfigSource::Explicit => Err(SpecDriftError::Config {
                    path: path.to_path_buf(),
                    message: "file not found".into(),
                }),
            };
        }
        let raw = std::fs::read_to_string(path).map_err(|e| SpecDriftError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let parsed: ConfigFile = toml::from_str(&raw).map_err(|e| SpecDriftError::Config {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        parsed.compile(path)
    }

    /// Walk upward from `start` looking for a `spec-drift.toml`. Returns
    /// `Ok(None)` if none is found.
    pub fn discover(start: &Path) -> Option<PathBuf> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            let candidate = d.join("spec-drift.toml");
            if candidate.exists() {
                return Some(candidate);
            }
            dir = d.parent();
        }
        None
    }

    /// True when this divergence should be dropped before reporting.
    pub fn is_suppressed(&self, d: &Divergence, root: &Path) -> bool {
        if self.ignored_rules.contains(&d.rule) {
            return true;
        }
        let rel = d
            .location
            .file
            .strip_prefix(root)
            .unwrap_or(&d.location.file);
        if self.ignored_paths.is_match(rel) {
            return true;
        }
        // Best-effort symbol matcher: pull the first backticked token out of
        // `stated`, strip any trailing `()`, and test against the symbols glob.
        if let Some(sym) = extract_backticked_symbol(&d.stated)
            && self.ignored_symbols.is_match(sym)
        {
            return true;
        }
        false
    }

    /// Apply `[severity]` overrides in place.
    pub fn apply_severity_overrides(&self, divs: &mut [Divergence]) {
        if self.severities.is_empty() {
            return;
        }
        for d in divs.iter_mut() {
            if let Some(s) = self.severities.get(&d.rule) {
                d.severity = *s;
            }
        }
    }
}

fn extract_backticked_symbol(text: &str) -> Option<&str> {
    let start = text.find('`')?;
    let rest = &text[start + 1..];
    let end = rest.find('`')?;
    let inner = &rest[..end];
    let inner = inner.strip_suffix("()").unwrap_or(inner);
    Some(inner)
}

// ---------------------------------------------------------------------------
// Raw on-disk representation
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    severity: HashMap<String, String>,
    #[serde(default)]
    ignore: IgnoreBlock,
    #[serde(default)]
    rules: RulesBlock,
    #[serde(default)]
    llm: Option<RawLlmConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLlmConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_calls: Option<u32>,
    #[serde(default)]
    timeout_s: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct IgnoreBlock {
    #[serde(default)]
    rules: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    symbols: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesBlock {
    #[serde(default)]
    constraint_violation: Vec<RawConstraintRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConstraintRule {
    name: String,
    glob: String,
    #[serde(default)]
    return_type: Option<String>,
}

impl ConfigFile {
    fn compile(self, path: &Path) -> Result<Config, SpecDriftError> {
        let mut severities = HashMap::new();
        for (rule_name, sev) in self.severity {
            let rule = parse_rule_id(&rule_name).ok_or_else(|| SpecDriftError::Config {
                path: path.to_path_buf(),
                message: format!("unknown rule in [severity]: `{rule_name}`"),
            })?;
            let sev = parse_severity(&sev).ok_or_else(|| SpecDriftError::Config {
                path: path.to_path_buf(),
                message: format!("unknown severity for `{rule_name}`: `{sev}`"),
            })?;
            severities.insert(rule, sev);
        }

        let mut ignored_rules = Vec::new();
        for rule_name in self.ignore.rules {
            let rule = parse_rule_id(&rule_name).ok_or_else(|| SpecDriftError::Config {
                path: path.to_path_buf(),
                message: format!("unknown rule in [ignore].rules: `{rule_name}`"),
            })?;
            ignored_rules.push(rule);
        }

        let mut constraint_rules = Vec::new();
        for raw in self.rules.constraint_violation {
            let glob = Glob::new(&raw.glob)
                .map_err(|e| SpecDriftError::Config {
                    path: path.to_path_buf(),
                    message: format!(
                        "invalid glob in [[rules.constraint_violation]] `{}`: {e}",
                        raw.name
                    ),
                })?
                .compile_matcher();
            constraint_rules.push(ConstraintRule {
                name: raw.name,
                glob,
                return_type: raw.return_type,
            });
        }

        let llm = match self.llm {
            Some(raw) => {
                let provider = match raw.provider.as_deref() {
                    None | Some("anthropic") => LlmProvider::Anthropic,
                    Some("openai") => LlmProvider::OpenAi,
                    Some("local") => LlmProvider::Local,
                    Some(other) => {
                        return Err(SpecDriftError::Config {
                            path: path.to_path_buf(),
                            message: format!("unknown [llm].provider: `{other}`"),
                        });
                    }
                };
                LlmConfig {
                    enabled: raw.enabled,
                    provider,
                    model: raw.model.unwrap_or_else(|| "claude-sonnet-4-6".to_string()),
                    max_calls: raw.max_calls.unwrap_or(50),
                    timeout_s: raw.timeout_s.unwrap_or(30),
                }
            }
            None => LlmConfig::default(),
        };

        Ok(Config {
            severities,
            ignored_rules,
            ignored_paths: build_globset(&self.ignore.paths, path, "paths")?,
            ignored_symbols: build_globset(&self.ignore.symbols, path, "symbols")?,
            constraint_rules,
            llm,
        })
    }
}

fn build_globset(patterns: &[String], path: &Path, field: &str) -> Result<GlobSet, SpecDriftError> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let glob = Glob::new(p).map_err(|e| SpecDriftError::Config {
            path: path.to_path_buf(),
            message: format!("invalid glob in [ignore].{field}: `{p}`: {e}"),
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|e| SpecDriftError::Config {
        path: path.to_path_buf(),
        message: format!("failed to compile [ignore].{field} glob set: {e}"),
    })
}

fn parse_rule_id(name: &str) -> Option<RuleId> {
    Some(match name {
        "symbol_absence" => RuleId::SymbolAbsence,
        "constraint_violation" => RuleId::ConstraintViolation,
        "outdated_logic" => RuleId::OutdatedLogic,
        "compile_failure" => RuleId::CompileFailure,
        "deprecated_usage" => RuleId::DeprecatedUsage,
        "logic_gap" => RuleId::LogicGap,
        "lying_test" => RuleId::LyingTest,
        "missing_coverage" => RuleId::MissingCoverage,
        "ghost_command" => RuleId::GhostCommand,
        "env_mismatch" => RuleId::EnvMismatch,
        _ => return None,
    })
}

fn parse_severity(name: &str) -> Option<Severity> {
    Some(match name {
        "notice" => Severity::Notice,
        "warning" => Severity::Warning,
        "critical" => Severity::Critical,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Location;

    fn make_div(rule: RuleId, file: &str) -> Divergence {
        Divergence {
            rule,
            severity: Severity::Critical,
            location: Location::new(file, 1),
            stated: "`legacy_shim` exists in the codebase".into(),
            reality: "not found".into(),
            risk: "x".into(),
            attribution: None,
        }
    }

    fn write_config(body: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("spec-drift.toml");
        std::fs::write(&path, body).unwrap();
        (tmp, path)
    }

    #[test]
    fn absent_config_loads_as_default_when_discovered() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config::load(&tmp.path().join("nope.toml"), ConfigSource::Discovered).unwrap();
        assert!(cfg.severities.is_empty());
        assert!(cfg.ignored_rules.is_empty());
    }

    #[test]
    fn absent_config_errors_when_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Config::load(&tmp.path().join("nope.toml"), ConfigSource::Explicit).is_err());
    }

    #[test]
    fn parses_severity_overrides() {
        let (_tmp, path) = write_config(
            r#"
            [severity]
            symbol_absence = "warning"
        "#,
        );
        let cfg = Config::load(&path, ConfigSource::Discovered).unwrap();
        assert_eq!(
            cfg.severities.get(&RuleId::SymbolAbsence),
            Some(&Severity::Warning)
        );
    }

    #[test]
    fn parses_documented_constraint_rule_shape() {
        let (_tmp, path) = write_config(
            r#"
            [[rules.constraint_violation]]
            name = "handlers_return_result"
            glob = "src/handlers/**"
            return_type = "Result<_, ApiError>"
        "#,
        );

        let cfg = Config::load(&path, ConfigSource::Discovered).unwrap();

        assert_eq!(cfg.constraint_rules.len(), 1);
        assert_eq!(cfg.constraint_rules[0].name, "handlers_return_result");
        assert_eq!(
            cfg.constraint_rules[0].return_type.as_deref(),
            Some("Result<_, ApiError>")
        );
    }

    #[test]
    fn rejects_unknown_severity_value() {
        let (_tmp, path) = write_config(
            r#"
            [severity]
            symbol_absence = "loud"
        "#,
        );
        assert!(Config::load(&path, ConfigSource::Discovered).is_err());
    }

    #[test]
    fn rejects_unknown_rule_name() {
        let (_tmp, path) = write_config(
            r#"
            [severity]
            not_a_rule = "warning"
        "#,
        );
        assert!(Config::load(&path, ConfigSource::Discovered).is_err());
    }

    #[test]
    fn suppresses_by_rule_and_by_path() {
        let (_tmp, path) = write_config(
            r#"
            [ignore]
            rules = ["outdated_logic"]
            paths = ["docs/legacy/**"]
            symbols = ["legacy_*"]
        "#,
        );
        let cfg = Config::load(&path, ConfigSource::Discovered).unwrap();
        let root = Path::new("/root");

        let by_rule = make_div(RuleId::OutdatedLogic, "/root/README.md");
        assert!(cfg.is_suppressed(&by_rule, root));

        let by_path = make_div(RuleId::SymbolAbsence, "/root/docs/legacy/api.md");
        assert!(cfg.is_suppressed(&by_path, root));

        let by_symbol = make_div(RuleId::SymbolAbsence, "/root/README.md");
        assert!(cfg.is_suppressed(&by_symbol, root));

        let kept = Divergence {
            location: Location::new("/root/src/lib.rs", 1),
            stated: "`keep_me` exists in the codebase".into(),
            ..make_div(RuleId::SymbolAbsence, "/root/src/lib.rs")
        };
        assert!(!cfg.is_suppressed(&kept, root));
    }

    #[test]
    fn applies_severity_overrides_in_place() {
        let (_tmp, path) = write_config(
            r#"
            [severity]
            symbol_absence = "notice"
        "#,
        );
        let cfg = Config::load(&path, ConfigSource::Discovered).unwrap();
        let mut divs = vec![make_div(RuleId::SymbolAbsence, "/root/a.md")];
        cfg.apply_severity_overrides(&mut divs);
        assert_eq!(divs[0].severity, Severity::Notice);
    }

    #[test]
    fn llm_defaults_when_block_absent() {
        let (_tmp, path) = write_config("");
        let cfg = Config::load(&path, ConfigSource::Discovered).unwrap();
        assert!(!cfg.llm.enabled);
        assert_eq!(cfg.llm.provider, LlmProvider::Anthropic);
        assert_eq!(cfg.llm.max_calls, 50);
    }

    #[test]
    fn llm_block_parses_all_fields() {
        let (_tmp, path) = write_config(
            r#"
            [llm]
            enabled = true
            provider = "openai"
            model = "gpt-5"
            max_calls = 10
            timeout_s = 15
        "#,
        );
        let cfg = Config::load(&path, ConfigSource::Discovered).unwrap();
        assert!(cfg.llm.enabled);
        assert_eq!(cfg.llm.provider, LlmProvider::OpenAi);
        assert_eq!(cfg.llm.model, "gpt-5");
        assert_eq!(cfg.llm.max_calls, 10);
        assert_eq!(cfg.llm.timeout_s, 15);
    }

    #[test]
    fn llm_rejects_unknown_provider() {
        let (_tmp, path) = write_config(
            r#"
            [llm]
            enabled = true
            provider = "invented-corp"
        "#,
        );
        assert!(Config::load(&path, ConfigSource::Discovered).is_err());
    }

    #[test]
    fn discover_walks_upward() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(tmp.path().join("spec-drift.toml"), "").unwrap();
        let found = Config::discover(&nested).unwrap();
        assert_eq!(found, tmp.path().join("spec-drift.toml"));
    }
}
