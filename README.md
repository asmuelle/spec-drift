# 🛠️ Specification: `spec-drift`
**Subtitle:** *Semantic Coherence Analysis between Specification and Implementation*

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

## 3. CLI Interface (UX)

```bash
# Analyze the entire project for coherence
spec-drift

# Focus only on documentation and agent instructions
spec-drift --docs

# Check if examples are still valid (compile-check)
spec-drift --examples

# Generate a "Correction Prompt" for the AI to fix the drift
spec-drift --fix-prompt
```

### Example Output:
```text
📉 SPEC DRIFT REPORT: [3 Divergences Found]

❌ CRITICAL: Documentation Drift
- File: README.md (Line 42)
- Stated: "Use `Client::new()` to initialize the connection."
- Reality: `Client::new()` was replaced by `Client::builder().build()`.
- Risk: New developers and AI agents will use deprecated/non-existent APIs.

⚠️ WARNING: Agent Instruction Drift
- File: AGENTS.md (Line 12)
- Stated: "Always use `tokio::sync::Mutex` for shared state."
- Reality: Found 3 instances of `std::sync::Mutex` in `src/state.rs`.
- Risk: AI will be confused by conflicting patterns.

🟡 NOTICE: Example Drift
- File: examples/basic_usage.rs
- Status: Fails to compile.
- Error: Mismatched types in `send_request` call.
```

---

## 4. The "Vibe Coding" Grand Architecture

You now have a complete **AI-Native Development Lifecycle (AIDL)**.

| Stage | Tool | Responsibility | Purpose |
| :--- | :--- | :--- | :--- |
| **1. Input** | `cargo-context` | Context Engineering | Ensure the AI is "smart" enough to start. |
| **2. Filter** | `diff-risk` | Semantic Guardrails | Prevent the AI from introducing "silent" disasters. |
| **3. Verify** | `cargo-impact` | Blast Radius Analysis | Prove the change works
