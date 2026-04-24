//! `--blame` divergence attribution via `git blame --porcelain`.
//!
//! For each divergence, run `git blame -L <line>,<line> -- <file>` in the
//! project root and attach the resulting commit / author / date / subject.
//! Blame is opt-in because it spawns one git process per divergence — cheap
//! in absolute terms, noticeable if the run has hundreds of divergences.
//!
//! All failures (non-git directory, uncommitted file, blame parse error) map
//! to "no attribution" rather than surfacing as errors; attribution is
//! enrichment, not load-bearing.

use crate::domain::{Attribution, Divergence};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Enrich each divergence with blame attribution, if the engine can resolve
/// one. Divergences for which blame fails keep `attribution: None`.
pub fn apply(mut divs: Vec<Divergence>, root: &Path, engine: &dyn BlameEngine) -> Vec<Divergence> {
    for d in &mut divs {
        d.attribution = engine.blame(root, &d.location.file, d.location.line);
    }
    divs
}

/// Indirection so tests don't need a real git repo.
pub trait BlameEngine {
    fn blame(&self, root: &Path, file: &Path, line: u32) -> Option<Attribution>;
}

/// Real implementation that shells out to `git blame`.
pub struct GitBlameEngine;

impl BlameEngine for GitBlameEngine {
    fn blame(&self, root: &Path, file: &Path, line: u32) -> Option<Attribution> {
        // Git blame needs a repo-relative path. If `file` is already under
        // `root` we strip it; otherwise pass the absolute path and let git
        // resolve it itself.
        let arg: PathBuf = file
            .strip_prefix(root)
            .map(PathBuf::from)
            .unwrap_or_else(|_| file.to_path_buf());
        let spec = format!("{line},{line}");

        let out = Command::new("git")
            .current_dir(root)
            .args(["blame", "--porcelain", "-L", &spec])
            .arg(&arg)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        parse_porcelain(&text)
    }
}

/// Parse one line's worth of `git blame --porcelain` output into an
/// [`Attribution`]. Returns `None` if any required field is missing.
///
/// Porcelain format shape (per line range requested):
///
/// ```text
/// <full-sha> <orig-line> <final-line> [<num-lines>]
/// author <name>
/// author-mail <<mail>>
/// author-time <unix-ts>
/// author-tz <tz>
/// committer <name>
/// ...
/// summary <subject>
/// previous <sha> <filename>
/// filename <filename>
/// \t<content>
/// ```
pub fn parse_porcelain(raw: &str) -> Option<Attribution> {
    let mut lines = raw.lines();
    let header = lines.next()?;
    let sha = header.split_whitespace().next()?;
    if sha.len() < 7 || sha.chars().any(|c| !c.is_ascii_hexdigit()) {
        return None;
    }

    let mut author: Option<String> = None;
    let mut author_time: Option<i64> = None;
    let mut summary: Option<String> = None;

    for line in lines {
        if let Some(rest) = line.strip_prefix("author ") {
            author = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("author-time ") {
            author_time = rest.parse::<i64>().ok();
        } else if let Some(rest) = line.strip_prefix("summary ") {
            summary = Some(rest.to_string());
        } else if line.starts_with('\t') {
            // The content line signals end-of-header. Stop to avoid consuming
            // subsequent blame blocks for multi-line ranges.
            break;
        }
    }

    Some(Attribution {
        commit: sha.chars().take(7).collect(),
        author: author?,
        date: format_unix_date(author_time?),
        summary: summary?,
    })
}

/// Convert a unix timestamp to `YYYY-MM-DD` in UTC without pulling in a date
/// crate. Uses Howard Hinnant's `civil_from_days` algorithm — proven correct
/// for every civil date in the proleptic Gregorian calendar.
fn format_unix_date(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(mut z: i64) -> (i32, u32, u32) {
    z += 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_porcelain_block() {
        let raw = "abc1234def5678 10 10 1\n\
                   author Ada Lovelace\n\
                   author-mail <ada@example.com>\n\
                   author-time 1700000000\n\
                   author-tz +0000\n\
                   committer Ada Lovelace\n\
                   committer-mail <ada@example.com>\n\
                   committer-time 1700000000\n\
                   committer-tz +0000\n\
                   summary Fix the foo handler\n\
                   filename README.md\n\
                   \tUse `foo()` to start.\n";

        let a = parse_porcelain(raw).unwrap();
        assert_eq!(a.commit, "abc1234");
        assert_eq!(a.author, "Ada Lovelace");
        assert_eq!(a.summary, "Fix the foo handler");
        assert_eq!(a.date, "2023-11-14");
    }

    #[test]
    fn rejects_non_hex_sha() {
        let raw = "not-a-sha 1 1 1\nauthor X\nauthor-time 0\nsummary y\n\tz\n";
        assert!(parse_porcelain(raw).is_none());
    }

    #[test]
    fn requires_author_time_and_summary() {
        let raw = "abc1234def5678 1 1 1\nauthor X\n\tcontent\n";
        assert!(parse_porcelain(raw).is_none());
    }

    #[test]
    fn stops_at_first_content_line() {
        // A second blame block must not overwrite the first.
        let raw = "aaaaaaa 1 1 1\n\
                   author First Author\n\
                   author-time 1700000000\n\
                   summary First\n\
                   \tcontent\n\
                   bbbbbbb 2 2 1\n\
                   author Second Author\n\
                   author-time 1600000000\n\
                   summary Second\n\
                   \tcontent\n";
        let a = parse_porcelain(raw).unwrap();
        assert_eq!(a.author, "First Author");
        assert_eq!(a.summary, "First");
    }

    #[test]
    fn format_unix_date_known_values() {
        // 1970-01-01 UTC
        assert_eq!(format_unix_date(0), "1970-01-01");
        // 2000-01-01 UTC = 946684800
        assert_eq!(format_unix_date(946684800), "2000-01-01");
        // 2024-02-29 UTC (leap day) = 1709164800
        assert_eq!(format_unix_date(1709164800), "2024-02-29");
    }

    struct FakeEngine;
    impl BlameEngine for FakeEngine {
        fn blame(&self, _: &Path, _: &Path, _: u32) -> Option<Attribution> {
            Some(Attribution {
                commit: "deadbee".into(),
                author: "Test Author".into(),
                date: "2025-06-01".into(),
                summary: "Write the README".into(),
            })
        }
    }

    struct NullEngine;
    impl BlameEngine for NullEngine {
        fn blame(&self, _: &Path, _: &Path, _: u32) -> Option<Attribution> {
            None
        }
    }

    fn div() -> Divergence {
        use crate::domain::{Location, RuleId, Severity};
        Divergence {
            rule: RuleId::SymbolAbsence,
            severity: Severity::Critical,
            location: Location::new("README.md", 42),
            stated: "x".into(),
            reality: "y".into(),
            risk: "z".into(),
            attribution: None,
        }
    }

    #[test]
    fn apply_fills_attribution_from_engine() {
        let divs = apply(vec![div()], Path::new("."), &FakeEngine);
        let a = divs[0].attribution.as_ref().unwrap();
        assert_eq!(a.commit, "deadbee");
        assert_eq!(a.author, "Test Author");
    }

    #[test]
    fn apply_leaves_none_when_engine_cannot_resolve() {
        let divs = apply(vec![div()], Path::new("."), &NullEngine);
        assert!(divs[0].attribution.is_none());
    }
}
