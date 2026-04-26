use super::Reporter;
use crate::domain::{Divergence, Severity};
use serde::Serialize;

/// SARIF 2.1.0 reporter — emits the minimum viable schema that GitHub code
/// scanning and most IDE integrations accept.
///
/// Spec: <https://docs.oasis-open.org/sarif/sarif/v2.1.0/os/sarif-v2.1.0-os.html>
///
/// The mapping is conservative:
/// - Every `RuleId` becomes one entry in `tool.driver.rules`.
/// - Every `Divergence` becomes one `result` whose `ruleId` matches.
/// - Severity maps: critical→error, warning→warning, notice→note.
pub struct SarifReporter;

impl Reporter for SarifReporter {
    fn render(&self, divergences: &[Divergence]) -> String {
        let mut rules: Vec<SarifRule> = Vec::new();
        let mut seen: std::collections::BTreeSet<&'static str> = Default::default();
        for d in divergences {
            if seen.insert(d.rule.as_str()) {
                rules.push(SarifRule {
                    id: d.rule.as_str(),
                    short_description: SarifText {
                        text: d.rule.as_str().to_string(),
                    },
                });
            }
        }

        let results: Vec<SarifResult> = divergences
            .iter()
            .map(|d| SarifResult {
                rule_id: d.rule.as_str(),
                level: level_for(d.severity),
                message: SarifText {
                    text: format!("Stated: {} / Reality: {}", d.stated, d.reality),
                },
                locations: vec![SarifLocation {
                    physical_location: SarifPhysicalLocation {
                        artifact_location: SarifArtifactLocation {
                            uri: d.location.file.display().to_string(),
                        },
                        region: SarifRegion {
                            start_line: d.location.line.max(1),
                        },
                    },
                }],
                partial_fingerprints: d.attribution.as_ref().map(|a| SarifFingerprints {
                    commit_sha: a.commit.clone(),
                }),
            })
            .collect();

        let doc = SarifDoc {
            schema: "https://json.schemastore.org/sarif-2.1.0.json",
            version: "2.1.0",
            runs: vec![SarifRun {
                tool: SarifTool {
                    driver: SarifDriver {
                        name: "spec-drift",
                        information_uri: "https://github.com/asmuelle/spec-drift",
                        rules,
                    },
                },
                results,
            }],
        };

        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
    }
}

fn level_for(sev: Severity) -> &'static str {
    match sev {
        Severity::Critical => "error",
        Severity::Warning => "warning",
        Severity::Notice => "note",
    }
}

#[derive(Serialize)]
struct SarifDoc {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<SarifRun>,
}

#[derive(Serialize)]
struct SarifRun {
    tool: SarifTool,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(Serialize)]
struct SarifDriver {
    name: &'static str,
    #[serde(rename = "informationUri")]
    information_uri: &'static str,
    rules: Vec<SarifRule>,
}

#[derive(Serialize)]
struct SarifRule {
    id: &'static str,
    #[serde(rename = "shortDescription")]
    short_description: SarifText,
}

#[derive(Serialize)]
struct SarifResult {
    #[serde(rename = "ruleId")]
    rule_id: &'static str,
    level: &'static str,
    message: SarifText,
    locations: Vec<SarifLocation>,
    #[serde(
        rename = "partialFingerprints",
        skip_serializing_if = "Option::is_none"
    )]
    partial_fingerprints: Option<SarifFingerprints>,
}

#[derive(Serialize)]
struct SarifFingerprints {
    /// Matches GitHub's suggested `commitSha` fingerprint key for deduping
    /// results across runs once the offending commit is known.
    #[serde(rename = "commitSha")]
    commit_sha: String,
}

#[derive(Serialize)]
struct SarifLocation {
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysicalLocation,
}

#[derive(Serialize)]
struct SarifPhysicalLocation {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifactLocation,
    region: SarifRegion,
}

#[derive(Serialize)]
struct SarifArtifactLocation {
    uri: String,
}

#[derive(Serialize)]
struct SarifRegion {
    #[serde(rename = "startLine")]
    start_line: u32,
}

#[derive(Serialize)]
struct SarifText {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Location, RuleId};

    fn div(rule: RuleId, sev: Severity) -> Divergence {
        Divergence {
            rule,
            severity: sev,
            location: Location::new("README.md", 42),
            stated: "X exists".into(),
            reality: "X doesn't exist".into(),
            risk: "bad".into(),
            attribution: None,
        }
    }

    #[test]
    fn emits_version_and_single_run() {
        let out = SarifReporter.render(&[div(RuleId::SymbolAbsence, Severity::Critical)]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"].as_array().unwrap().len(), 1);
        assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "spec-drift");
    }

    #[test]
    fn maps_severity_to_sarif_level() {
        let divs = vec![
            div(RuleId::SymbolAbsence, Severity::Critical),
            div(RuleId::GhostCommand, Severity::Warning),
            div(RuleId::EnvMismatch, Severity::Notice),
        ];
        let out = SarifReporter.render(&divs);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let results = v["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results[0]["level"], "error");
        assert_eq!(results[1]["level"], "warning");
        assert_eq!(results[2]["level"], "note");
    }

    #[test]
    fn deduplicates_rule_entries() {
        let divs = vec![
            div(RuleId::SymbolAbsence, Severity::Critical),
            div(RuleId::SymbolAbsence, Severity::Critical),
        ];
        let out = SarifReporter.render(&divs);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let rules = v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["id"], "symbol_absence");
    }

    #[test]
    fn empty_input_still_valid_sarif() {
        let out = SarifReporter.render(&[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
    }
}
