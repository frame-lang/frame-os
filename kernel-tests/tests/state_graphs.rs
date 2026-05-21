// kernel-tests/tests/state_graphs.rs
//
// Level 2 test: state-graph snapshot for the Kernel HSM.
//
// Invokes `framec -l graphviz` on frame/kernel.frs and compares the DOT
// output against a committed insta snapshot. A failure means the source's
// state graph changed; review with `cargo insta review` and accept if
// intentional. Mirrors shell/tests/state_graphs.rs.
//
// This test only needs `framec` (not `dot`); if framec isn't on PATH the
// test skips rather than fails, matching the shell crate's behavior.

use std::path::PathBuf;
use std::process::Command;

fn frame_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("crate manifest dir has a parent (workspace root)")
        .join("frame")
}

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
fn kernel_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("kernel.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("kernel_state_graph", dot);
}

#[test]
fn serial_driver_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("serial_driver.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("serial_driver_state_graph", dot);
}

#[test]
fn task_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("task.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("task_state_graph", dot);
}

#[test]
fn scheduler_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("scheduler.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("scheduler_state_graph", dot);
}

#[test]
fn page_fault_handler_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("page_fault_handler.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("page_fault_handler_state_graph", dot);
}
