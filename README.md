# 🛠️ Specification: `spec-drift`
**Subtitle:** *Semantic Coherence Analysis between Specification and Implementation*

[![Rust CI](https://github.com/asmuelle/spec-drift/actions/workflows/rust.yml/badge.svg?branch=main)](https://github.com/asmuelle/spec-drift/actions/workflows/rust.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust edition](https://img.shields.io/badge/rust-2024%20%7C%201.95%2B-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

## 1. Core Philosophy
`spec-drift` operates on the principle of **Single Source of Truth (SSOT) Verification**. It treats documentation, examples, and CI configs as "executable specifications." When the code changes, the specification must evolve, or it becomes a "lie."

`spec-drift` detects these "lies" by cross-referencing the semantic claims made in natural language (Markdown) against the structural reality of the Rust codebase.

---

## 2. The Coherence Pillars

`spec-drift` analyzes four primary surfaces to detect divergence:

### A. Documentation Drift (`README.md`, `AGENTS.md`, `docs/`)
*   **The Check:** Extracts mentioned function names, types, and architectural constraints from Markdown.
*   **Drift Detection:**
    *   **Symbol Absence:** The `README` mentions `fn connect_to_db()`, but that function was renamed to `fn init_connection()`.
    *   **Constraint Violation:** `AGENTS.md` states *"All API handlers must return a `Result<T, ApiError>`"*, but a new handler is found returning `Option<T>`.
    *   **Outdated Logic:** The docs describe a 3-step auth flow, but the code now implements a 2-step flow.

### B. Example Drift (`examples/*.rs`)
*   **The Check:** This is the "Hard Truth" check. It attempts to compile examples against the current library version.
*   **Drift Detection:**
    *   **Compilation Failure:** An example fails to compile because a public API changed.
    *   **Deprecated Usage:** The example uses a function marked with `#[deprecated]`.
    *   **Logic Gap:** The example demonstrates a feature that has been removed or fundamentally altered in the core logic.

### C. Test-Spec Drift (`tests/*.rs`, `#[test]`)
*   **The Check:** Compares the *intent* described in test names/doc-comments with the *assertion* logic.
*   **Drift Detection:**
    *   **The "Lying Test":** A test named `test_user_cannot_access_admin_panel` exists, but the assertion inside is commented out or merely checks for a `200 OK` instead of a `403 Forbidden`.
    *   **Missing Coverage:** The `README` claims the tool "supports concurrent writes," but no tests in the `tests/` directory exercise concurrency.

### D. CI/Infrastructure Drift (`.github/workflows/`, `Makefile`, `justfile`)
*   **The Check:** Matches the build/test commands in CI against the actual project structure.
*   **Drift Detection:**
    *   **Ghost Commands:** CI runs `cargo test --package legacy_crate`, but `legacy_crate` was merged into the main crate.
    *   **Environment Mismatch:** The `README` says the project requires `libssl-dev`, but the CI workflow is using a container that provides `openssl-devel`.

---

## 3. How It Works

`spec-drift` follows a ports-and-adapters (hexagonal) architecture. The domain model knows nothing about filesystems, parsers, or output formats — every I/O concern lives behind a trait at the edge.

```
┌──────────────────────────────────────────────────────────────────┐
│                            spec-drift                            │
│                                                                  │
│   Sources ──▶ Parsers ──▶ Domain Model ──▶ Analyzers             │
│   (adapters) (adapters)  (core, pure)      (use cases)           │
│                                │                                 │
│                                ▼                                 │
│                            Reporters                             │
│                            (adapters)                            │
└──────────────────────────────────────────────────────────────────┘
```

### Layers

| Layer | Responsibility | Key types / crates |
|---|---|---|
| **Sources**   | Enumerate project files; optionally diff against git `HEAD`.              | `FsWalker`, `GitHistory` (`ignore`, `git2`)                                     |
| **Parsers**   | Translate raw bytes into structured facts. Pure, cached per file.         | `syn` (`full`), `pulldown-cmark`, `serde_yaml`, `regex`                         |
| **Domain**    | Pure types. No I/O, no globals.                                           | `SpecClaim`, `CodeFact`, `Divergence`, `Severity`, `RuleId`, `Location`         |
| **Analyzers** | Implement `trait DriftAnalyzer`. Independent, parallel-safe under `rayon`.| `DocsAnalyzer`, `ExamplesAnalyzer`, `TestsAnalyzer`, `CiAnalyzer`               |
| **Reporters** | Serialize `Vec<Divergence>` to an output format.                          | `HumanReporter`, `JsonReporter`, `SarifReporter`, `FixPromptReporter`           |

### The core contract

```rust
pub trait DriftAnalyzer {
    fn id(&self) -> &'static str;
    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence>;
}

pub struct Divergence {
    pub rule:     RuleId,
    pub severity: Severity,
    pub location: Location,
    pub stated:   String,
    pub reality:  String,
    pub risk:     String,
}
```

Analyzers are independent by construction and run in parallel. Parsed ASTs are cached on `ProjectContext` so each source file is parsed at most once per run. Errors use a single `SpecDriftError` enum (`thiserror`) at library boundaries; the CLI crate wraps with `anyhow`.

---

## 4. Detection Confidence Matrix

Not every drift check is deterministic. The matrix below states the mechanism and confidence level of each rule, so users and CI gates can calibrate trust.

| Pillar    | Rule                   | Mechanism                                                                                  | Confidence              |
|-----------|------------------------|--------------------------------------------------------------------------------------------|-------------------------|
| Docs      | `symbol_absence`       | `syn` AST lookup for symbols mentioned in Markdown code spans.                             | **Deterministic**       |
| Docs      | `constraint_violation` | User-authored rule DSL (e.g. *"all handlers return `Result<_, ApiError>`"*) checked on AST.| **Heuristic**           |
| Docs      | `outdated_logic`       | LLM summarization + structural comparison.                                                 | **Experimental (LLM)**  |
| Examples  | `compile_failure`      | Thin wrapper over `cargo check --examples --message-format=json`.                          | **Deterministic**       |
| Examples  | `deprecated_usage`     | `cargo clippy` `deprecated` lint re-framed as drift.                                       | **Deterministic**       |
| Examples  | `logic_gap`            | LLM comparison of example narrative vs. current API surface.                               | **Experimental (LLM)**  |
| Tests     | `lying_test`           | Parse test-name intent (negative / positive / status class) vs. `assert!`/`assert_eq!` bodies. | **Heuristic**       |
| Tests     | `missing_coverage`     | README capability claims vs. test-corpus symbol coverage.                                  | **Heuristic**           |
| CI        | `ghost_command`        | Parse workflows / `Makefile` / `justfile`; cross-reference `cargo metadata`.               | **Deterministic**       |
| CI        | `env_mismatch`         | Normalize named deps (e.g. `libssl-dev` ≡ `openssl-devel`) and match CI image manifest.    | **Heuristic**           |

Confidence levels map to defaults:

- **Deterministic** — enabled by default, failures are actionable verdicts.
- **Heuristic** — enabled by default, reported at `warning` or lower. False positives are expected; inline ignores exist for a reason.
- **Experimental (LLM)** — **opt-in only**. Requires `[llm] enabled = true` and network/credential access. `--no-llm` disables globally.

`--strict` promotes every heuristic rule one severity level.

---

## 5. CLI Interface (UX)

```bash
# Analyze the entire project for coherence
spec-drift

# Focus a single pillar
spec-drift --docs
spec-drift --examples

# Generate a "Correction Prompt" for the AI to fix the drift
spec-drift --fix-prompt

# CI integration
spec-drift --format sarif --deny warning --baseline .spec-drift.baseline.json
```

### Flags

| Flag                                      | Purpose                                                                 |
|-------------------------------------------|-------------------------------------------------------------------------|
| `--docs`, `--examples`, `--tests`, `--ci` | Run a single pillar.                                                    |
| `--format {human,json,sarif}`             | Output format. Default `human`.                                         |
| `--deny <severity>`                       | Exit non-zero when divergences at or above `<severity>` exist.          |
| `--baseline <file>`                       | Accept existing divergences; fail only on *new* drift.                  |
| `--config <path>`                         | Path to `spec-drift.toml`. Default: walk up from CWD.                   |
| `--fix-prompt`                            | Emit a structured correction prompt instead of a report.                |
| `--strict`                                | Promote heuristic rules one severity level.                             |
| `--diff <ref>`                            | Only analyze files changed since the given git ref (e.g. `HEAD`).       |
| `--blame`                                 | Attribute each divergence to the commit/author/date that wrote the line.|
| `--no-llm`                                | Disable all LLM-backed checks regardless of config.                     |

### Exit codes

- `0` — no divergences at or above `--deny` threshold.
- `1` — divergences found.
- `2` — tool error (bad config, parse failure, I/O).

### Example Output
```text
📉 SPEC DRIFT REPORT: [3 Divergences Found]

❌ CRITICAL: symbol_absence
- File: README.md (Line 42)
- Stated: `Client::new` exists in the codebase
- Reality: no symbol named `new` found in the parsed Rust sources
- Risk: New developers and AI agents will reach for a non-existent API.
- Blame: abc1234 Ada Lovelace (2024-01-02): Initial README

⚠️  WARNING: ghost_command
- File: .github/workflows/ci.yml (Line 14)
- Stated: CI runs `cargo` against package `legacy_crate`
- Reality: `legacy_crate` is not a member of the workspace
- Risk: CI exercises a target that no longer exists; the step is a no-op at best.

🟡 NOTICE: missing_coverage
- File: README.md (Line 8)
- Stated: `place_order` is a capability the project exposes
- Reality: no test references `place_order` by name
- Risk: Capability claimed in the docs has no guard-rail in tests.
```

---

## GitHub Action

Run `spec-drift` in CI and publish results to GitHub code scanning:

```yaml
name: spec-drift
on: [push, pull_request]
jobs:
  drift:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      security-events: write
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }  # --blame needs full history
      - uses: asmuelle/spec-drift@main
        with:
          format: sarif
          output: spec-drift.sarif
          args: --blame --deny warning
      - uses: github/codeql-action/upload-sarif@v3
        if: always()
        with:
          sarif_file: spec-drift.sarif
          category: spec-drift
```

### Action inputs

| Input                | Default  | Purpose                                                                |
|----------------------|----------|------------------------------------------------------------------------|
| `version`            | `main`   | git ref (branch, tag, or SHA) to install.                              |
| `format`             | `human`  | Output format: `human`, `json`, or `sarif`.                            |
| `output`             | *(stdout)* | File path to write output to.                                         |
| `deny`               | `notice` | Fail the step when divergences at or above this severity exist.        |
| `args`               | *(empty)* | Extra arguments forwarded to `spec-drift` (e.g. `--blame --strict`). |
| `working-directory`  | `.`      | Directory the scan runs in.                                            |
| `anthropic-api-key`  | *(empty)* | Enables LLM-backed rules when combined with `[llm] enabled = true`. |

---

## 6. Configuration

Project-level config lives in `spec-drift.toml` at the project root. Every rule can be silenced in config, or inline at the source.

### `spec-drift.toml`

```toml
[project]
root    = "."
include = ["src/**", "examples/**", "tests/**", "docs/**", "README.md", "AGENTS.md"]
exclude = ["target/**", "vendor/**"]

[analyzers]
docs     = "on"       # on | off
examples = "on"
tests    = "on"
ci       = "on"

[severity]
# Deterministic
symbol_absence       = "critical"
compile_failure      = "critical"
ghost_command        = "warning"
deprecated_usage     = "warning"
# Heuristic
lying_test           = "critical"
constraint_violation = "warning"
env_mismatch         = "notice"
missing_coverage     = "notice"
# Experimental (LLM)
outdated_logic       = "notice"
logic_gap            = "notice"

[ignore]
rules   = ["outdated_logic"]
paths   = ["docs/legacy/**", "examples/archived/**"]
symbols = ["internal_*", "*_deprecated"]

[llm]
enabled   = false              # opt-in; --no-llm always wins
provider  = "anthropic"        # anthropic | openai | local
model     = "claude-sonnet-4-6"
max_calls = 50                 # per run; fail closed when exceeded
timeout_s = 30

# User-authored structural rules parsed from AGENTS.md or declared here.
[rules.constraint_violation.handlers_return_result]
glob    = "src/handlers/**"
pattern = "fn * -> Result<_, ApiError>"
```

### Inline ignores

Inline ignores must name the rule. A blanket ignore with no rule ID is rejected — silence must be specific and auditable.

```rust
// Rust: accept a specific divergence on the next item.
#[allow(spec_drift::symbol_absence)]
pub fn legacy_shim() {}
```

```markdown
<!-- spec-drift: ignore-rule symbol_absence -->
Use `Client::new()` to connect.
```

```yaml
# .github/workflows/ci.yml
# spec-drift: ignore-rule ghost_command
- run: cargo test --package legacy_crate
```

### Baselines

`--baseline <file>` snapshots the current set of divergences. Subsequent runs only fail on *new* drift. Use this to adopt `spec-drift` on a legacy repo without a flag-day cleanup:

```bash
spec-drift --format json > .spec-drift.baseline.json
spec-drift --baseline .spec-drift.baseline.json --deny warning
```

---

## 7. The "Vibe Coding" Grand Architecture

`spec-drift` is the final stage of a complete **AI-Native Development Lifecycle (AIDL)**. Each tool owns one failure mode of AI-assisted coding.

| Stage         | Tool             | Responsibility          | Purpose                                                          |
| :------------ | :--------------- | :---------------------- | :--------------------------------------------------------------- |
| **1. Input**  | `cargo-context`  | Context Engineering     | Ensure the AI is "smart" enough to start.                        |
| **2. Filter** | `diff-risk`      | Semantic Guardrails     | Prevent the AI from introducing "silent" disasters.              |
| **3. Verify** | `cargo-impact`   | Blast Radius Analysis   | Prove the change works and name everything it touches.           |
| **4. Align**  | `spec-drift`     | Coherence Verification  | Keep docs, tests, examples, and CI honest with the code.         |

### How the stages compose

```
            ┌──────────────┐   ┌────────────┐   ┌──────────────┐   ┌──────────────┐
  intent ─▶ │ cargo-context│─▶ │ diff-risk  │─▶ │ cargo-impact │─▶ │  spec-drift  │ ─▶ merge
            └──────────────┘   └────────────┘   └──────────────┘   └──────────────┘
              Load smarts        Block bad         Prove correct       Keep honest
```

- `cargo-context` gives the agent the right **information**.
- `diff-risk` vetoes the wrong **change**.
- `cargo-impact` confirms the right **result**.
- `spec-drift` keeps the **story** the repo tells about itself true.

Skip any stage and drift creeps back in somewhere else. `spec-drift` is the last line of defense: once the code is right, it makes sure the rest of the repo agrees.
