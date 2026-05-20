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
//   qemu              — boot the bare-metal kernel in QEMU x86_64 via Limine UEFI
//   qemu-test         — run kernel QEMU smoke tests (stub until B0 Step 4)
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
        SubCmd::Qemu => run_qemu(),
        SubCmd::QemuTest => run_qemu_test(),
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
    ("job_control.frs", "job_control.svg"),
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
// QEMU (B0 Step 1: kernel boots via Limine UEFI)
// ---------------------------------------------------------------------------

/// Limine v-branch with prebuilt binaries to use. The Limine project
/// publishes prebuilts on `vN.x-binary` branches; the source-only tarball
/// would require building Limine itself which we don't want as a
/// dependency. The `limine` crate version in kernel/Cargo.toml ("0.5" ->
/// protocol revision 3) is compatible with Limine v11.x.
const LIMINE_BINARY_BRANCH: &str = "v11.x-binary";
const LIMINE_REPO: &str = "https://github.com/limine-bootloader/limine.git";

fn run_qemu() -> Result<()> {
    let workspace = workspace_root()?;

    let kernel_elf = build_kernel(&workspace)?;
    let limine_dir = ensure_limine_binaries(&workspace)?;
    let esp_img = build_esp_image(&workspace, &kernel_elf, &limine_dir)?;

    let (ovmf_code, ovmf_vars_template) = find_ovmf()?;
    let ovmf_vars = workspace.join("target").join("qemu").join("ovmf-vars.fd");
    std::fs::create_dir_all(ovmf_vars.parent().unwrap())?;
    if !ovmf_vars.exists() {
        // OVMF vars file must be writable; copy QEMU's read-only template.
        std::fs::copy(&ovmf_vars_template, &ovmf_vars)
            .with_context(|| format!("failed to copy {}", ovmf_vars_template.display()))?;
    }

    eprintln!("booting kernel in QEMU (Ctrl-C or Ctrl-A x to quit)...");
    let status = Command::new("qemu-system-x86_64")
        .args(["-machine", "q35", "-cpu", "qemu64", "-m", "256M"])
        // UEFI firmware (split into read-only code + writable NVRAM).
        .args(["-drive"])
        .arg(format!(
            "if=pflash,format=raw,readonly=on,file={}",
            ovmf_code.display()
        ))
        .args(["-drive"])
        .arg(format!("if=pflash,format=raw,file={}", ovmf_vars.display()))
        // Boot drive — real FAT image with our ESP layout.
        .args(["-drive"])
        .arg(format!("format=raw,file={}", esp_img.display()))
        // No graphical window; serial to stdio so the kernel banner
        // appears in the terminal where xtask ran.
        .args(["-display", "none", "-serial", "stdio"])
        // Don't reboot on triple fault; hold QEMU open after halt.
        .args(["-no-reboot", "-no-shutdown"])
        .status()
        .context("failed to invoke qemu-system-x86_64")?;

    if !status.success() {
        bail!("qemu exited with status: {status}");
    }
    Ok(())
}

fn run_qemu_test() -> Result<()> {
    bail!("cargo xtask qemu-test — not implemented yet (lands at B0 Step 4)");
}

/// Build kernel.elf via `cargo build -p frame-os-kernel --target x86_64-unknown-none`.
/// Returns the path to the built ELF.
fn build_kernel(workspace: &Path) -> Result<PathBuf> {
    let status = Command::new("cargo")
        .current_dir(workspace)
        .args([
            "build",
            "-p",
            "frame-os-kernel",
            "--target",
            "x86_64-unknown-none",
        ])
        .status()
        .context("failed to invoke cargo for kernel build")?;
    if !status.success() {
        bail!("kernel build failed");
    }
    let elf = workspace
        .join("target")
        .join("x86_64-unknown-none")
        .join("debug")
        .join("frame-os-kernel");
    if !elf.exists() {
        bail!("kernel ELF not found at {}", elf.display());
    }
    Ok(elf)
}

/// Ensure the Limine bootloader binaries are present at target/limine/.
/// Shallow-clones the `v11.x-binary` branch on first invocation; cached
/// thereafter. The binary branch ships prebuilt UEFI / BIOS bootloader
/// images, which we just need to copy into our ESP layout.
fn ensure_limine_binaries(workspace: &Path) -> Result<PathBuf> {
    let limine_dir = workspace.join("target").join("limine");
    let bootx64 = limine_dir.join("BOOTX64.EFI");
    if bootx64.exists() {
        return Ok(limine_dir);
    }

    eprintln!("fetching Limine {LIMINE_BINARY_BRANCH} binaries...");
    // Wipe any previous failed clone so git clone has a clean target.
    if limine_dir.exists() {
        std::fs::remove_dir_all(&limine_dir)
            .with_context(|| format!("failed to clear {}", limine_dir.display()))?;
    }

    let status = Command::new("git")
        .args(["clone", "--depth", "1", "--branch", LIMINE_BINARY_BRANCH])
        .arg(LIMINE_REPO)
        .arg(&limine_dir)
        .status()
        .context("failed to invoke git (is it installed?)")?;
    if !status.success() {
        bail!("git clone failed for {LIMINE_REPO}#{LIMINE_BINARY_BRANCH}");
    }

    if !bootx64.exists() {
        bail!("Limine clone succeeded but {} not found", bootx64.display());
    }
    Ok(limine_dir)
}

