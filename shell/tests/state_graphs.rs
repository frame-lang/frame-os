// shell/tests/state_graphs.rs
//
// Level 2 tests: state-graph snapshots.
//
// For each Frame system this crate uses, we invoke `framec -l graphviz` and
// compare the output against a committed snapshot via the insta crate.
//
// A failure means the source's state graph has changed. To accept the new
// graph: `cargo insta review`, look at the diff, and accept if intentional.
//
// To regenerate snapshots after a deliberate change:
//   cargo insta test
//   cargo insta review
//
// These tests require `framec` and `dot` (GraphViz) to be installed; if they
// aren't, the test is skipped with a clear message rather than failing.

use std::path::PathBuf;
use std::process::Command;

/// Returns the path to the workspace's frame/ directory.
fn frame_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("crate manifest dir has a parent (workspace root)")
        .join("frame")
}

/// Run framec on the given .frs file with the graphviz target.
/// Returns the DOT output as a string, or None if framec isn't available.
fn frame_to_dot(frs_filename: &str) -> Option<String> {
    let frs_path = frame_dir().join(frs_filename);
    if !frs_path.exists() {
        panic!("Frame source not found: {}", frs_path.display());
    }

    let output = match Command::new("framec")
        .arg(&frs_path)
        .arg("-l")
        .arg("graphviz")
        .output()
    {
        Ok(out) => out,
        Err(_) => {
            // framec not installed: skip rather than fail. Same behavior as
            // ignoring a test on a platform that can't run it.
            eprintln!(
                "skipping state graph snapshot test: framec not on PATH \
                 (install with `cargo install framec`)"
            );
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("framec failed on {}:\n{}", frs_path.display(), stderr);
    }

    Some(String::from_utf8(output.stdout).expect("framec graphviz output is UTF-8"))
}

#[test]
fn shell_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("shell.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("shell_state_graph", dot);
}

#[test]
fn parser_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("parser.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("parser_state_graph", dot);
}

#[test]
fn pipeline_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("pipeline.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("pipeline_state_graph", dot);
}

#[test]
fn job_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("job.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("job_state_graph", dot);
}

#[test]
fn job_control_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("job_control.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("job_control_state_graph", dot);
}
