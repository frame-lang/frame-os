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
//   qemu-test         — run kernel QEMU smoke tests (B0 Step 4+)
//
// Adding a new subcommand:
//   1. Add a variant to the Subcommand enum
//   2. Add its handler match arm in main()
//   3. Document it in the doc comment above

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

    /// Boot the bare-metal kernel in QEMU x86_64 via Limine UEFI.
    /// Serial is routed to stdio; Ctrl-A x quits QEMU.
    Qemu,

    /// Run the bare-metal kernel's QEMU smoke tests headlessly.
    /// Each test boots the kernel, captures serial output to file,
    /// and asserts expected substrings appear (and panic markers
    /// do not). Fails the whole run non-zero if any test fails.
    QemuTest,

    /// B5-3: boot the kernel on a real TAP link and `ping` it from the host.
    /// Requires a Linux host with NET_ADMIN + /dev/net/tun (the dev container
    /// with `TAP=1 docker/run.sh "cargo xtask qemu-tap"`). Sets up `tap0`,
    /// boots QEMU with `-netdev tap`, pings the guest, and asserts a reply.
    QemuTap,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        SubCmd::InstallTools => install_tools(),
        SubCmd::CheckDiagrams => diagrams(DiagramMode::Check),
        SubCmd::RegenDiagrams => diagrams(DiagramMode::Regen),
        SubCmd::Qemu => run_qemu(),
        SubCmd::QemuTest => run_qemu_test(),
        SubCmd::QemuTap => run_qemu_tap(),
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
    ("kernel.frs", "kernel.svg"),
    ("serial_driver.frs", "serial_driver.svg"),
    ("task.frs", "task.svg"),
    ("scheduler.frs", "scheduler.svg"),
    ("page_fault_handler.frs", "page_fault_handler.svg"),
    ("syscall_dispatcher.frs", "syscall_dispatcher.svg"),
    ("process.frs", "process.svg"),
    ("process_table.frs", "process_table.svg"),
    ("elf_loader.frs", "elf_loader.svg"),
    ("block_request.frs", "block_request.svg"),
    ("mount.frs", "mount.svg"),
    ("open_file.frs", "open_file.svg"),
    ("arp_resolver.frs", "arp_resolver.svg"),
    ("rx_pipeline.frs", "rx_pipeline.svg"),
    ("udp_socket.frs", "udp_socket.svg"),
    ("tcp_connection.frs", "tcp_connection.svg"),
    ("ip_reassembly.frs", "ip_reassembly.svg"),
    ("hub_port.frs", "hub_port.svg"),
    ("usb_enumeration.frs", "usb_enumeration.svg"),
    ("usb_transfer.frs", "usb_transfer.svg"),
    ("event_counter.frs", "event_counter.svg"),
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

/// Artifacts needed by every QEMU invocation: built kernel ELF, OVMF
/// firmware (read-only code + writable NVRAM vars copy), and the ESP
/// disk image that Limine boots from. Produced once per `cargo xtask`
/// run and reused across each smoke test in a `qemu-test` batch.
struct QemuArtifacts {
    esp_img: PathBuf,
    ovmf_code: PathBuf,
    /// QEMU's read-only vars template. UEFI mutates NVRAM during boot, so
    /// QEMU needs a *writable* copy — but we never hand it this template
    /// directly. Instead each invocation gets its own fresh copy (see
    /// `fresh_ovmf_vars`). That isolation matters: if a smoke test times
    /// out and QEMU is SIGKILLed mid-NVRAM-write, a shared vars file would
    /// be left corrupt and every *subsequent* boot would hang in firmware
    /// (serial capture shows only the OVMF screen-clear escapes, no kernel
    /// output). Per-invocation copies make that impossible to cascade.
    ovmf_vars_template: PathBuf,
    /// A blank raw disk template attached as a virtio-blk device (B4). Each
    /// invocation gets a fresh copy (see `fresh_blk_disk`) so a write test
    /// can't corrupt another's disk.
    blk_template: PathBuf,
    /// Directory under `target/` where per-invocation vars copies live.
    qemu_dir: PathBuf,
}

const BLK_DISK_BLOCKS: u32 = 2048; // 1 MiB / 512

fn prepare_qemu_artifacts(workspace: &Path) -> Result<QemuArtifacts> {
    let kernel_elf = build_kernel(workspace)?;
    let limine_dir = ensure_limine_binaries(workspace)?;
    let esp_img = build_esp_image(workspace, &kernel_elf, &limine_dir)?;

    let (ovmf_code, ovmf_vars_template) = find_ovmf()?;
    let qemu_dir = target_dir(workspace).join("qemu");
    std::fs::create_dir_all(&qemu_dir)?;

    // An mkfs'd virtio-blk disk template (B4): formatted with the Frame OS FS
    // and pre-populated with a couple of files plus a real ELF at /bin/hello,
    // which the userspace shell `exec`s from disk (B4 Step 4). Each invocation
    // copies the template.
    let blk_template = qemu_dir.join("blk-template.img");
    let hello_elf = build_user_disk_elf(workspace)?;
    let mut files: Vec<(&str, &[u8])> = FS_FILES.to_vec();
    files.push(("/bin/hello", &hello_elf));
    let image = build_fs_image(BLK_DISK_BLOCKS, &files);
    std::fs::write(&blk_template, &image)
        .with_context(|| format!("failed to write {}", blk_template.display()))?;

    Ok(QemuArtifacts {
        esp_img,
        ovmf_code,
        ovmf_vars_template,
        blk_template,
        qemu_dir,
    })
}

/// Files baked into the FS image by `mkfs` (absolute path, contents). Paths may
/// name one directory level (`/bin/info`); intermediate dirs are created.
const FS_FILES: &[(&str, &[u8])] = &[
    ("/motd", b"Frame OS B4 filesystem online.\n"),
    ("/readme", b"hello from the disk\n"),
    ("/bin/info", b"nested directory works\n"),
];

/// `mkfs`: build a Frame OS FS disk image (`total_blocks` × 512 bytes) and bake
/// in `files`. Mirrors the on-disk layout the kernel reads (shared::fs).
/// Supports one directory level: a path `/bin/info` creates a `/bin` directory.
fn build_fs_image(total_blocks: u32, files: &[(&str, &[u8])]) -> Vec<u8> {
    use frame_os_shared::fs;
    let mut disk = vec![0u8; total_blocks as usize * fs::BLOCK_SIZE];

    let blk = |b: u32| {
        let s = b as usize * fs::BLOCK_SIZE;
        s..s + fs::BLOCK_SIZE
    };
    fn set_used(disk: &mut [u8], b: u32) {
        let byte = fs::BITMAP_BLOCK as usize * fs::BLOCK_SIZE + (b as usize / 8);
        disk[byte] |= 1 << (b % 8);
    }

    for b in 0..fs::DATA_START {
        set_used(&mut disk, b);
    }

    // Directory registry: (name "" = root, inode, data block, running dirent
    // offset). Root is inode 1.
    let mut next_ino = fs::ROOT_INODE; // 1
    let mut next_data = fs::DATA_START;
    let alloc_dir = |next_ino: &mut u32, next_data: &mut u32, disk: &mut [u8]| -> (u32, u32) {
        let ino = *next_ino;
        *next_ino += 1;
        let dblk = *next_data;
        *next_data += 1;
        set_used(disk, dblk);
        (ino, dblk)
    };
    let (root_ino, root_data) = alloc_dir(&mut next_ino, &mut next_data, &mut disk);
    // dirs: (name, ino, data_block, dirent_off)
    let mut dirs: Vec<(&str, u32, u32, usize)> = vec![("", root_ino, root_data, 0)];

    for (path, data) in files {
        let p = path.trim_start_matches('/');
        let (dirname, name) = match p.rsplit_once('/') {
            Some((d, n)) => (d, n),
            None => ("", p),
        };
        // Get or create the parent directory (one level under root).
        let di = match dirs.iter().position(|d| d.0 == dirname) {
            Some(i) => i,
            None => {
                let (ino, dblk) = alloc_dir(&mut next_ino, &mut next_data, &mut disk);
                // Add the subdir's dirent to root.
                let (rblk, roff) = (dirs[0].2, dirs[0].3);
                fs::write_dirent(&mut disk[blk(rblk)], roff, dirname.as_bytes(), ino);
                dirs[0].3 += fs::DIRENT_SIZE;
                dirs.push((dirname, ino, dblk, 0));
                dirs.len() - 1
            }
        };

        // Allocate the file inode + data blocks.
        let ino = next_ino;
        next_ino += 1;
        let mut node = fs::Inode::empty();
        node.kind = fs::T_FILE;
        node.nlink = 1;
        node.size = data.len() as u32;
        let nb = data.len().div_ceil(fs::BLOCK_SIZE);
        for (i, item) in node.direct.iter_mut().enumerate().take(nb) {
            let b = next_data;
            next_data += 1;
            set_used(&mut disk, b);
            *item = b;
            let lo = i * fs::BLOCK_SIZE;
            let hi = ((i + 1) * fs::BLOCK_SIZE).min(data.len());
            let r = blk(b);
            disk[r.start..r.start + (hi - lo)].copy_from_slice(&data[lo..hi]);
        }
        let (iblk, ioff) = fs::inode_loc(ino);
        node.write(&mut disk[blk(iblk)], ioff);

        // Add the file's dirent to its parent directory.
        let (pblk, poff) = (dirs[di].2, dirs[di].3);
        fs::write_dirent(&mut disk[blk(pblk)], poff, name.as_bytes(), ino);
        dirs[di].3 += fs::DIRENT_SIZE;
    }

    // Write every directory inode with its final size.
    for &(_, ino, dblk, off) in &dirs {
        let mut d = fs::Inode::empty();
        d.kind = fs::T_DIR;
        d.nlink = 1;
        d.direct[0] = dblk;
        d.size = off as u32;
        let (iblk, ioff) = fs::inode_loc(ino);
        d.write(&mut disk[blk(iblk)], ioff);
    }

    let sb = fs::Superblock {
        magic: fs::MAGIC,
        total_blocks,
    };
    sb.write(&mut disk[0..fs::BLOCK_SIZE]);
    disk
}

