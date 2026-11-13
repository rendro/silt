//! A failed scheduled run reaches someone.
//!
//! The scheduled workflows (the nightly fuzz and oracle jobs, the weekly
//! dependency audit) once failed for weeks with nobody told, and a fuzz
//! crash could not fail its job. Now: each scheduled workflow ends with a
//! `report` job that opens or updates one issue per failed job, a fuzz
//! crash fails its job, and the release refuses to start while the newest
//! finished run of either workflow on main is not green. These tests
//! read the workflow files as text and hold those three things.

use std::path::PathBuf;

fn workflow(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".github/workflows")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// The jobs of a workflow's text: each job's name and its lines (the
/// lines from `  <name>:` to the next job), in their order.
fn jobs(text: &str) -> Vec<(String, Vec<&str>)> {
    let mut jobs: Vec<(String, Vec<&str>)> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if !line.starts_with([' ', '#']) && !line.is_empty() {
            inside = line == "jobs:";
            continue;
        }
        if !inside {
            continue;
        }
        let job = line
            .strip_prefix("  ")
            .filter(|rest| !rest.starts_with([' ', '#']))
            .and_then(|rest| rest.strip_suffix(':'));
        match job {
            Some(name) => jobs.push((name.to_string(), Vec::new())),
            None => {
                if let Some((_, lines)) = jobs.last_mut() {
                    lines.push(line);
                }
            }
        }
    }
    jobs
}

/// The `report` job of the scheduled workflow `name`: last, after every
/// other job, only on the schedule and not when the run was cancelled,
/// the only job whose token may write issues, and it runs the script
/// with `label`.
fn holds_a_report_job(name: &str, label: &str) {
    let text = workflow(name);
    let jobs = jobs(&text);
    let (last, lines) = jobs.last().expect("the workflow has jobs");
    assert_eq!(last, "report", "{name}: the last job is the report job");
    let line_with = |start: &str| {
        lines
            .iter()
            .map(|line| line.trim())
            .find(|line| line.starts_with(start))
            .unwrap_or_else(|| panic!("{name}: the report job has no `{start}` line"))
    };
    let others: Vec<&str> = jobs[..jobs.len() - 1]
        .iter()
        .map(|(job, _)| job.as_str())
        .collect();
    assert_eq!(
        line_with("needs:"),
        format!("needs: [{}]", others.join(", ")),
        "{name}: the report job waits for every other job"
    );
    let condition = line_with("if:");
    for part in ["!cancelled()", "github.event_name == 'schedule'"] {
        assert!(
            condition.contains(part),
            "{name}: {condition} has no {part}"
        );
    }
    assert_eq!(
        line_with("run:"),
        format!("run: bash .github/scripts/report-failed-jobs.sh {label}"),
        "{name}"
    );
    assert!(lines.iter().any(|line| line.trim() == "issues: write"));
    assert_eq!(
        text.matches("issues: write").count(),
        1,
        "{name}: only the report job may write issues"
    );
    assert!(
        !text.contains("continue-on-error"),
        "{name}: a step that may fail without failing its job hides a failure"
    );
}

#[test]
fn a_failed_nightly_or_weekly_job_opens_an_issue() {
    holds_a_report_job("fuzz-nightly.yml", "nightly");
    holds_a_report_job("audit.yml", "weekly");
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/scripts/report-failed-jobs.sh");
    let script = std::fs::read_to_string(script).expect("the report script");
    assert!(script.contains("title=\"$label: $job failed\""));
    assert!(script.contains("gh issue comment") && script.contains("gh issue create"));
}

#[test]
fn a_fuzz_crash_fails_its_job_after_the_crash_file_is_saved() {
    let text = workflow("fuzz-nightly.yml");
    let (_, fuzz) = jobs(&text)
        .into_iter()
        .find(|(job, _)| job == "fuzz")
        .expect("the fuzz job");
    let fuzz = fuzz.join("\n");
    let step = |name: &str| {
        fuzz.find(&format!("- name: {name}"))
            .unwrap_or_else(|| panic!("no step {name}"))
    };
    let (run, upload) = (
        step("Fuzz ${{ matrix.target }}"),
        step("Upload reproducers"),
    );
    assert!(run < upload, "the crash file is saved after the run");
    // The fuzz step's failure is the job's: nothing in it lets a crash
    // pass as a success.
    let fuzz_step = &fuzz[run..upload];
    for hides in ["continue-on-error", "|| true", "set +e", "|| :"] {
        assert!(
            !fuzz_step.contains(hides),
            "the fuzz step holds `{hides}`: a crash would not fail its job"
        );
    }
    assert!(
        fuzz[upload..].contains("if: always()"),
        "the upload runs after a failed run"
    );
}

#[test]
fn a_release_waits_for_green_scheduled_runs() {
    let text = workflow("release.yml");
    let (_, prepare) = jobs(&text)
        .into_iter()
        .find(|(job, _)| job == "prepare")
        .expect("the prepare job");
    let prepare = prepare.join("\n");
    assert!(prepare.contains("for workflow in fuzz-nightly.yml audit.yml; do"));
    assert!(prepare.contains(
        "gh run list --workflow \"$workflow\" --branch main --status completed --limit 1"
    ));
    assert!(prepare.contains("\"\" | \"success \"*) ;;"));
    assert!(
        prepare.contains("actions: read"),
        "the gate reads the runs of other workflows"
    );
}
