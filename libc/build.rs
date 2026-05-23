// libc/build.rs
//
// Compiles frame-libc's own Frame sources (B10-3) into `$OUT_DIR`, where
// `src/frame_systems.rs` `include!`s them. These are libc-specific FSMs (the
// printf format-spec scanner; later the FILE* stream lifecycle), so they live
// in `libc/frame/` rather than the shared `frame/` dir. The generated Rust is
// `no_std`-clean (only `alloc::` + the prelude names the include module
// re-exports), matching the kernel/user pattern.
//
// framec must be on PATH (`cargo install framec`) — the kernel/user/shell
// builds already require it, so this adds no new toolchain dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest.parent().expect("libc crate has a parent");
    let out_dir = PathBuf::from(env("OUT_DIR"));

    println!("cargo:rerun-if-changed=build.rs");

    // (module stem, source .frs). `printf_scan` is libc-specific; `open_file` is
    // the *same* FSM the kernel compiles (frame/open_file.frs) — frame-libc reuses
    // it to gate FILE* read/write modes (one source, two targets, B10-3b).
    let sources = [
        ("printf_scan", manifest.join("frame").join("printf_scan.frs")),
        ("open_file", workspace_root.join("frame").join("open_file.frs")),
    ];

    for (module, input) in &sources {
        let output = out_dir.join(format!("{module}.rs"));
        compile_frame_source(input, &out_dir, &output);
        println!("cargo:rerun-if-changed={}", input.display());
    }
}

fn compile_frame_source(input: &Path, out_dir: &Path, expected_output: &Path) {
    assert!(input.exists(), "Frame source not found: {}", input.display());

    let result = Command::new("framec")
        .arg("compile")
        .arg("-l")
        .arg("rust")
        .arg("-o")
        .arg(out_dir)
        .arg(input)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke framec on {}: {e}", input.display()));

    assert!(
        result.status.success(),
        "framec failed for {}:\n{}",
        input.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        expected_output.exists(),
        "framec did not produce expected output at {}",
        expected_output.display()
    );
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}
