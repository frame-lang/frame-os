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
    ("elf_loader", "elf_loader.frs"),
    ("block_request", "block_request.frs"),
    ("mount", "mount.frs"),
    ("open_file", "open_file.frs"),
    ("arp_resolver", "arp_resolver.frs"),
    ("rx_pipeline", "rx_pipeline.frs"),
    ("udp_socket", "udp_socket.frs"),
    ("tcp_connection", "tcp_connection.frs"),
    ("ip_reassembly", "ip_reassembly.frs"),
    ("hub_port", "hub_port.frs"),
    ("usb_enumeration", "usb_enumeration.frs"),
    ("usb_transfer", "usb_transfer.frs"),
    ("usb_msd", "usb_msd.frs"),
    ("event_counter", "event_counter.frs"),
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

    // --- User program (B3 Step 4) -----------------------------------------
    // Build the freestanding user crate and stage its ELF where usermode.rs
    // can `include_bytes!` it (concat!(env!("OUT_DIR"), "/user_hello.elf")).
    build_user_program(workspace_root, &out_dir)?;

    Ok(())
}

/// Compile the freestanding `user/` crate (a standalone, workspace-excluded
/// bare-metal package) into a static ELF and copy it to OUT_DIR/user_hello.elf.
///
/// The nested `cargo` runs with its own target dir (no lock contention with
/// the kernel build) and with the outer build's RUSTFLAGS scrubbed, so the
/// kernel's link args can't leak into the user link — the user crate's own
/// `.cargo/config.toml` supplies its target + linker script.
fn build_user_program(workspace_root: &Path, out_dir: &Path) -> Result<()> {
    let user_dir = workspace_root.join("user");
    let user_target = out_dir.join("user-target");

    let status = Command::new(env_var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(&user_dir)
        .env("CARGO_TARGET_DIR", &user_target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        // Scrub the rustc wrapper too: when the kernel is *clippy*-checked,
        // cargo sets RUSTC_WORKSPACE_WRAPPER=clippy-driver, and without this
        // the nested user build would inherit it and get clippy-checked
        // (failing on user-code lints). The baked ELF must build identically
        // regardless of whether the outer command is build/clippy/test.
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .args(["build", "--release"])
        .status()
        .with_context(|| {
            format!(
                "failed to invoke cargo for user crate at {}",
                user_dir.display()
            )
        })?;
    if !status.success() {
        return Err(anyhow!("user program build failed"));
    }

    // Stage each built binary's ELF where usermode.rs include_bytes!s it.
    let release = user_target.join("x86_64-unknown-none").join("release");
    for (bin, staged_name) in [
        ("hello", "user_hello.elf"),
        ("faulter", "user_faulter.elf"),
        ("forker", "user_forker.elf"),
        ("spawner", "user_spawner.elf"),
        ("waiter", "user_waiter.elf"),
        ("brktest", "user_brktest.elf"),
        ("fwtest", "user_fwtest.elf"),
        ("shell", "user_shell.elf"),
        ("frameshell", "user_frameshell.elf"),
        ("ish", "user_ish.elf"),
    ] {
        let elf = release.join(bin);
        if !elf.exists() {
            return Err(anyhow!("user program ELF not found at {}", elf.display()));
        }
        let staged = out_dir.join(staged_name);
        std::fs::copy(&elf, &staged).with_context(|| {
            format!(
                "failed to stage user ELF {} -> {}",
                elf.display(),
                staged.display()
            )
        })?;
    }

    // Rebuild whenever the user sources change.
    for f in [
        "src/main.rs",
        "src/faulter.rs",
        "src/forker.rs",
        "src/spawner.rs",
        "src/waiter.rs",
        "src/brktest.rs",
        "src/fwtest.rs",
        "src/shell.rs",
        "src/frameshell.rs",
        "src/ish.rs",
        "src/cmain.rs",
        "src/frame_systems.rs",
        "src/mem.rs",
        "build.rs",
        "linker.ld",
        "Cargo.toml",
        ".cargo/config.toml",
        // frame-os-libc (B10): a sibling path dependency of the user crate.
        "../libc/src/lib.rs",
        "../libc/src/malloc.rs",
        "../libc/src/printf.rs",
        "../libc/src/stdio.rs",
        "../libc/src/frame_systems.rs",
        "../libc/build.rs",
        "../libc/frame/printf_scan.frs",
        "../libc/Cargo.toml",
        // frame-libc reuses the kernel's OpenFile FSM (frame/open_file.frs) for
        // FILE* mode gating; rebuild the staged programs if it changes.
        "../frame/open_file.frs",
    ] {
        println!("cargo:rerun-if-changed={}", user_dir.join(f).display());
    }

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
