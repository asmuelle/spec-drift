# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-04-23

Initial public release. Every rule in the README confidence matrix is
implemented, every CLI flag in the table is wired, and every `spec-drift.toml`
block parses.

### Added

#### Rules (Docs pillar)
- `symbol_absence` (deterministic, Critical) — Markdown inline code spans are
  matched against the `syn`-parsed public surface; Rust-shape filters reject
  prose / all-caps / bare lowercase words to keep signal high.
- `constraint_violation` (heuristic, Warning) — user-declared
  `[[rules.constraint_violation]]` in `spec-drift.toml`, with return-type
  matching via a bracket-aware `_` wildcard (`Result<_, ApiError>` matches
  `Result<Vec<User>, ApiError>`).
- `missing_coverage` (heuristic, Notice) — README function-shaped claims
  whose target function exists but is never referenced by any test-scope
  file. Word-boundary match prevents `new` from spuriously matching `renew`.
- `outdated_logic` (experimental, Notice) — LLM-backed. Splits Markdown at
  H2/H3, compares section prose against matching `fn` bodies.

#### Rules (Examples pillar)
- `compile_failure` (deterministic, Critical) — `cargo check --examples
  --message-format=json` diagnostics under `examples/` surface as drift.
- `deprecated_usage` (deterministic, Warning) — `cargo clippy --examples`
  `deprecated`-coded warnings re-framed as drift.
- `logic_gap` (experimental, Notice) — LLM-backed. Compares each
  `examples/*.rs` leading `//!` narrative against the library's public `fn`
  surface.

#### Rules (Tests pillar)
- `lying_test` (heuristic, Critical) — negatively-named `#[test]` fns with
  only positive assertions. Detection-prefix allow-list (`flags_`,
  `detects_`, `does_not_`, ...) exempts analyzer self-tests.

#### Rules (CI pillar)
- `ghost_command` (deterministic, Warning) — `cargo --package` / `--bin`
  names in `Makefile`, `justfile`, or `.github/workflows/*.yml` that don't
  match `cargo metadata`.
- `env_mismatch` (heuristic, Notice) — README-listed system packages with
  no matching `apt-get|apk|yum|dnf|brew install` line, after cross-distro
  normalization (`libssl-dev` ≡ `openssl-devel`).

#### Reporters
- `HumanReporter` — glyph-prefixed severity, per-divergence block with
  optional `- Blame:` line.
- `JsonReporter` — `serde` round-trip, drives the `--baseline` format.
- `SarifReporter` — SARIF 2.1.0 for GitHub code scanning; emits
  `partialFingerprints.commitSha` when blame is present.
- `FixPromptReporter` — structured Markdown brief consumable by an AI for
  correction work.

#### CLI
- `--format {human,json,sarif}`, `--fix-prompt`, `--deny <severity>`,
  `--baseline <file>`, `--config <path>`, `--strict`, `--no-llm`, `--diff
  <ref>`, `--blame`, per-pillar `--docs` / `--examples` / `--tests` / `--ci`.

#### Config (`spec-drift.toml`)
- `[severity]` overrides per rule.
- `[ignore]` suppression by rule / path glob / symbol glob.
- `[[rules.constraint_violation]]` user-declared structural rules.
- `[llm]` with `enabled` / `provider` / `model` / `max_calls` / `timeout_s`.

#### Architecture
- Ports-and-adapters layout: `sources → parsers → domain → analyzers →
  reporters`.
<!-- spec-drift: ignore-rule symbol_absence -->
- `rayon::par_iter` across analyzers (each one is `Send + Sync`).
- `GitHistory::changed_files` + `GitHistory::narrow` back `--diff <ref>` for
  pre-commit use.
- Inline-ignore engine: `spec-drift: ignore-rule <id>` in any comment
  syntax, plus `#[allow(spec_drift::<id>)]`. Lookback is 4 lines to cover
  stacked Rust attributes.
- Baseline engine: identity tuple `(rule, file, line, stated)` is loose on
  prose so analyzer tweaks don't invalidate snapshots.
- `LlmClient` trait + `NullLlmClient` default + `AnthropicLlmClient` with
  `ANTHROPIC_API_KEY`. `BudgetedClient` atomic counter wraps the provider
  so rayon can't overspend `max_calls`. Fail-closed: any `None` verdict
  skips silently — experimental rules never flag on incomplete evidence.
- `--blame` attribution via `git blame --porcelain`, parsed into
  `Attribution { commit, author, date, summary }`. Unix timestamp → date
  via Howard Hinnant's `civil_from_days` (no `chrono` dep).

#### Workspace support
- `--package <name>` narrows analysis to a single workspace member, paths
  discovered via `cargo metadata`.

#### Distribution
- Composite GitHub Action at `action.yml` — installs `spec-drift` (from a
  pre-built release binary when available, `cargo install --git` fallback).
- `.github/workflows/rust.yml` self-check job uploads SARIF to GitHub code
  scanning on every push/PR.
- `.github/workflows/release.yml` builds Linux x86_64, macOS arm64, and
  macOS x86_64 binaries on tagged releases. Publishes to crates.io when
  `CARGO_REGISTRY_TOKEN` is set in repo secrets.

[Unreleased]: https://github.com/asmuelle/spec-drift/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/asmuelle/spec-drift/releases/tag/v0.1.0
