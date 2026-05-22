// kernel-tests/build.rs
//
// Generate the host-target Rust for the kernel's Frame systems from the
// shared frame/*.frs sources. Same invocation as kernel/build.rs and
// shell/build.rs — framec output is target-agnostic, so the same .frs
// compiles for both x86_64-unknown-none (the kernel bin) and the host
// (this test crate).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};

// (module_name, source_filename_relative_to_frame_dir)
const FRAME_SYSTEMS: &[(&str, &str)] = &[
    ("serial_driver", "serial_driver.frs"),
    ("kernel", "kernel.frs"),
    ("task", "task.frs"),
    ("scheduler", "scheduler.frs"),
    ("page_fault_handler", "page_fault_handler.frs"),
    ("syscall_dispatcher", "syscall_dispatcher.frs"),
    ("process", "process.frs"),
    ("process_table", "process_table.frs"),
    ("elf_loader", "elf_loader.frs"),
    ("block_request", "block_request.frs"),
    ("mount", "mount.frs"),
    ("open_file", "open_file.frs"),
    ("arp_resolver", "arp_resolver.frs"),
    ("rx_pipeline", "rx_pipeline.frs"),
    ("udp_socket", "udp_socket.frs"),
];

fn main() -> Result<()> {
    let manifest = PathBuf::from(env_var("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest
        .parent()
        .ok_or_else(|| anyhow!("could not find workspace root from {}", manifest.display()))?;
    let frame_dir = workspace_root.join("frame");
    let out_dir = PathBuf::from(env_var("OUT_DIR")?);

    check_framec_installed()?;

    for (module, source) in FRAME_SYSTEMS {
        let input = frame_dir.join(source);
        let output = out_dir.join(format!("{module}.rs"));
        compile_frame_source(&input, &output)?;
        println!("cargo:rerun-if-changed={}", input.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", frame_dir.display());

    Ok(())
}

fn check_framec_installed() -> Result<()> {
    match Command::new("framec").arg("--version").output() {
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
            "framec did not produce expected output at {}",
            output.display()
        ));
    }
    Ok(())
}

fn env_var(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} is not set"))
}
