// xtask/src/main.rs
//
// Internal build orchestration for Frame OS.
//
// Invoked as `cargo xtask <subcommand>` thanks to the entry in
// .cargo/config.toml that aliases `xtask` to `run -p xtask --`.
//
// Subcommands:
//   install-tools     — install framec and Rust targets needed by the project
//   check-diagrams    — regenerate state-graph SVGs and assert no drift
//   regen-diagrams    — regenerate state-graph SVGs (commits the new content)
//   qemu              — boot the bare-metal kernel in QEMU (B0+, stub for now)
//   qemu-test         — run kernel QEMU smoke tests (B0+, stub for now)
//
// Adding a new subcommand:
//   1. Add a variant to the Subcommand enum
//   2. Add its handler match arm in main()
//   3. Document it in the doc comment above

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Internal build orchestration for Frame OS")]
struct Cli {
    #[command(subcommand)]
    command: SubCmd,
}

#[derive(Subcommand)]
enum SubCmd {
    /// Install framec and Rust bare-metal targets.
    ///
    /// What this currently does:
    ///   - Checks whether framec is installed; if not, runs `cargo install framec`
    ///   - Adds the rustup targets x86_64-unknown-none, aarch64-unknown-none,
    ///     and thumbv6m-none-eabi (Pi Pico)
    ///
    /// What this does NOT do:
    ///   - Install QEMU (use your package manager: brew, apt, choco)
    ///   - Install GraphViz (use your package manager)
    InstallTools,

    /// Regenerate all state-graph SVGs from .frs sources and assert no drift.
    ///
    /// Run this in CI to detect changes to .frs source that weren't committed
    /// alongside an updated .svg.
    CheckDiagrams,

    /// Regenerate all state-graph SVGs from .frs sources, overwriting committed copies.
    ///
    /// Run this locally after intentional .frs changes, then commit the new SVGs.
    RegenDiagrams,

    /// Boot the bare-metal kernel in QEMU. (Stub until B0 lands.)
    Qemu,

    /// Run the bare-metal kernel's QEMU smoke tests. (Stub until B0 lands.)
    QemuTest,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        SubCmd::InstallTools => install_tools(),
        SubCmd::CheckDiagrams => diagrams(DiagramMode::Check),
        SubCmd::RegenDiagrams => diagrams(DiagramMode::Regen),
        SubCmd::Qemu => stub_qemu(),
        SubCmd::QemuTest => stub_qemu_test(),
    }
}

// ---------------------------------------------------------------------------
// install-tools
// ---------------------------------------------------------------------------

fn install_tools() -> Result<()> {
    println!("Checking framec installation...");
    if !is_framec_installed() {
        println!("framec not found; installing via `cargo install framec`...");
        let status = Command::new("cargo")
            .args(["install", "framec"])
            .status()
            .context("failed to invoke cargo")?;
        if !status.success() {
            bail!("`cargo install framec` failed");
        }
    } else {
        println!("framec is already installed.");
    }

    println!("Adding Rust bare-metal targets via rustup...");
    let targets = [
        "x86_64-unknown-none",  // QEMU x86_64 kernel
        "aarch64-unknown-none", // QEMU aarch64 / Pi 4/5 kernel
        "thumbv6m-none-eabi",   // Pi Pico (RP2040)
    ];
    for target in targets {
        println!("  rustup target add {target}");
        let status = Command::new("rustup")
            .args(["target", "add", target])
            .status()
            .context("failed to invoke rustup")?;
        if !status.success() {
            eprintln!(
                "warning: `rustup target add {target}` failed; \
                 you may need to install rustup or add the target manually"
            );
        }
    }

    println!();
    println!("Done. Tools NOT installed by this command:");
    println!("  - QEMU         (install via your package manager: brew/apt/choco)");
    println!("  - GraphViz dot (install via your package manager)");

    Ok(())
}

fn is_framec_installed() -> bool {
    Command::new("framec")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// check-diagrams / regen-diagrams
// ---------------------------------------------------------------------------

enum DiagramMode {
    /// Generate to a temp file and compare to the committed copy.
    Check,
    /// Generate and overwrite the committed copy.
    Regen,
}

/// (frs_filename_relative_to_frame_dir, output_svg_relative_to_docs_systems_dir)
const DIAGRAMS: &[(&str, &str)] = &[
    ("shell.frs", "shell.svg"),
    ("parser.frs", "parser.svg"),
    ("job.frs", "job.svg"),
];

fn diagrams(mode: DiagramMode) -> Result<()> {
    let workspace_root = workspace_root()?;
    let frame_dir = workspace_root.join("frame");
    let systems_dir = workspace_root.join("docs").join("systems");

    if !is_framec_installed() {
        bail!("framec is not installed. Run `cargo xtask install-tools` first.");
    }
    if !is_dot_installed() {
        bail!(
            "GraphViz `dot` is not installed. Install via your package manager \
             (e.g. `brew install graphviz`, `apt install graphviz`)."
        );
    }

    let mut drift_count = 0;

    for (frs, svg) in DIAGRAMS {
        let frs_path = frame_dir.join(frs);
        let svg_path = systems_dir.join(svg);
        let generated = generate_svg(&frs_path)?;

        match mode {
            DiagramMode::Regen => {
                std::fs::write(&svg_path, &generated)
                    .with_context(|| format!("failed to write {}", svg_path.display()))?;
                println!("wrote {}", svg_path.display());
            }
            DiagramMode::Check => {
                let committed = std::fs::read(&svg_path).ok();
                let matches = committed.as_deref() == Some(generated.as_slice());
                if !matches {
                    drift_count += 1;
                    eprintln!(
                        "drift: {} differs from generated output for {}",
                        svg_path.display(),
                        frs_path.display()
                    );
                } else {
                    println!("ok: {}", svg_path.display());
                }
            }
        }
    }

    if matches!(mode, DiagramMode::Check) && drift_count > 0 {
        bail!("{drift_count} diagram(s) out of date. Run `cargo xtask regen-diagrams` and commit.");
    }

    Ok(())
}

fn generate_svg(frs_path: &Path) -> Result<Vec<u8>> {
    let dot_output = Command::new("framec")
        .arg(frs_path)
        .arg("-l")
        .arg("graphviz")
        .output()
        .context("failed to invoke framec")?;
    if !dot_output.status.success() {
        bail!(
            "framec failed for {}: {}",
            frs_path.display(),
            String::from_utf8_lossy(&dot_output.stderr)
        );
    }

    let mut child = Command::new("dot")
        .args(["-Tsvg"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("failed to invoke dot")?;
    {
        use std::io::Write;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("failed to open dot stdin"))?;
        stdin
            .write_all(&dot_output.stdout)
            .context("failed to write to dot stdin")?;
    }
    let svg_output = child.wait_with_output().context("failed to wait for dot")?;
    if !svg_output.status.success() {
        bail!(
            "dot failed: {}",
            String::from_utf8_lossy(&svg_output.stderr)
        );
    }
    Ok(svg_output.stdout)
}

fn is_dot_installed() -> bool {
    Command::new("dot")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// QEMU stubs (B0+)
// ---------------------------------------------------------------------------

fn stub_qemu() -> Result<()> {
    bail!("cargo xtask qemu — not implemented yet (lands at B0)");
}

fn stub_qemu_test() -> Result<()> {
    bail!("cargo xtask qemu-test — not implemented yet (lands at B0)");
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest_dir
        .parent()
        .ok_or_else(|| anyhow!("xtask is not inside a workspace"))?
        .to_path_buf())
}
