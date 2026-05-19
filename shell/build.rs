// shell/build.rs
//
// Invokes framec on the Frame source files this crate uses, writing the
// generated Rust into OUT_DIR. The generated files are include!()'d from
// src/frame_systems.rs.
//
// Failure modes:
//   - framec not installed: returns a clear error explaining how to install it
//   - .frs file missing: cargo's standard "rerun-if-changed" surfaces the error
//   - framec invocation fails: stderr is captured and shown
//
// Add a new Frame system by:
//   1. Adding `frame/<name>.frs` to the repo's frame/ directory
//   2. Appending `("<name>", "<name>.frs")` to FRAME_SYSTEMS below
//   3. Adding `include!(concat!(env!("OUT_DIR"), "/<name>.rs"));` to
//      src/frame_systems.rs

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};

// (module_name, source_filename_relative_to_frame_dir)
//
// module_name determines the generated .rs filename in OUT_DIR.
const FRAME_SYSTEMS: &[(&str, &str)] = &[("shell", "shell.frs"), ("parser", "parser.frs")];

fn main() -> Result<()> {
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest_dir.parent().ok_or_else(|| {
        anyhow!(
            "could not find workspace root from {}",
            manifest_dir.display()
        )
    })?;
    let frame_dir = workspace_root.join("frame");
    let out_dir = PathBuf::from(env_var("OUT_DIR")?);

    check_framec_installed()?;

    for (module, source) in FRAME_SYSTEMS {
        let input = frame_dir.join(source);
        let output = out_dir.join(format!("{module}.rs"));
        compile_frame_source(&input, &output)?;
        // Tell cargo to re-run this script when the .frs file changes.
        println!("cargo:rerun-if-changed={}", input.display());
    }

    // Also re-run if this build script or the workspace's frame/ dir changes.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", frame_dir.display());

    Ok(())
}

fn check_framec_installed() -> Result<()> {
    let status = Command::new("framec").arg("--version").output();
    match status {
        Ok(out) if out.status.success() => Ok(()),
        Ok(_) | Err(_) => Err(anyhow!(
            "\n\
            framec is not installed or not on PATH.\n\
            Install it with: `cargo install framec`\n\
            Then re-run `cargo build`.\n\
            "
        )),
    }
}

fn compile_frame_source(input: &Path, output: &Path) -> Result<()> {
    if !input.exists() {
        return Err(anyhow!("Frame source not found: {}", input.display()));
    }

    // framec writes <output_dir>/<input_stem>.rs. We assume the .rs filename
    // we want in OUT_DIR matches the input stem (e.g. shell.frs -> shell.rs).
    let out_dir = output
        .parent()
        .ok_or_else(|| anyhow!("output path {} has no parent dir", output.display()))?;

    let result = Command::new("framec")
        .arg("compile")
        .arg("-l")
        .arg("rust")
        .arg("-o")
        .arg(out_dir)
        .arg(input)
        .output()
        .with_context(|| format!("failed to invoke framec on {}", input.display()))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(anyhow!(
            "framec failed for {}:\n{}",
            input.display(),
            stderr
        ));
    }

    if !output.exists() {
        return Err(anyhow!(
            "framec did not produce expected output at {} \
             (input stem must match the module name in FRAME_SYSTEMS)",
            output.display()
        ));
    }

    Ok(())
}

fn env_var(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} is not set"))
}
