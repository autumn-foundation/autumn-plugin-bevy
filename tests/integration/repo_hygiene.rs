//! Repository rules: the documents, the CI jobs and the Verus spec exist.

use std::path::Path;

fn read(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{}: {e}", full.display()))
}

// ── AC-29 ──────────────────────────────────────────────────────────────────

#[test]
fn docs_and_adrs_exist() {
    for path in [
        "README.md",
        "CHANGELOG.md",
        "CLAUDE.md",
        "docs/adr/0001-dedicated-thread-and-command-queue.md",
        "docs/adr/0002-failure-and-backpressure-policy.md",
        "docs/adr/0003-start-in-on-startup.md",
    ] {
        let text = read(path);
        assert!(text.len() > 200, "{path} is too short");
    }
    let changelog = read("CHANGELOG.md");
    assert!(
        changelog.contains("## [Unreleased]"),
        "CHANGELOG.md needs an Unreleased section"
    );
    let readme = read("README.md");
    for needle in [
        "```mermaid",
        "BevyPlugin::new",
        "[bevy]",
        "bevy_ticks_total",
        "Readiness",
    ] {
        assert!(readme.contains(needle), "README.md must mention {needle}");
    }
}

// ── AC-30 ──────────────────────────────────────────────────────────────────

#[test]
fn ci_workflow_has_required_jobs() {
    let ci = read(".github/workflows/ci.yml");
    for job in [
        "fmt:",
        "clippy:",
        "test:",
        "coverage:",
        "msrv:",
        "docs:",
        "ci-gate:",
    ] {
        assert!(
            ci.contains(&format!("\n  {job}")),
            "ci.yml needs the job {job}"
        );
    }
    for needle in [
        "ubuntu-latest",
        "macos-latest",
        "windows-latest",
        "--fail-under-lines 85",
        "1.95",
        "-D warnings",
        "--no-default-features",
    ] {
        assert!(ci.contains(needle), "ci.yml must contain {needle}");
    }
    let gate = ci.split("\n  ci-gate:").nth(1).expect("ci-gate job");
    for job in ["fmt", "clippy", "test", "coverage", "msrv", "docs"] {
        assert!(gate.contains(job), "ci-gate must need {job}");
    }
}

// ── AC-28 ──────────────────────────────────────────────────────────────────

#[test]
fn verus_spec_and_job_exist() {
    let spec = read("verification/bevy_runtime.rs");
    for lemma in [
        "terminal_is_absorbing",
        "rank_goes_up",
        "steps_keep_the_invariant",
        "no_command_is_lost",
        "no_work_after_failure",
        "health_up_means_running",
        "deadline_is_in_the_future",
        "deadlines_go_up",
        "late_ticks_are_skipped",
        "one_update_per_due_deadline",
    ] {
        assert!(
            spec.contains(&format!("proof fn {lemma}")),
            "spec needs {lemma}"
        );
    }
    let job = read(".github/workflows/verus.yml");
    assert!(job.contains("scripts/verify-verus.sh"));
    let script = read("scripts/verify-verus.sh");
    assert!(script.contains("verification/*.rs"));
}