/// Build the freestanding `hello` user program and return its ELF bytes, for
/// baking onto the FS image at `/bin/hello` (which the shell `exec`s from
/// disk). Mirrors the kernel build.rs nested-cargo invocation: own target dir,
/// scrubbed RUSTFLAGS + rustc wrapper so the outer build's link args / clippy
/// can't leak into the user link.
fn build_user_disk_elf(workspace: &Path) -> Result<Vec<u8>> {
    let user_dir = workspace.join("user");
    let user_target = target_dir(workspace).join("user-disk-elf");

    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(&user_dir)
        .env("CARGO_TARGET_DIR", &user_target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .args(["build", "--release", "--bin", "hello"])
        .status()
        .with_context(|| format!("failed to invoke cargo for user crate at {}", user_dir.display()))?;
    if !status.success() {
        bail!("user `hello` build failed");
    }

    let elf = user_target
        .join("x86_64-unknown-none")
        .join("release")
        .join("hello");
    std::fs::read(&elf).with_context(|| format!("failed to read user ELF {}", elf.display()))
}

/// Produce a fresh copy of the virtio-blk disk for one QEMU invocation.
fn fresh_blk_disk(artifacts: &QemuArtifacts, tag: &str) -> Result<PathBuf> {
    let disk = artifacts.qemu_dir.join(format!("blk-{tag}.img"));
    std::fs::copy(&artifacts.blk_template, &disk).with_context(|| {
        format!(
            "failed to copy blk template {} -> {}",
            artifacts.blk_template.display(),
            disk.display()
        )
    })?;
    Ok(disk)
}

/// Produce a fresh, writable copy of the OVMF NVRAM vars for a single QEMU
/// invocation, named after `tag`. Always overwrites any prior copy so a
/// corrupted file from an earlier (timed-out, SIGKILLed) run can never
/// poison the next boot.
fn fresh_ovmf_vars(artifacts: &QemuArtifacts, tag: &str) -> Result<PathBuf> {
    let vars = artifacts.qemu_dir.join(format!("ovmf-vars-{tag}.fd"));
    std::fs::copy(&artifacts.ovmf_vars_template, &vars).with_context(|| {
        format!(
            "failed to copy OVMF vars template {} -> {}",
            artifacts.ovmf_vars_template.display(),
            vars.display()
        )
    })?;
    Ok(vars)
}

/// Build a QEMU `Command` with the standard machine + firmware + disk
/// arguments. Callers add the serial routing they want
/// (`-serial stdio` for interactive, `-serial file:<path>` for smoke
/// tests) and then either spawn or run.
/// How QEMU's virtio-net `net0` netdev is wired up.
enum NetMode {
    /// User-mode networking (slirp): no host privilege, CI-friendly. The
    /// default for every smoke test. `guestfwd` is set only for the 4e
    /// active-open test (QEMU connects to the guestfwd target at startup,
    /// so a listener must already be up — we mustn't add it otherwise).
    Slirp { guestfwd: bool },
    /// A host TAP device (`ifname`), giving a real inbound L2 peer. Needs
    /// NET_ADMIN + /dev/net/tun (Linux container). Used by `qemu-tap` to
    /// validate the kernel's inbound ARP/ICMP responders (B5 Step 5).
    Tap { ifname: String },
}

fn qemu_base_command(
    artifacts: &QemuArtifacts,
    ovmf_vars: &Path,
    blk_disk: &Path,
    net: &NetMode,
) -> Command {
    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args(["-machine", "q35", "-cpu", "qemu64", "-m", "256M"])
        // B7: 4 cores. Limine starts the APs; the kernel brings them up + parks
        // them (B7 Step 1), then schedules across them (later steps). Harmless
        // for B0–B6 (the APs stay parked).
        .args(["-smp", "4"])
        // UEFI firmware (split into read-only code + writable NVRAM).
        .args(["-drive"])
        .arg(format!(
            "if=pflash,format=raw,readonly=on,file={}",
            artifacts.ovmf_code.display()
        ))
        .args(["-drive"])
        .arg(format!("if=pflash,format=raw,file={}", ovmf_vars.display()))
        // Boot drive — real FAT image with our ESP layout.
        .args(["-drive"])
        .arg(format!("format=raw,file={}", artifacts.esp_img.display()))
        // virtio-blk data disk (B4). `disable-modern=on` forces the legacy
        // (I/O BAR) virtio interface our driver speaks.
        .args(["-drive"])
        .arg(format!(
            "file={},if=none,id=blk0,format=raw",
            blk_disk.display()
        ))
        .args([
            "-device",
            "virtio-blk-pci,drive=blk0,disable-modern=on,disable-legacy=off",
        ])
        // virtio-net. `disable-modern=on` forces the legacy I/O-BAR interface
        // our driver speaks. Two wirings:
        //
        //   Slirp (user-mode networking): no host privilege, CI-friendly, the
        //   default for every smoke test. slirp answers ARP for the gateway
        //   (10.0.2.2), which the B5 Step 1 demo relies on. hostfwd forwards
        //   host 127.0.0.1:TCP_PROBE_PORT → guest :7 (the B5 4b/4c passive tests
        //   connect in). guestfwd (only for the 4e active-open test) forwards the
        //   guest's connection to 10.0.2.100:9 → host 127.0.0.1:TCP_ACTIVE_PORT.
        //
        //   Tap: a real host TAP device, giving an actual inbound L2 peer so a
        //   real `ping 10.0.2.15` from the host reaches the kernel's inbound
        //   ARP/ICMP responders (B5 Step 5). Needs NET_ADMIN + /dev/net/tun.
        .args(["-netdev"])
        .arg(match net {
            NetMode::Slirp { guestfwd: true } => format!(
                "user,id=net0,hostfwd=tcp::{TCP_PROBE_PORT}-:7,guestfwd=tcp:10.0.2.100:9-tcp:127.0.0.1:{TCP_ACTIVE_PORT}"
            ),
            NetMode::Slirp { guestfwd: false } => {
                format!("user,id=net0,hostfwd=tcp::{TCP_PROBE_PORT}-:7")
            }
            NetMode::Tap { ifname } => {
                format!("tap,id=net0,ifname={ifname},script=no,downscript=no")
            }
        })
        .args([
            "-device",
            "virtio-net-pci,netdev=net0,disable-modern=on,disable-legacy=off",
        ])
        // B6: an xHCI USB host controller with a HID keyboard attached. The
        // kernel's xhci::init() discovers the controller (PCI class 0C0330),
        // brings it up, and detects the keyboard connected on a port. Harmless
        // for B0–B5 (nothing touches USB there).
        .args(["-device", "qemu-xhci,id=xhci"])
        .args(["-device", "usb-kbd,bus=xhci.0"])
        .args(["-display", "none"])
        // isa-debug-exit: the kernel's halt path writes port 0xf4, which
        // makes QEMU exit cleanly (code (val<<1)|1) once the boot/demos are
        // done — so the smoke harness gets the complete serial capture
        // immediately instead of racing a timeout. Harmless on real hardware.
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        // `-no-reboot`: on a triple fault QEMU exits (instead of looping
        // through firmware), so the smoke harness surfaces an incomplete
        // capture rather than hanging to the timeout.
        //
        // We deliberately do NOT pass `-no-shutdown`: it intercepts the
        // isa-debug-exit device's process-exit path, which would leave QEMU
        // running after the kernel's clean halt and force every smoke test
        // to wait out its full timeout.
        .args(["-no-reboot"]);
    cmd
}

fn run_qemu() -> Result<()> {
    let workspace = workspace_root()?;
    let artifacts = prepare_qemu_artifacts(&workspace)?;

    eprintln!("booting kernel in QEMU (Ctrl-C or Ctrl-A x to quit)...");
    let ovmf_vars = fresh_ovmf_vars(&artifacts, "run")?;
    let blk_disk = fresh_blk_disk(&artifacts, "run")?;
    let mut cmd = qemu_base_command(
        &artifacts,
        &ovmf_vars,
        &blk_disk,
        &NetMode::Slirp { guestfwd: false },
    );
    cmd.args(["-serial", "stdio"]);
    let status = cmd
        .status()
        .context("failed to invoke qemu-system-x86_64")?;

    // The kernel's halt path writes `isa-debug-exit` (port 0xf4), which
    // makes QEMU exit with code 33 = `(0x10 << 1) | 1`. That's the normal,
    // healthy "kernel reached halt_forever()" outcome, so accept it.
    if !status.success() && status.code() != Some(33) {
        bail!("qemu exited with status: {status}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `cargo xtask qemu-tap` — real inbound networking over a host TAP (B5 Step 5)
//
// slirp can't give the kernel a genuine inbound L2 peer: it answers ARP/ICMP
// for its own virtual gateway, but nothing on the host can `ping 10.0.2.15` and
// reach the *guest's* responders. A TAP device can. This subcommand:
//
//   1. brings up a tap0 device, host side 10.0.2.1/24;
//   2. boots the kernel with `-netdev tap,ifname=tap0` (no slirp);
//   3. `ping`s 10.0.2.15 from the host in a retry loop while the guest boots;
//   4. asserts the ping succeeds AND the serial shows "[icmp] answered ping".
//
// It needs NET_ADMIN + /dev/net/tun, so it only runs in the Linux dev container
// launched with `TAP=1 docker/run.sh "cargo xtask qemu-tap"`. The guest reaches
// its inbound-serve window because, with no slirp gateway, ARP-gateway
// resolution fails and run_demo() falls into serve_inbound() (see net.rs).
// ---------------------------------------------------------------------------

const TAP_IFNAME: &str = "tap0";
const TAP_HOST_IP: &str = "10.0.2.1/24";
const GUEST_IP: &str = "10.0.2.15";

/// Run `ip <args>`, bailing with context on failure.
fn ip_cmd(args: &[&str]) -> Result<()> {
    let status = Command::new("ip")
        .args(args)
        .status()
        .with_context(|| format!("failed to invoke `ip {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`ip {}` failed with status: {status}", args.join(" "));
    }
    Ok(())
}

fn tap_setup() -> Result<()> {
    // Best-effort teardown of a stale tap0 from a previous aborted run, then
    // create it fresh, give the host an address on the link, and bring it up.
    let _ = Command::new("ip")
        .args(["tuntap", "del", "dev", TAP_IFNAME, "mode", "tap"])
        .status();
    ip_cmd(&["tuntap", "add", "dev", TAP_IFNAME, "mode", "tap"])?;
    ip_cmd(&["addr", "add", TAP_HOST_IP, "dev", TAP_IFNAME])?;
    ip_cmd(&["link", "set", TAP_IFNAME, "up"])?;
    Ok(())
}

fn tap_teardown() {
    let _ = Command::new("ip")
        .args(["tuntap", "del", "dev", TAP_IFNAME, "mode", "tap"])
        .status();
}

fn run_qemu_tap() -> Result<()> {
    let workspace = workspace_root()?;
    let artifacts = prepare_qemu_artifacts(&workspace)?;
    let ovmf_vars = fresh_ovmf_vars(&artifacts, "tap")?;
    let blk_disk = fresh_blk_disk(&artifacts, "tap")?;
    let serial_path = artifacts.qemu_dir.join("serial-tap.txt");
    let _ = std::fs::remove_file(&serial_path);

    tap_setup().context("TAP setup failed — run via `TAP=1 docker/run.sh` (needs NET_ADMIN + /dev/net/tun)")?;
    // Ensure the TAP device is torn down no matter how we exit below.
    let result = run_qemu_tap_inner(&artifacts, &ovmf_vars, &blk_disk, &serial_path);
    tap_teardown();
    result
}

fn run_qemu_tap_inner(
    artifacts: &QemuArtifacts,
    ovmf_vars: &Path,
    blk_disk: &Path,
    serial_path: &Path,
) -> Result<()> {
    let mut cmd = qemu_base_command(
        artifacts,
        ovmf_vars,
        blk_disk,
        &NetMode::Tap {
            ifname: TAP_IFNAME.to_string(),
        },
    );
    cmd.args(["-serial"])
        .arg(format!("file:{}", serial_path.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());

    eprintln!("[tap] booting kernel with tap0 (host {TAP_HOST_IP}); pinging guest {GUEST_IP}...");
    let mut child = cmd.spawn().context("failed to invoke qemu-system-x86_64")?;

    // Two probes during the guest's serve_inbound() window, each retried while it
    // boots. Success of each = a `ping` reply returns AND the serial confirms the
    // *guest* did the work (so we know the guest's responder/reassembly replied):
    //   1. a normal ping  → "[icmp] answered ping"            (B5-3, single frame)
    //   2. a 4000-byte ping (fragments at the 1500 MTU)       (B5-5, reassembly)
    //      → the guest reassembles ("[ip] reassembled …"), replies, and re-fragments
    //        the >MTU reply outbound so the host's ping round-trips.
    let mut small_ping = false;
    let mut frag_ping = false;
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if child.try_wait().context("failed to poll qemu process")?.is_some() {
            break; // kernel halted (serve window closed)
        }
        if !small_ping {
            if ping_once(GUEST_IP, None) {
                small_ping = true;
                eprintln!("[tap] ping {GUEST_IP}: reply received");
            }
        } else if !frag_ping {
            // Fragmented ping: -s 4000 → ~4028-byte datagram → 3 IP fragments.
            if ping_once(GUEST_IP, Some(4000)) {
                frag_ping = true;
                eprintln!("[tap] ping -s 4000 {GUEST_IP}: reassembled round-trip ok");
            }
        }
        let serial = std::fs::read_to_string(serial_path).unwrap_or_default();
        let answered = serial.contains("[icmp] answered ping");
        let reassembled = serial.contains("[ip] reassembled");
        if small_ping && frag_ping && answered && reassembled {
            break; // all signals seen — done early
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Always stop QEMU before the caller tears down tap0 — otherwise QEMU is
    // still holding the device's fd and `ip tuntap del` races it ("Device or
    // resource busy"). The kernel may already have halted (serve window closed),
    // in which case kill/wait are harmless no-ops.
    let _ = child.kill();
    let _ = child.wait();

    let serial = std::fs::read_to_string(serial_path).unwrap_or_default();
    let answered = serial.contains("[icmp] answered ping");
    let reassembled = serial.contains("[ip] reassembled");
    if !small_ping {
        bail!("TAP ping failed: no reply from {GUEST_IP} (serial answered={answered})");
    }
    if !answered {
        bail!("TAP ping: host got a reply but the kernel never logged \"[icmp] answered ping\"");
    }
    if !frag_ping {
        bail!(
            "TAP fragmented ping (-s 4000) failed: no reassembled round-trip from {GUEST_IP} \
             (serial reassembled={reassembled})"
        );
    }
    if !reassembled {
        bail!(
            "TAP fragmented ping: host got a reply but the kernel never logged \"[ip] reassembled\""
        );
    }
    // Confirm the ARP responder fired too (the ping reply implies it, but log it
    // explicitly so the oracle is complete).
    if serial.contains("[arp] answered who-has 10.0.2.15") {
        eprintln!("[tap] kernel answered ARP who-has + ICMP echo: ok");
    }
    eprintln!("[tap] OK — kernel answered a real inbound ping (B5-3) and reassembled a");
    eprintln!("[tap]      fragmented ping, fragmenting the reply outbound (B5-5), over TAP");
    Ok(())
}

/// One `ping -c1` to `ip`, optionally with a payload size (`-s`). Returns whether
/// a reply came back. A large `-s` forces the request to fragment at the MTU.
fn ping_once(ip: &str, size: Option<usize>) -> bool {
    let mut cmd = Command::new("ping");
    cmd.args(["-c", "1", "-W", "1"]);
    if let Some(s) = size {
        cmd.arg("-s").arg(s.to_string());
    }
    cmd.arg(ip)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// QEMU smoke tests (B0 Step 4)
//
// The kernel boots, the Kernel HSM drives its init chain to $Running,
// then kmain parks the CPU in a `hlt` loop — there's no `isa-debug-exit`
// plumbing yet because adding that would require feature-gating the
// kernel, and it isn't needed to validate B0's "boots, runs the boot
// HSM, and rests" contract. Each smoke test launches QEMU, waits a fixed
// timeout (plenty: boot → HSM chain → halt finishes in well under a
// second), then SIGKILLs QEMU and reads the serial capture file.
// Assertions are substring matches against the captured output.
//
// When B-track milestones land that need a clean pass/fail signal from
// inside the kernel (e.g. behavioral tests of the boot HSM running in
// QEMU), we'll add an `isa-debug-exit` path gated behind a `smoke-test`
// Cargo feature on the kernel crate, and replace the timeout-and-kill
// here with "wait for QEMU to exit; assert exit code". Deferring that
// keeps Step 4 minimal: no kernel changes, just an xtask subcommand
// that proves the smoke test infrastructure works.
// ---------------------------------------------------------------------------

/// A single QEMU smoke test. Each test runs the same kernel image (we
/// have only one bare-metal binary) and checks that the captured serial
/// output contains specific substrings. Substrings are checked in the
/// order listed — a missing earlier substring stops the test there with
/// a clear message.
struct SmokeTest {
    name: &'static str,
    /// Substrings that MUST appear in the captured serial output.
    /// Checked in order.
    expect_contains: &'static [&'static str],
    /// Substrings that MUST NOT appear. Useful for catching panics or
    /// triple-faults that wouldn't fail an `expect_contains` check on
    /// their own.
    expect_absent: &'static [&'static str],
    /// Wall-clock seconds to let QEMU run before killing it and reading
    /// the serial capture. This budget is dominated by the OVMF UEFI
    /// firmware and Limine cold-start (several seconds), not the kernel's
    /// own runtime (which prints in well under a second once it runs).
    /// Keep it generous so a loaded machine or slow CI runner doesn't
    /// false-fail before the bootloader even reaches the kernel.
    timeout_secs: u64,
}

const SMOKE_TESTS: &[SmokeTest] = &[
    SmokeTest {
        name: "boot_prints_banner_b0",
        // The em dash is `\u{2014}` in the kernel source (bytes
        // 0xe2 0x80 0x94 in the UTF-8 serial stream); literal here.
        expect_contains: &["Frame OS kernel \u{2014} B0 Step 2", "entering boot HSM..."],
        expect_absent: &["KERNEL PANIC", "triple fault"],
        // OVMF + Limine cold-start can take several seconds before the
        // kernel runs; 20s is comfortable headroom (the kernel itself
        // prints in <1s once reached).
        timeout_secs: 20,
    },
    SmokeTest {
        // The Step 2 payload: the Kernel HSM drives the boot chain.
        // Each `[boot] <phase>` line is one init child's `$>` enter
        // handler firing, in transition order; `[run] kernel running`
        // is `$Running`'s enter handler. Asserting them in order
        // proves the whole HSM chain executed on bare metal, not just
        // that the kernel booted.
        name: "boot_hsm_runs_init_chain_b0",
        expect_contains: &[
            "[boot] init memory",
            "[boot] init IDT",
            "[boot] init timer",
            "[boot] init console",
            "[boot] launching init",
            "[run] kernel running",
        ],
        expect_absent: &["KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B1 Step 3a/3b: the IDT and the timer IRQ. `[int3 ok]` +
        // `survived int3` prove the IDT, gate descriptors, and iretq work;
        // `20 ticks elapsed` proves the PIC remap + PIT + timer ISR fire
        // IRQ0 (the hlt-wait loop only exits if ticks accumulate). No
        // exception safety-net line should appear.
        name: "interrupts_and_timer_b1",
        expect_contains: &[
            "[int3 ok]",
            "[idt] survived int3",
            "[timer] 20 ticks elapsed",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B1 Step 2: the native cooperative context switch. Two kernel
        // threads ping-pong on independent stacks and hand control back to
        // main. "ABABAB" proves repeated alternation between the two
        // stacks; the bookend lines prove the round-trip (main → A/B → main)
        // completed without a fault.
        name: "context_switch_ping_pong_b1",
        expect_contains: &[
            "[switch] starting A/B ping-pong",
            "ABABAB",
            "[switch] back in main, demo done",
        ],
        expect_absent: &["KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B1 Step 3c: preemptive multitasking. Two threads busy-loop and
        // print '1'/'2' WITHOUT ever yielding — the only thing that can let
        // both make progress is the timer ISR preempting them. "1212"
        // (repeated alternation) is the proof; a single thread monopolizing
        // the CPU (no preemption) would print only one digit.
        name: "preemption_b1",
        expect_contains: &[
            "[preempt] starting two non-yielding threads",
            "12", // a 1→2 adjacency: the timer preempted worker1 into worker2
            "scheduler is $Idle",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B2 Step 1: the physical frame allocator over Limine's memory map.
        // Two distinct page-aligned frames, free restoring the count, and a
        // successful realloc-after-free exercise alloc/free/bitmap.
        name: "frame_allocator_b2",
        expect_contains: &[
            "[frames] alloc two distinct frames: ok",
            "[frames] free restores count: ok",
            "[frames] realloc after free: ok",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B2 Step 2: 4-level paging. Map a fresh frame at an unmapped VA,
        // write through the mapping and confirm it lands in the right
        // physical frame (cross-checked via the HHDM), translate, unmap.
        name: "paging_b2",
        expect_contains: &[
            "[paging] map + write + read-back: ok",
            "[paging] translate matches frame: ok",
            "[paging] unmap clears mapping: ok",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B2 Step 3 (demand paging): touching a registered lazy region
        // faults (#PF), the PageFaultHandler HSM classifies it $LazyFault,
        // maps a frame, and the access retries successfully — all from
        // inside the exception handler. The #PF goes to isr_page_fault, so
        // the isr_exception safety net (KERNEL EXCEPTION) must NOT fire.
        name: "page_fault_demand_b2",
        expect_contains: &["[#PF] demand fault recovered: ok"],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B2 Step 3 (fatal): an unmapped, non-lazy access is classified
        // $Fatal — reported and halted cleanly, NOT a silent triple-fault
        // and NOT the generic exception safety net.
        name: "page_fault_fatal_b2",
        expect_contains: &[
            "[#PF] triggering a deliberate fatal fault",
            "[#PF] FATAL unhandled kernel fault at 0x0000600000000000",
            "[#PF] halting.",
        ],
        expect_absent: &["KERNEL EXCEPTION", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B2 Step 4: per-process address spaces. Build a fresh PML4 (kernel
        // higher-half mirrored), map a page only in it, switch CR3 (the
        // kernel survives the switch — proving the higher-half copy), read
        // the page back, switch back, and confirm the mapping was isolated
        // to the new space.
        name: "address_space_switch_b2",
        expect_contains: &[
            "[vm] address-space switch sees its mapping: ok",
            "[vm] mapping isolated to its address space: ok",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B3 Step 1a: install our own GDT + TSS (syscall/sysret selector
        // layout + a kernel stack for ring-3 interrupts). Reaching the line
        // proves the lgdt + CS/segment reload + ltr didn't fault.
        name: "gdt_loaded_b3",
        expect_contains: &["[gdt] loaded our GDT + TSS"],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B3 Step 1b + 3 + 4a + 5a: a real ELF run as a preemptible, scheduled
        // Process. ElfLoader parses + maps the baked `hello` program into its
        // own address space; it's spawned ($Ready), scheduled (entered ring 3
        // via the scheduler's iretq, preemptible), prints "hello from ELF" via
        // write_char syscalls on its own kernel stack, exit(42)s (→ $Zombie +
        // yields to the scheduler), then is reaped ($Reaped, slot freed).
        name: "ring3_syscall_b3",
        expect_contains: &[
            "[elf] loaded hello, entry 0x",
            "[proc] spawned pid 1 (Ready)",
            "[sched] user process scheduled",
            "hello from ELF",
            "[user] exited with code 42",
            "[proc] pid 1 exited -> Zombie",
            "[sched] user process left the CPU",
            "[proc] reaped pid 1; exit 42; table count 0",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B3 Step 4b: hardware isolation. A second user program (`faulter`,
        // pid 2) reads a kernel-half address from ring 3 → #PF with the U/S
        // bit set. The PageFaultHandler routes it (via $FaultActive's
        // load-bearing `=> $^` funnel) to $Killing: the process is torn down
        // (killed → $Zombie, reaped with exit -1) and the kernel SURVIVES,
        // going on to the deliberate kernel fatal demo. A user fault must not
        // halt or crash the kernel.
        name: "user_fault_does_not_crash_kernel_b3",
        expect_contains: &[
            "[elf] loaded faulter",
            "[proc] spawned pid 2 (Ready)",
            "[#PF] user fault at 0xffffffff80000000 -> killing process (kernel survives)",
            "[proc] pid 2 killed -> Zombie",
            "[proc] reaped pid 2; exit -1; table count 0",
            // Survival proof: the kernel keeps running past the user fault and
            // reaches the (separate, deliberate) kernel-fault demo.
            "[#PF] triggering a deliberate fatal fault",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B3 Step 5b: fork → two concurrent user processes. The `forker`
        // program (pid 3) forks (eager address-space copy + trap-frame copy);
        // the child (pid 4) returns 0, the parent returns the child pid. Both
        // run concurrently in separate address spaces — interleaved on the
        // serial console by the preemptive scheduler — and both exit. (The
        // child lingers as a zombie until wait()/reap at Step 5d, so the
        // parent's reap leaves table count 1.)
        name: "fork_concurrency_b3",
        expect_contains: &[
            "[fork] pid 3 forked child pid 4",
            // Order-stable proof that BOTH ran to exit: the parent's reap only
            // happens once the scheduler is idle (both parent + child exited),
            // and `table count 1` means the child (pid 4) exited and lingers as
            // an unreaped zombie. (The two processes' own exit lines race, since
            // they run concurrently — so we don't assert their relative order.)
            "[proc] reaped pid 3; exit 0; table count 1",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B3 Step 5c: fork + exec — the canonical shell-launch pattern. The
        // `spawner` program (pid 5) forks; the child (pid 6) execs program 0
        // (`hello`), *becoming* hello — its image is replaced and it runs
        // hello's code, printing "hello from ELF". The parent prints 'S'. The
        // "hello from ELF" matched here (after the exec marker) is the exec'd
        // child's, not the original pid-1 hello (which ran far earlier).
        name: "exec_b3",
        expect_contains: &[
            "[fork] pid 5 forked child pid 6",
            "[exec] pid 6 exec'd program 0",
            "hello from ELF",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B3 Step 5d: wait + reap. The `waiter` (pid 7) forks a child (pid 8),
        // then BLOCKS in wait() — the one place a syscall suspends. The child
        // runs concurrently (prints 'cccc'), exits(7), and its exit (SIGCHLD)
        // wakes the parent, which reaps it: collects the status (7), frees the
        // Process slot, and tears down the child's address space. Unlike the
        // forker/spawner children (which linger as zombies), pid 8 is fully
        // reaped — proving the blocking wait + teardown path.
        name: "wait_reap_b3",
        expect_contains: &[
            "[fork] pid 7 forked child pid 8",
            "[wait] pid 7 reaped child pid 8 (exit 7)",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B4 Step 1: the virtio-blk driver + the post/drain deferred-event
        // pattern. The kernel inits the device, writes a known pattern to a
        // sector and reads it back — the completion IRQ `post`s, the kernel
        // `drain`s it and drives a `BlockRequest` to $Complete, and the data
        // verifies. The first async-interrupt → Frame boundary.
        name: "blk_roundtrip_b4",
        expect_contains: &[
            "[blk] virtio-blk ready",
            "[blk] sector write/read round-trip: ok",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B4 Step 2: the on-disk filesystem over the buffer cache + the Mount
        // HSM. The disk is mkfs'd by the host (xtask build_fs_image) with a
        // `motd` file; the kernel mounts it, reads `motd` back (proving the
        // on-disk format round-trips host-write → kernel-read), then runs a
        // create → write → read → delete cycle (proving the write + bitmap +
        // inode paths).
        name: "fs_file_roundtrip_b4",
        expect_contains: &[
            "[fs] mounted",
            "[fs] /motd: Frame OS B4 filesystem online.",
            "[fs] create/write/read round-trip: ok",
            "[fs] delete: ok",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B4 Step 2: the FS persists across a reboot of the same disk image.
        // The harness boots TWICE on one disk: boot 1 creates a `persist`
        // marker file (kernel write), boot 2 reads it back. We check boot 2's
        // capture — "persistence verified across reboot" only prints when a
        // prior boot's write survived. (See `run_smoke_test`'s reboot path.)
        name: "fs_persists_across_reboot_b4",
        expect_contains: &["[fs] mounted", "[fs] persistence verified across reboot"],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "marker CORRUPT",
        ],
        timeout_secs: 20,
    },
    SmokeTest {
        // B4 Step 3: VFS + OpenFile + path lookup. The kernel opens files by
        // absolute path through the fd table (each fd an OpenFile in $Reading),
        // reads `/motd` and the nested `/bin/info` (proving directory walking),
        // and confirms a closed fd reads nothing.
        name: "vfs_path_lookup_b4",
        expect_contains: &[
            "[vfs] read /motd via fd: Frame OS B4 filesystem online.",
            "[vfs] read /bin/info via fd: nested directory works",
            "[vfs] read after close returns 0: ok",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B4 Step 4: the userspace shell. A ring-3 program uses the file-I/O
        // syscalls to `cat /motd`, then `exec`s `/bin/hello` *from disk by
        // path* — the on-disk ELF replaces the shell's image and runs to its
        // own exit(42). Asserting the shell's cat output, the exec line, and
        // hello's output + exit code (in order) proves open/read/close and
        // exec-from-disk all work end to end.
        name: "userspace_shell_runs_program_from_disk_b4",
        expect_contains: &[
            "[shell] cat /motd:",
            "Frame OS B4 filesystem online.",
            "[shell] exec /bin/hello:",
            "[exec] pid",
            "from disk",
            "hello from ELF",
            "[user] exited with code 42",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[shell] open /motd failed",
            "[shell] exec /bin/hello failed",
        ],
        timeout_secs: 20,
    },
    SmokeTest {
        // B4 Step 4b: the Frame-driven shell. A ring-3 program tokenizes its
        // command lines with the *same* `parser.frs` the hosted shell compiles
        // — the "one source, two targets" demonstration. It cats a *quoted*
        // path (`cat "/motd"`), which only resolves if the Parser's
        // $InQuotedString state ran in ring 3 to strip the quotes, then execs
        // `/bin/hello` by the parsed token. Asserting the motd contents (via
        // the quoted cat) + hello's output + exit code proves the reused Frame
        // tokenizer works end to end in userspace.
        name: "userspace_frame_parser_reuse_b4",
        expect_contains: &[
            "[frameshell] tokenizing with parser.frs",
            "[frameshell] $ cat \"/motd\"",
            "Frame OS B4 filesystem online.",
            "[frameshell] $ /bin/hello",
            "hello from ELF",
            "[user] exited with code 42",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[frameshell] open failed",
            "[frameshell] exec failed",
        ],
        timeout_secs: 20,
    },
    SmokeTest {
        // B5 Step 2a: ARP resolution through the `ArpResolver` Frame system. The
        // kernel inits the NIC, then resolves the QEMU slirp gateway (10.0.2.2)
        // via ArpResolver — the first networking Frame system: `$Incomplete`'s
        // enter handler sends the request + arms a retransmit timer (the native
        // timer pattern), the receive loop fires `reply()` on the matching ARP
        // reply. Proves NIC TX/RX + post/drain + the Frame-driven resolution
        // lifecycle. slirp answers ARP for the gateway deterministically.
        name: "arp_resolves_gateway_b5",
        expect_contains: &[
            "[net] virtio-net ready",
            "[net] MAC ",
            "[arp] who-has 10.0.2.2 (gateway)",
            "[arp] resolved 10.0.2.2 -> ",
            "[net] gateway resolved via ArpResolver: ok",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[net] virtio-net NOT found",
            "[arp] resolution failed",
            "[net] gateway resolution did not complete",
        ],
        timeout_secs: 20,
    },
    SmokeTest {
        // B5 Step 2b: IPv4 + ICMP echo. After resolving the gateway MAC, the
        // kernel sends an ICMP echo request to 10.0.2.2 (Ethernet → IPv4 →
        // ICMP, both checksums) and matches the reply — proving the IPv4/ICMP
        // encode + parse path. slirp answers ping to its gateway address
        // deterministically. (Answering inbound pings — the responder, B5-3 —
        // lands with TAP, where inbound ICMP can reach the guest.)
        name: "kernel_pings_gateway_b5",
        expect_contains: &[
            "[arp] resolved 10.0.2.2 -> ",
            "[icmp] ping 10.0.2.2 seq 0",
            "[icmp] reply from 10.0.2.2 seq 0",
            "[net] ping ok",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[icmp] no reply (timeout)",
        ],
        timeout_secs: 20,
    },
    SmokeTest {
        // B5 Step 3b: UDP + UdpSocket. The kernel binds a UDP socket on :68 and
        // sends a DHCP DISCOVER; slirp's DHCP server answers with an OFFER,
        // which the RxPipeline classifies (IPv4 → UDP) and delivers to the bound
        // socket (on_udp → UdpSocket.recv()). Proves UDP encode/parse + the
        // bind lifecycle + the pipeline's $Udp leaf on a real inbound datagram.
        // slirp always runs its DHCP server, so the OFFER is deterministic.
        name: "dhcp_offer_b5",
        expect_contains: &[
            "[udp] socket bound on :68",
            "[dhcp] DISCOVER",
            "[dhcp] OFFER: 10.0.2.",
            "[udp] datagram delivered to socket :68 (count 1)",
            "[net] DHCP offer via UdpSocket: ok",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[dhcp] no OFFER (timeout)",
        ],
        timeout_secs: 20,
    },
    SmokeTest {
        // B5 Step 4b: the live passive TCP handshake. The kernel passive-opens
        // a TcpConnection on :7 and serves; the harness connects through slirp
        // hostfwd (driving the 3-way handshake), so the FSM reaches
        // $Established against the host's real TCP stack. "[tcp] established" is
        // the oracle — it's logged only when the guest completes the handshake.
        name: "tcp_handshake_b5",
        expect_contains: &["[tcp] listening on :7", "[tcp] established"],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
        ],
        // The serve loop waits a few seconds for the client; give it headroom.
        timeout_secs: 30,
    },
    SmokeTest {
        // B5 Step 4c/4d (B5-4): the full TCP exchange against a real client —
        // handshake, request/response, clean close. After the handshake the
        // harness sends a request; $Established echoes it back (verified by the
        // harness reading it, gated in run_qemu_once); then the kernel actively
        // closes, driving $FinWait1 → $TimeWait → (2·MSL timer) → $Closed.
        name: "tcp_echo_b5",
        expect_contains: &[
            "[tcp] established",
            "[tcp] echoed 18 bytes",
            "[tcp] closed",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // B5 Step 4e: active open (the kernel as TCP client). The kernel
        // connects out to 10.0.2.100:9, which slirp guestfwd forwards to the
        // harness's host listener; the FSM goes $SynSent → $Established. The
        // harness just listens (accepting completes the connection); the serial
        // oracle confirms the active open.
        name: "tcp_active_open_b5",
        expect_contains: &[
            "[tcp] connecting to 10.0.2.100:9 (active open)",
            "[tcp] connected (active open)",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // B6 Step 1: xHCI USB host-controller bring-up. The kernel discovers the
        // qemu-xhci controller (PCI class 0C0330), maps its MMIO window, resets
        // it, stands up the DCBAA/command-ring/event-ring, sets Run, and detects
        // the attached usb-kbd connected on a port.
        name: "usb_controller_b6",
        expect_contains: &[
            "[usb] xHCI running",
            "[usb] device connected on port",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // B6 Step 2: the HubPort Frame system drives the connected port through
        // connect → reset (a timed transition: PORTSC.PR + a settle deadline) →
        // enabled. The keyboard lands on port 5 in this qemu-xhci/q35 config.
        name: "usb_port_reset_b6",
        expect_contains: &[
            "[usb] resetting port 5",
            "[usb] port 5 enabled",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // B6 Step 3: the UsbEnumeration Frame system enumerates the device to
        // $Configured — Enable Slot + Address Device (command ring), then
        // GET_DESCRIPTOR + SET_CONFIGURATION (EP0 control transfers). Each
        // command/transfer completion drives an FSM milestone event.
        name: "usb_enumerates_b6",
        expect_contains: &[
            "[usb] slot 1 enabled",
            "[usb] device addressed (slot 1)",
            "[usb] device descriptor: idVendor",
            "[usb] device configured (slot 1)",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[usb] command failed during enumeration",
            "[usb] control transfer failed during enumeration",
        ],
        timeout_secs: 30,
    },
    SmokeTest {
        // B6 Step 4 (closes B6-3): the UsbTransfer Frame system completes a real
        // interrupt-IN transfer. The kernel configures the keyboard's interrupt
        // endpoint (Configure Endpoint + EP1 ring), queues an interrupt-IN read,
        // and logs "waiting for key report"; the harness then injects a keypress
        // via the QEMU monitor (`sendkey a`), so the keyboard produces a HID
        // report and the transfer completes ($InFlight → $Complete).
        name: "usb_transfer_b6",
        expect_contains: &[
            "[usb] device configured (slot 1)",
            "[usb] interrupt endpoint configured (EP1 IN)",
            "[usb] waiting for key report",
            "[usb] HID report:",
            "[usb] key transfer complete",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[usb] configure endpoint failed",
            "[usb] key report not received (no transfer)",
        ],
        timeout_secs: 30,
    },
    SmokeTest {
        // B7 Step 1: SMP application-processor bringup. Limine starts the APs;
        // the kernel launches each at ap_entry (Limine MP request), where it sets
        // up its per-CPU GS-base block and reports online; the BSP waits and logs
        // the count. With `-smp 4`, all 4 cores must come online. (Per-CPU
        // scheduling across cores lands in later B7 steps; here the APs park.)
        name: "smp_cores_online_b7",
        expect_contains: &["[smp] cores online: 4 of 4"],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[smp] no MP response (single core)",
        ],
        timeout_secs: 30,
    },
    SmokeTest {
        // B7 Step 2: cross-core locking. All 4 cores hammer a shared counter
        // 50000 times each through the IRQ-safe SpinLock, concurrently; the exact
        // total (200000, no lost updates) proves the lock serialized every
        // increment under true parallelism.
        name: "smp_concurrent_b7",
        expect_contains: &[
            "[smp] cores online: 4 of 4",
            "[smp] shared counter: 200000 (expected 200000)",
            "[smp] cross-core lock: ok (no lost updates)",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[smp] cross-core lock: FAILED",
        ],
        timeout_secs: 30,
    },
    SmokeTest {
        // B7 cross-core post: a Frame system (EventCounter) driven from other
        // cores. The 3 APs each post 200 tick events into a SpinLock MPSC queue;
        // the BSP owns the EventCounter instance (a local — never shared) and
        // drains the queue into it. The exact count (4 cores × 200 = 800) proves
        // every cross-core-posted event was dispatched once; the post-close tick
        // being dropped proves the FSM still gates posts by state. Confirms the
        // post/drain architecture gives cross-core safety with no framec
        // Send/Sync change (the instance never leaves the BSP).
        name: "smp_cross_core_post_b7",
        expect_contains: &[
            "[smp] cross-core post: counter 800 (expected 800)",
            "[smp] cross-core post -> Frame dispatch: ok",
            "[smp] post-close tick ignored ($Closed gates it): ok",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[smp] cross-core post: FAILED",
        ],
        timeout_secs: 30,
    },
    SmokeTest {
        // B7 Step 4: per-CPU preemptive execution. Each AP loads the IDT, starts
        // its own LAPIC timer, enables interrupts, and runs a busy loop that the
        // timer preempts — proving each core runs a real, time-sliced thread (not
        // just a one-shot). The BSP reports each core's tick + work counts; "ok"
        // means every AP was preempted TARGET_TICKS times.
        name: "smp_preempt_b7",
        expect_contains: &[
            "[smp] core 1: ",
            "[smp] per-core preemption: ok (each AP timer-sliced)",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
];

fn run_qemu_test() -> Result<()> {
    let workspace = workspace_root()?;
    let artifacts = prepare_qemu_artifacts(&workspace)?;

    let serial_dir = target_dir(&workspace).join("qemu-smoke");
    std::fs::create_dir_all(&serial_dir)
        .with_context(|| format!("failed to create {}", serial_dir.display()))?;

    // Optional substring filter for iterating on one test:
    // `FRAMEOS_SMOKE_FILTER=tcp_handshake cargo xtask qemu-test`.
    let filter = std::env::var("FRAMEOS_SMOKE_FILTER").unwrap_or_default();
    let tests: Vec<&SmokeTest> = SMOKE_TESTS
        .iter()
        .filter(|t| filter.is_empty() || t.name.contains(&filter))
        .collect();

    let mut failures: Vec<String> = Vec::new();
    let total = tests.len();

    for test in tests {
        eprintln!("smoke: {} ...", test.name);
        let serial_path = serial_dir.join(format!("{}.log", test.name));
        // Truncate any previous run's log; QEMU appends with `-serial
        // file:`, which we want fresh per run.
        if serial_path.exists() {
            std::fs::remove_file(&serial_path).with_context(|| {
                format!("failed to clear previous log {}", serial_path.display())
            })?;
        }

        // Reboot-persistence tests boot twice on one disk (B4-4).
        let reboot = test.name.contains("persists_across_reboot");
        match run_smoke_test(test, &artifacts, &serial_path, reboot) {
            Ok(()) => eprintln!("  PASS"),
            Err(err) => {
                eprintln!("  FAIL: {err}");
                failures.push(format!("{}: {err}", test.name));
            }
        }
    }

    if failures.is_empty() {
        eprintln!("\nqemu-test: {total}/{total} passed");
        Ok(())
    } else {
        let passed = total - failures.len();
        eprintln!("\nqemu-test: {passed}/{total} passed");
        for line in &failures {
            eprintln!("  - {line}");
        }
        bail!("{} smoke test(s) failed", failures.len());
    }
}

/// Boot the kernel once in QEMU on a given disk + NVRAM, routing serial to
/// `serial_path`. Returns when QEMU exits (clean isa-debug-exit) or the
/// timeout forces a kill. `-display none` is set in `qemu_base_command`.
/// Host port slirp forwards to the guest's TCP listener (:7), for the B5 Step
/// 4b/4c TCP probes.
const TCP_PROBE_PORT: u16 = 15580;
/// Host port the guestfwd target maps to — the harness listens here for the
/// kernel's active-open connection (B5 Step 4e).
const TCP_ACTIVE_PORT: u16 = 15581;
/// The request the echo probe sends; the kernel echoes it back verbatim.
const TCP_ECHO_REQUEST: &[u8] = b"frame-os tcp echo\n";
/// Host port for the QEMU HMP monitor (the B6 usb-transfer test connects here to
/// `sendkey` a keypress, completing the keyboard's interrupt-IN transfer).
const MONITOR_PORT: u16 = 15582;

/// What (if anything) the harness drives over the forwarded TCP port.
#[derive(Clone, Copy, PartialEq)]
enum TcpProbe {
    None,
    /// 4b: connect (drive the 3-way handshake); the serial oracle confirms it.
    Handshake,
    /// 4c/4d: connect, send a request, verify the echoed reply (validates the
    /// outbound data path's seq + checksum — the host TCP would drop a bad one).
    Echo,
    /// 4e: listen on the guestfwd target so the kernel's active open succeeds
    /// (the serial oracle "[tcp] connected (active open)" confirms `$SynSent` →
    /// `$Established`).
    Active,
}

fn run_qemu_once(
    artifacts: &QemuArtifacts,
    ovmf_vars: &Path,
    blk_disk: &Path,
    serial_path: &Path,
    timeout_secs: u64,
    tcp_probe: TcpProbe,
    usb_key: bool,
) -> Result<()> {
    // 4e: for the active-open test, the guestfwd target must be listening when
    // QEMU starts (QEMU connects to it at startup), so bind it before spawning.
    let active_listener = if tcp_probe == TcpProbe::Active {
        let l = std::net::TcpListener::bind(("127.0.0.1", TCP_ACTIVE_PORT))
            .context("failed to bind the active-open listener")?;
        l.set_nonblocking(true).ok();
        Some(l)
    } else {
        None
    };
    let mut active_streams: Vec<std::net::TcpStream> = Vec::new();

    let mut cmd = qemu_base_command(
        artifacts,
        ovmf_vars,
        blk_disk,
        &NetMode::Slirp {
            guestfwd: tcp_probe == TcpProbe::Active,
        },
    );
    cmd.args(["-serial"])
        .arg(format!("file:{}", serial_path.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    // B6 Step 4: a TCP HMP monitor so the harness can `sendkey` a keypress,
    // completing the keyboard's interrupt-IN transfer.
    if usb_key {
        cmd.args(["-monitor"])
            .arg(format!("tcp:127.0.0.1:{MONITOR_PORT},server,nowait"));
    }

    let mut child = cmd.spawn().context("failed to invoke qemu-system-x86_64")?;

    // B5 Step 4b/4c: drive the forwarded TCP port (retrying while the kernel
    // boots to its serve loop). Handshake: a successful connect means the guest
    // reached $Established (serial oracle "[tcp] established"). Echo: also send
    // a request and verify the echoed reply (a bad outbound seq/checksum would
    // make the host TCP drop it, so reading it back validates the data path).
    let mut probed = false;
    let mut echo_ok = false;
    let mut key_sent = false;

    // Poll for QEMU's natural exit, otherwise force-kill at timeout.
    // The kernel's `halt_forever()` writes `isa-debug-exit` (port 0xf4)
    // before parking, so a healthy boot exits QEMU promptly (exit code 33)
    // — no timeout race. We deliberately do NOT treat a non-zero exit as a
    // failure here: the isa-debug-exit code is non-zero by construction,
    // and a triple-fault that reboots into firmware would also exit
    // non-zero. Either way the verdict comes from the captured serial
    // output — a clean halt produces the full marker sequence; a crash
    // produces a truncated capture that fails the substring check.
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait().context("failed to poll qemu process")? {
            Some(_status) => break,
            None => {
                // 4e: accept the kernel's active-open connection (nonblocking).
                if let Some(ref l) = active_listener {
                    if let Ok((stream, _)) = l.accept() {
                        active_streams.push(stream); // keep alive
                    }
                }
                // Connect only once the kernel is actually serving (it logs
                // "[tcp] listening" when it enters the serve loop). Connecting
                // earlier piles up dead slirp connections that the guest would
                // handshake instead of the live one. Waiting → exactly one
                // connection, during the serve window.
                let serving = std::fs::read_to_string(serial_path)
                    .map(|s| s.contains("[tcp] listening on :7"))
                    .unwrap_or(false);
                if matches!(tcp_probe, TcpProbe::Handshake | TcpProbe::Echo)
                    && !probed
                    && serving
                {
                    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], TCP_PROBE_PORT));
                    if let Ok(mut stream) =
                        std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500))
                    {
                        match tcp_probe {
                            TcpProbe::Handshake => probed = true, // handshake done via slirp
                            TcpProbe::Echo => {
                                use std::io::{Read, Write};
                                stream
                                    .set_read_timeout(Some(Duration::from_millis(2500)))
                                    .ok();
                                let mut buf = vec![0u8; TCP_ECHO_REQUEST.len()];
                                if stream.write_all(TCP_ECHO_REQUEST).is_ok()
                                    && stream.read_exact(&mut buf).is_ok()
                                    && buf == TCP_ECHO_REQUEST
                                {
                                    echo_ok = true;
                                    probed = true;
                                } else {
                                    eprintln!("    (echo attempt: read {buf:?})");
                                }
                            }
                            TcpProbe::None | TcpProbe::Active => {}
                        }
                    }
                }
                // B6 Step 4: once the kernel has queued the interrupt-IN read
                // (it logs "[usb] waiting for key report"), inject a keypress via
                // the QEMU monitor so the keyboard produces a report and the
                // transfer completes.
                if usb_key && !key_sent {
                    let waiting = std::fs::read_to_string(serial_path)
                        .map(|s| s.contains("[usb] waiting for key report"))
                        .unwrap_or(false);
                    if waiting && send_monitor_command(MONITOR_PORT, "sendkey a\n") {
                        eprintln!("    (sent `sendkey a` to QEMU monitor)");
                        key_sent = true;
                    }
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    // The echo probe gates the test: the harness must have read its request
    // back (proving the kernel's outbound data segment was well-formed).
    if tcp_probe == TcpProbe::Echo && !echo_ok {
        bail!("TCP echo not verified: the harness never read the echoed request back");
    }
    Ok(())
}

/// Send one HMP command to QEMU's TCP monitor (`sendkey`). Best-effort: connect,
/// write the command line, give QEMU a moment to process it before the socket
/// closes, return whether the write succeeded.
fn send_monitor_command(port: u16, command: &str) -> bool {
    use std::io::Write;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        Ok(mut stream) => {
            let ok = stream.write_all(command.as_bytes()).is_ok() && stream.flush().is_ok();
            // Hold the connection briefly so QEMU's HMP reader processes the line
            // before we drop the socket.
            std::thread::sleep(Duration::from_millis(100));
            ok
        }
        Err(_) => false,
    }
}

fn run_smoke_test(
    test: &SmokeTest,
    artifacts: &QemuArtifacts,
    serial_path: &Path,
    reboot: bool,
) -> Result<()> {
    // One fresh disk for the test. A `reboot` test boots TWICE on the *same*
    // disk (so the first boot's writes persist into the second) with fresh
    // NVRAM each boot; the second boot's capture is the one we check.
    let blk_disk = fresh_blk_disk(artifacts, test.name)?;
    // The TCP tests drive an external connection through the forwarded port.
    let tcp_probe = match test.name {
        "tcp_handshake_b5" => TcpProbe::Handshake,
        "tcp_echo_b5" => TcpProbe::Echo,
        "tcp_active_open_b5" => TcpProbe::Active,
        _ => TcpProbe::None,
    };
    // The B6 transfer test injects a keypress via the QEMU monitor.
    let usb_key = test.name == "usb_transfer_b6";
    if reboot {
        let ovmf1 = fresh_ovmf_vars(artifacts, &format!("{}-boot1", test.name))?;
        let boot1_log = serial_path.with_extension("boot1.log");
        let _ = std::fs::remove_file(&boot1_log);
        run_qemu_once(
            artifacts,
            &ovmf1,
            &blk_disk,
            &boot1_log,
            test.timeout_secs,
            TcpProbe::None,
            false,
        )?;
    }
    let ovmf_vars = fresh_ovmf_vars(artifacts, test.name)?;
    run_qemu_once(
        artifacts,
        &ovmf_vars,
        &blk_disk,
        serial_path,
        test.timeout_secs,
        tcp_probe,
        usb_key,
    )?;

    // Read the captured serial output.
    let mut captured = String::new();
    std::fs::File::open(serial_path)
        .with_context(|| format!("failed to open serial capture {}", serial_path.display()))?
        .read_to_string(&mut captured)
        .with_context(|| format!("failed to read serial capture {}", serial_path.display()))?;

    // Required substrings, in order.
    let mut search_from = 0usize;
    for needle in test.expect_contains {
        match captured[search_from..].find(needle) {
            Some(rel) => search_from += rel + needle.len(),
            None => {
                return Err(anyhow!(
                    "missing expected substring {:?} in serial output. \
                     Full capture follows:\n---\n{captured}\n---",
                    needle
                ));
            }
        }
    }

    // Forbidden substrings, anywhere.
    for needle in test.expect_absent {
        if captured.contains(needle) {
            return Err(anyhow!(
                "forbidden substring {:?} appeared in serial output. \
                 Full capture follows:\n---\n{captured}\n---",
                needle
            ));
        }
    }

    Ok(())
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
    let elf = target_dir(workspace)
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
    let limine_dir = target_dir(workspace).join("limine");
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
    let img = target_dir(workspace).join("limine-esp.img");

    // Write limine.conf to a temp file we can `mcopy` later.
    let conf_tmp = target_dir(workspace).join("limine.conf");
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
///
/// Distros and packagers disagree on filenames:
///   - QEMU Homebrew (macOS) ships `edk2-x86_64-code.fd` + `edk2-i386-vars.fd`
///     under `<prefix>/share/qemu/`
///   - Ubuntu's `ovmf` package ships `OVMF_CODE.fd` + `OVMF_VARS.fd` under
///     `/usr/share/OVMF/`
///   - Fedora / Arch use similar `OVMF_CODE.fd` naming with their own paths
///
/// We scan a small candidate matrix of (dir, code-name, vars-name) tuples
/// and return the first hit. Adding a new distro = adding a row.
fn find_ovmf() -> Result<(PathBuf, PathBuf)> {
    // (search_dir, code_filename, vars_filename)
    const CANDIDATES: &[(&str, &str, &str)] = &[
        // QEMU brew (Intel macOS) and Linux QEMU bundled firmware
        (
            "/usr/local/share/qemu",
            "edk2-x86_64-code.fd",
            "edk2-i386-vars.fd",
        ),
        (
            "/opt/homebrew/share/qemu",
            "edk2-x86_64-code.fd",
            "edk2-i386-vars.fd",
        ),
        (
            "/usr/share/qemu",
            "edk2-x86_64-code.fd",
            "edk2-i386-vars.fd",
        ),
        // Ubuntu / Debian `ovmf` package
        ("/usr/share/OVMF", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        // Fedora / Arch `edk2-ovmf` package
        ("/usr/share/edk2/ovmf", "OVMF_CODE.fd", "OVMF_VARS.fd"),
    ];

    for (dir, code_name, vars_name) in CANDIDATES {
        let code = PathBuf::from(dir).join(code_name);
        let vars = PathBuf::from(dir).join(vars_name);
        if code.exists() && vars.exists() {
            return Ok((code, vars));
        }
    }

    // Try to discover via brew (Homebrew installs into /usr/local/Cellar on
    // Intel and /opt/homebrew/Cellar on Apple Silicon). Brew always uses
    // QEMU's `edk2-x86_64-code.fd` + `edk2-i386-vars.fd` naming.
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
        "could not locate OVMF UEFI firmware. Tried QEMU's edk2-* layout and \
         the OVMF_*.fd layout in standard system paths. On macOS install via \
         `brew install qemu`; on Ubuntu/Debian install the `ovmf` package; on \
         Fedora/Arch install `edk2-ovmf`."
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

/// The directory cargo writes build artifacts into. Honors `CARGO_TARGET_DIR`
/// (the dev container sets it to `/target`, a named volume kept off the bind
/// mount), falling back to `<workspace>/target`. Every xtask-derived artifact
/// path (kernel ELF, ESP image, QEMU scratch dirs, Limine binaries) must route
/// through here: `build_kernel` runs `cargo build`, which obeys
/// `CARGO_TARGET_DIR`, so reading the ELF back from a hardcoded
/// `<workspace>/target` would silently pick up a stale cross-build instead.
fn target_dir(workspace: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => workspace.join("target"),
    }
}
