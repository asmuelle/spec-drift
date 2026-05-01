use std::path::Path;
use std::process::Command;

fn spec_drift_bin() -> &'static str {
    env!("CARGO_BIN_EXE_spec-drift")
}

fn run_git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("git should launch");
    assert!(
        out.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn diff_reports_doc_drift_induced_by_changed_rust() {
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "Use `old_name()` to start.\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn old_name() {}\n").unwrap();

    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Test User"]);
    run_git(root, &["add", "."]);
    run_git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-m", "init"],
    );

    std::fs::write(root.join("src/lib.rs"), "pub fn new_name() {}\n").unwrap();

    let out = Command::new(spec_drift_bin())
        .args([
            "--root",
            root.to_str().unwrap(),
            "--docs",
            "--diff",
            "HEAD",
            "--format",
            "json",
            "--deny",
            "critical",
        ])
        .output()
        .expect("spec-drift should launch");

    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let first = json.as_array().unwrap().first().unwrap();
    assert_eq!(first["rule"], "symbol_absence");
    assert_eq!(first["location"]["file"], "README.md");
}
