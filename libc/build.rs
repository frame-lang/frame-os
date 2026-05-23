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

// (module_name, source_filename in libc/frame). module_name is the generated
// .rs stem in OUT_DIR; it must match the input stem.
const FRAME_SYSTEMS: &[(&str, &str)] = &[("printf_scan", "printf_scan.frs")];

fn main() {
    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let frame_dir = manifest.join("frame");
    let out_dir = PathBuf::from(env("OUT_DIR"));

    println!("cargo:rerun-if-changed=build.rs");

    for (module, source) in FRAME_SYSTEMS {
        let input = frame_dir.join(source);
        let output = out_dir.join(format!("{module}.rs"));
        compile_frame_source(&input, &out_dir, &output);
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
