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

#[test]
fn syscall_dispatcher_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("syscall_dispatcher.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("syscall_dispatcher_state_graph", dot);
}

#[test]
fn process_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("process.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("process_state_graph", dot);
}

#[test]
fn process_table_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("process_table.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("process_table_state_graph", dot);
}

#[test]
fn elf_loader_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("elf_loader.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("elf_loader_state_graph", dot);
}

#[test]
fn block_request_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("block_request.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("block_request_state_graph", dot);
}

#[test]
fn io_scheduler_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("io_scheduler.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("io_scheduler_state_graph", dot);
}

#[test]
fn mount_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("mount.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("mount_state_graph", dot);
}

#[test]
fn open_file_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("open_file.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("open_file_state_graph", dot);
}

#[test]
fn arp_resolver_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("arp_resolver.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("arp_resolver_state_graph", dot);
}

#[test]
fn rx_pipeline_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("rx_pipeline.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("rx_pipeline_state_graph", dot);
}

#[test]
fn udp_socket_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("udp_socket.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("udp_socket_state_graph", dot);
}

#[test]
fn tcp_connection_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("tcp_connection.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("tcp_connection_state_graph", dot);
}

#[test]
fn ip_reassembly_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("ip_reassembly.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("ip_reassembly_state_graph", dot);
}

#[test]
fn hub_port_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("hub_port.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("hub_port_state_graph", dot);
}

#[test]
fn usb_enumeration_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("usb_enumeration.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("usb_enumeration_state_graph", dot);
}

#[test]
fn usb_transfer_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("usb_transfer.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("usb_transfer_state_graph", dot);
}

#[test]
fn usb_msd_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("usb_msd.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("usb_msd_state_graph", dot);
}

#[test]
fn event_counter_state_graph_snapshot() {
    let Some(dot) = frame_to_dot("event_counter.frs") else {
        return; // framec unavailable; test skipped
    };
    insta::assert_snapshot!("event_counter_state_graph", dot);
}
