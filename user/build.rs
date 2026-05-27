// user/build.rs
//
// Compiles the Frame sources the ring-3 programs reuse (B4 Step 4b) into
// `$OUT_DIR`, where `src/frame_systems.rs` `include!`s them. The headline is
// `parser.frs` — the *same* source the hosted shell compiles (`shell/build.rs`)
// — now built for `x86_64-unknown-none`. The generated Rust is `no_std`-clean
// (only `alloc::` + the prelude names `String`/`Vec`/`Box`, which the include
// module re-exports), so it drops straight into the freestanding user crate.
//
// framec must be on PATH (`cargo install framec`); the kernel/shell builds
// already require it, so this adds no new toolchain dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

// (module_name, source_filename_relative_to_../frame). module_name is the
// generated .rs stem in OUT_DIR; it must match the input stem.
const FRAME_SYSTEMS: &[(&str, &str)] = &[
    ("parser", "parser.frs"),
    // Pipeline (M3a): the shared command-line grammar FSM — the SAME
    // frame/pipeline.frs the hosted shell compiles. ish drives its parsing
    // through Parser -> Pipeline (one FSM source, both targets), retiring its
    // hand-written parse_redirs / pipe split. src/frame_systems.rs include!s it.
    ("pipeline", "pipeline.frs"),
    // Shell (M3b): the SAME frame/shell.frs control-flow FSM the hosted shell
    // compiles. Generated for x86_64-unknown-none; ish.rs (only) include!s it in
    // a local module + supplies an IshShellEnv (the ShellEnv seam over
    // syscalls). NOT included in the shared frame_systems.rs — other user bins
    // don't reuse the Shell and shouldn't have to provide a ShellEnv.
    ("shell", "shell.frs"),
    // Job + JobControl (M4): the SAME hosted job-control FSMs, now compiled for
    // ring 3 over a syscall ProcessBackend — unifying the job table across
    // targets (retiring the ish-specific IshJobs). ish.rs include!s them in its
    // local shell_fsm module.
    ("job", "job.frs"),
    ("job_control", "job_control.frs"),
    // IshJobs (S10): the interactive shell's job-control FSM ($Idle/$Foreground
    // + background-job table), the ish-resident adaptation of the hosted-shell
    // job_control.frs. Built for x86_64-unknown-none; src/frame_systems.rs
    // include!s it. Tracks pids ish forks rather than spawning them itself.
    ("ish_jobs", "ish_jobs.frs"),
    // BuildDriver (B11-3e): the on-device toolchain pipeline FSM, driven by the
    // `build` bin. Generated to OUT_DIR; `src/build_frame.rs` include!s it.
    ("builddriver", "builddriver.frs"),
    // Hello (V1.0 capstone): the same hello.frs that framec also transpiles to
    // C; here generated to Rust and driven by the `fhello` bin (src/hello_frame.rs
    // include!s it). One Frame source, both backends.
    ("hello", "hello.frs"),
];

fn main() {
    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .parent()
        .expect("user crate has a parent (workspace root)");
    let frame_dir = workspace_root.join("frame");
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
    assert!(
        input.exists(),
        "Frame source not found: {}",
        input.display()
    );

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
