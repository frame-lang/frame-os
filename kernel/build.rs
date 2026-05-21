// kernel/build.rs
//
// Two jobs:
//   1. Tell rustc to use our custom linker script that lays out the kernel
//      for Limine's higher-half load address, and to emit a static
//      (ET_EXEC) ELF.
//   2. Invoke framec on the kernel's Frame sources (B0 Step 2: the Kernel
//      HSM; Step 3 adds SerialDriver), writing the generated Rust into
//      OUT_DIR where src/frame_systems.rs `include!`s it. Mirrors the
//      shell crate's build.rs.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};

// (module_name, source_filename_relative_to_frame_dir)
//
// module_name determines the generated .rs filename in OUT_DIR; it must
// match the input stem (kernel.frs -> kernel.rs) and the `include!` in
// src/frame_systems.rs.
const FRAME_SYSTEMS: &[(&str, &str)] = &[
    ("serial_driver", "serial_driver.frs"),
    ("scheduler", "scheduler.frs"),
    ("page_fault_handler", "page_fault_handler.frs"),
    ("syscall_dispatcher", "syscall_dispatcher.frs"),
    ("process", "process.frs"),
    ("process_table", "process_table.frs"),
    ("kernel", "kernel.frs"),
];

fn main() -> Result<()> {
    let manifest = PathBuf::from(env_var("CARGO_MANIFEST_DIR")?);
    let linker_script = manifest.join("linker.ld");

    // --- Linker configuration (B0 Step 1) ---------------------------------
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", linker_script.display());

    // Pass the linker script via rustc to LLD. The -T flag is the
    // standard "use this linker script" directive for ld-shaped linkers.
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());

    // Force a static (ET_EXEC) ELF instead of the default PIE (ET_DYN).
    // The Limine boot protocol's static-kernel path requires ET_EXEC;
    // ET_DYN would need a PT_DYNAMIC segment with relocations that we
    // don't emit.
    println!("cargo:rustc-link-arg=-static");
    println!("cargo:rustc-link-arg=--no-pie");

    // --- Frame codegen (B0 Step 2) ----------------------------------------
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

    // framec writes <output_dir>/<input_stem>.rs.
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