/// Build a raw FAT16 disk image containing the EFI System Partition layout.
/// Returns the path to the image file. Uses mtools (mformat, mmd, mcopy)
/// for FAT manipulation — QEMU's built-in `fat:rw:dir` vvfat driver is
/// quirky on UEFI boot, so we produce a real image instead.
///
/// Layout inside the image:
///   /EFI/BOOT/BOOTX64.EFI   ← Limine UEFI bootloader
///   /kernel.elf             ← our compiled kernel
///   /limine.conf            ← Limine configuration
fn build_esp_image(workspace: &Path, kernel_elf: &Path, limine_dir: &Path) -> Result<PathBuf> {
    let img = workspace.join("target").join("limine-esp.img");

    // Write limine.conf to a temp file we can `mcopy` later.
    let conf_tmp = workspace.join("target").join("limine.conf");
    let limine_conf = "\
# Limine configuration generated by `cargo xtask qemu`.
# `serial: yes` mirrors Limine's own boot output to the COM1 port so we
# can see what Limine is doing through `-serial stdio` in QEMU. Without
# this we get only display output (which `-display none` discards).
timeout: 0
default_entry: 1
serial: yes

/Frame OS
    protocol: limine
    kernel_path: boot():/kernel.elf
";
    std::fs::write(&conf_tmp, limine_conf).context("failed to write limine.conf")?;

    // Create a 64 MiB raw image (plenty of headroom for the bootloader,
    // kernel, and config — we'll trim later if needed).
    const IMG_SIZE_MIB: u64 = 64;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&img)
        .with_context(|| format!("failed to create {}", img.display()))?;
    f.set_len(IMG_SIZE_MIB * 1024 * 1024)
        .with_context(|| format!("failed to size {}", img.display()))?;
    drop(f);

    // mformat: write a FAT16 boot sector + filesystem header into the image.
    // -F forces FAT16. -i specifies the image file. ::  is mtools's syntax
    // for "the drive defined by the -i path."
    let status = Command::new("mformat")
        .args(["-i"])
        .arg(&img)
        .args(["-F", "::"])
        .status()
        .context("failed to invoke mformat (is mtools installed? `brew install mtools`)")?;
    if !status.success() {
        bail!("mformat failed");
    }

    // mmd: create the EFI/BOOT directory hierarchy inside the image.
    for dir in ["::/EFI", "::/EFI/BOOT"] {
        let status = Command::new("mmd")
            .args(["-i"])
            .arg(&img)
            .arg(dir)
            .status()
            .context("failed to invoke mmd")?;
        if !status.success() {
            bail!("mmd {dir} failed");
        }
    }

    // mcopy: copy our files into the image.
    let copies: &[(&Path, &str)] = &[
        (&limine_dir.join("BOOTX64.EFI"), "::/EFI/BOOT/BOOTX64.EFI"),
        (kernel_elf, "::/kernel.elf"),
        (&conf_tmp, "::/limine.conf"),
    ];
    for (src, dst) in copies {
        let status = Command::new("mcopy")
            .args(["-i"])
            .arg(&img)
            .arg(src)
            .arg(dst)
            .status()
            .context("failed to invoke mcopy")?;
        if !status.success() {
            bail!("mcopy {} -> {dst} failed", src.display());
        }
    }

    Ok(img)
}

/// Locate QEMU's bundled OVMF UEFI firmware. Returns (code, vars-template).
fn find_ovmf() -> Result<(PathBuf, PathBuf)> {
    // Standard QEMU Homebrew layout:
    //   /usr/local/Cellar/qemu/<ver>/share/qemu/edk2-x86_64-code.fd
    //   /usr/local/Cellar/qemu/<ver>/share/qemu/edk2-i386-vars.fd
    // OR /opt/homebrew/Cellar/qemu/... on Apple Silicon.
    // Linux package layouts vary; we'd extend this later.
    let candidate_dirs = [
        "/usr/local/share/qemu",
        "/opt/homebrew/share/qemu",
        "/usr/share/qemu",
        "/usr/share/OVMF",
    ];

    // Prefer the brew-bundled paths; fall back to scanning the candidates.
    for dir in candidate_dirs {
        let code = PathBuf::from(dir).join("edk2-x86_64-code.fd");
        let vars = PathBuf::from(dir).join("edk2-i386-vars.fd");
        if code.exists() && vars.exists() {
            return Ok((code, vars));
        }
    }

    // Try to discover via brew (Homebrew installs into /usr/local/Cellar on
    // Intel and /opt/homebrew/Cellar on Apple Silicon).
    if let Ok(out) = Command::new("brew").args(["--prefix", "qemu"]).output() {
        if out.status.success() {
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let share = PathBuf::from(prefix).join("share").join("qemu");
            let code = share.join("edk2-x86_64-code.fd");
            let vars = share.join("edk2-i386-vars.fd");
            if code.exists() && vars.exists() {
                return Ok((code, vars));
            }
        }
    }

    bail!(
        "could not locate OVMF UEFI firmware (edk2-x86_64-code.fd + edk2-i386-vars.fd). \
         On macOS install via `brew install qemu`; on Linux install the `ovmf` package."
    );
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
