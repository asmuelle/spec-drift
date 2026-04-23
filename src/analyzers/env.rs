use super::DriftAnalyzer;
use crate::context::ProjectContext;
use crate::domain::{Divergence, Location, RuleId, Severity};
use crate::parsers::MarkdownParser;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

/// EnvMismatchAnalyzer — enforces `env_mismatch` (heuristic, Notice).
///
/// Strategy: extract system-package names mentioned in Markdown inline code
/// spans, extract packages installed in `.github/workflows/*.yml` via common
/// package managers (`apt-get install`, `apk add`, `yum install`, `dnf install`,
/// `brew install`), then ensure every README-mentioned package has an install
/// line that covers it — directly or via a known cross-distro equivalent like
/// `libssl-dev` ≡ `openssl-devel`.
///
/// Missing coverage surfaces as drift: the README promises a dependency the CI
/// never installs (or installs under a name the README doesn't acknowledge).
pub struct EnvMismatchAnalyzer;

impl Default for EnvMismatchAnalyzer {
    fn default() -> Self {
        Self
    }
}

impl DriftAnalyzer for EnvMismatchAnalyzer {
    fn id(&self) -> &'static str {
        "env_mismatch"
    }

    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let installed = collect_installed_packages(&ctx.yaml_files);
        if installed.is_empty() {
            // No CI install lines found — can't make a claim either way.
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut seen: HashSet<(std::path::PathBuf, u32, String)> = HashSet::new();

        for md in &ctx.markdown_files {
            let Ok(claims) = MarkdownParser::parse(md) else {
                continue;
            };
            for claim in claims {
                let token = claim.text.trim();
                if !looks_like_system_package(token) {
                    continue;
                }

                if coverage_present(token, &installed) {
                    continue;
                }

                let key = (
                    claim.location.file.clone(),
                    claim.location.line,
                    token.to_string(),
                );
                if !seen.insert(key) {
                    continue;
                }

                out.push(Divergence {
                    rule: RuleId::EnvMismatch,
                    severity: Severity::Notice,
                    location: Location::new(
                        claim.location.file.clone(),
                        claim.location.line,
                    ),
                    stated: format!("project requires `{token}`"),
                    reality: format!(
                        "no CI install line covers `{token}` \
                         (or a known cross-distro equivalent)"
                    ),
                    risk: "CI may pass on a box missing a dependency the docs promise."
                        .to_string(),
                });
            }
        }

        out
    }
}

/// A small, hand-curated set of cross-distro equivalence classes. Each inner
/// slice lists names that refer to the same underlying library.
const EQUIVALENCE_CLASSES: &[&[&str]] = &[
    &["libssl-dev", "openssl-dev", "openssl-devel"],
    &["libpq-dev", "postgresql-dev", "postgresql-devel"],
    &["zlib1g-dev", "zlib-dev", "zlib-devel"],
    &["libsqlite3-dev", "sqlite-dev", "sqlite-devel"],
    &["libcurl4-openssl-dev", "libcurl-dev", "curl-dev", "libcurl-devel"],
    &["pkg-config", "pkgconf", "pkgconfig"],
    &["build-essential", "base-devel", "Development Tools"],
    &["libxml2-dev", "libxml2-devel"],
    &["libffi-dev", "libffi-devel"],
];

fn canonical_forms(name: &str) -> Vec<&'static str> {
    let lower = name.to_ascii_lowercase();
    for class in EQUIVALENCE_CLASSES {
        if class.iter().any(|n| n.eq_ignore_ascii_case(&lower)) {
            return class.to_vec();
        }
    }
    Vec::new()
}

/// Heuristic package-ish filter. Captures common distro conventions:
/// - `libfoo-dev`, `libfoo` (Debian/Ubuntu style)
/// - `foo-devel` (Fedora/RHEL style)
/// - single tokens with hyphens and lowercase letters
fn looks_like_system_package(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"^(?:lib[a-z0-9][a-z0-9\-]*|[a-z][a-z0-9]*-(?:dev|devel|essential))$",
        )
        .unwrap()
    });
    re.is_match(s)
}

fn coverage_present(package: &str, installed: &HashSet<String>) -> bool {
    let lower = package.to_ascii_lowercase();
    if installed.contains(&lower) {
        return true;
    }
    for name in canonical_forms(package) {
        if installed.contains(&name.to_ascii_lowercase()) {
            return true;
        }
    }
    false
}

fn collect_installed_packages(yaml_files: &[std::path::PathBuf]) -> HashSet<String> {
    let mut out = HashSet::new();
    for path in yaml_files {
        if !is_workflow_path(path) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in src.lines() {
            extract_packages_from_line(line, &mut out);
        }
    }
    out
}

fn is_workflow_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".github")
        && path.components().any(|c| c.as_os_str() == "workflows")
}

fn install_command_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Captures the package list portion after the install subcommand.
        Regex::new(
            r"(?:apt(?:-get)?|apk|yum|dnf|brew)\s+(?:-[^\s]+\s+)*(?:install|add)\s+([^\n#]*)",
        )
        .unwrap()
    })
}

fn extract_packages_from_line(line: &str, out: &mut HashSet<String>) {
    let Some(caps) = install_command_re().captures(line) else {
        return;
    };
    let list = caps.get(1).unwrap().as_str();
    for token in list.split(|c: char| c.is_whitespace() || c == ',') {
        let token = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        if token.is_empty() {
            continue;
        }
        // Skip apt flags.
        if token.starts_with('-') || token.starts_with('=') {
            continue;
        }
        out.insert(token.to_ascii_lowercase());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(md: &str, yml: &str) -> (tempfile::TempDir, ProjectContext) {
        let tmp = tempfile::tempdir().unwrap();
        let md_path = tmp.path().join("README.md");
        std::fs::write(&md_path, md).unwrap();

        let workflows = tmp.path().join(".github").join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        let yml_path = workflows.join("ci.yml");
        std::fs::write(&yml_path, yml).unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md_path);
        ctx.yaml_files.push(yml_path);
        (tmp, ctx)
    }

    #[test]
    fn flags_readme_dep_not_in_ci_install_lines() {
        let (_tmp, ctx) = ctx_with(
            "Requires `libssl-dev` and `libpq-dev` to build.\n",
            "jobs:\n  test:\n    steps:\n      - run: apt-get install libssl-dev\n",
        );
        let divs = EnvMismatchAnalyzer.analyze(&ctx);
        assert_eq!(divs.len(), 1);
        assert!(divs[0].stated.contains("libpq-dev"));
        assert_eq!(divs[0].severity, Severity::Notice);
    }

    #[test]
    fn accepts_cross_distro_equivalent() {
        // README says libssl-dev, CI installs openssl-devel — these are the
        // same library, just named for different distros.
        let (_tmp, ctx) = ctx_with(
            "Requires `libssl-dev` to build.\n",
            "jobs:\n  test:\n    steps:\n      - run: dnf install openssl-devel\n",
        );
        assert!(EnvMismatchAnalyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn silent_when_ci_has_no_install_lines() {
        let (_tmp, ctx) = ctx_with(
            "Requires `libssl-dev`.\n",
            "jobs:\n  test:\n    steps:\n      - run: cargo test\n",
        );
        // No install command → can't make a claim → stay quiet.
        assert!(EnvMismatchAnalyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn ignores_prose_and_non_package_tokens() {
        let (_tmp, ctx) = ctx_with(
            "Uses `Client::new()` and `Option<String>` extensively.\n",
            "jobs:\n  test:\n    steps:\n      - run: apt-get install libssl-dev\n",
        );
        // `Client::new()` and `Option<String>` aren't package-shaped — must
        // not be mistaken for missing deps.
        assert!(EnvMismatchAnalyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn accepts_multi_package_install_line() {
        let (_tmp, ctx) = ctx_with(
            "Requires `libssl-dev`, `pkg-config`, and `build-essential`.\n",
            "jobs:\n  t:\n    steps:\n      - run: apt-get install -y libssl-dev pkg-config build-essential\n",
        );
        assert!(EnvMismatchAnalyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn looks_like_system_package_accepts_common_shapes() {
        assert!(looks_like_system_package("libssl-dev"));
        assert!(looks_like_system_package("openssl-devel"));
        assert!(looks_like_system_package("libpq-dev"));
        assert!(looks_like_system_package("build-essential"));
        assert!(!looks_like_system_package("Client"));
        assert!(!looks_like_system_package("Option<T>"));
        assert!(!looks_like_system_package("fn_name"));
    }
}
