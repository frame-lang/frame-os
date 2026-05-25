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
    /// Serial is routed to stdio; Ctrl-A x quits QEMU. This is the DEMO
    /// kernel: it runs the B0–B7 self-test demos to the console, then halts.
    /// For a typeable shell, use `qemu-interactive`.
    Qemu,

    /// Boot the INTERACTIVE kernel in QEMU and drop you at the `frameos$`
    /// prompt (serial ↔ your terminal, no scripted input). Type programs
    /// yourself: `/bin/hello`, `/bin/tcc -v`, `buildc /hi.c`, `/bin/fhello`,
    /// `buildc /fhello.c`, then `exit`. Ctrl-A x quits QEMU. Needs a TTY — on
    /// macOS run it inside `docker/run.sh shell`.
    QemuInteractive,

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

    /// B8: boot the `interactive`-feature kernel and drive its serial console
    /// non-interactively — wait for the shell prompt, type `/bin/hello`, then
    /// `exit`, and assert the program ran. (`cargo xtask qemu-interactive` is
    /// the human-typeable version of the same kernel.)
    ConsoleTest,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        SubCmd::InstallTools => install_tools(),
        SubCmd::CheckDiagrams => diagrams(DiagramMode::Check),
        SubCmd::RegenDiagrams => diagrams(DiagramMode::Regen),
        SubCmd::Qemu => run_qemu(),
        SubCmd::QemuInteractive => run_qemu_interactive(),
        SubCmd::QemuTest => run_qemu_test(),
        SubCmd::QemuTap => run_qemu_tap(),
        SubCmd::ConsoleTest => run_console_test(),
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
    ("pipe.frs", "pipe.svg"),
    ("io_scheduler.frs", "io_scheduler.svg"),
    ("arp_resolver.frs", "arp_resolver.svg"),
    ("rx_pipeline.frs", "rx_pipeline.svg"),
    ("udp_socket.frs", "udp_socket.svg"),
    ("tcp_connection.frs", "tcp_connection.svg"),
    ("ip_reassembly.frs", "ip_reassembly.svg"),
    ("hub_port.frs", "hub_port.svg"),
    ("usb_enumeration.frs", "usb_enumeration.svg"),
    ("usb_transfer.frs", "usb_transfer.svg"),
    ("usb_msd.frs", "usb_msd.svg"),
    ("event_counter.frs", "event_counter.svg"),
    ("builddriver.frs", "builddriver.svg"),
    ("hello.frs", "hello.svg"),
];

fn diagrams(mode: DiagramMode) -> Result<()> {
    let workspace_root = workspace_root()?;
    let frame_dir = workspace_root.join("frame");
    let systems_dir = workspace_root.join("docs").join("systems");

    if !is_framec_installed() {
        bail!("framec is not installed. Run `cargo xtask install-tools` first.");
    }
    // `dot` is only needed to *render* SVGs (regen). The drift check compares the
    // GraphViz-independent DOT, so it needs framec only — no graphviz on CI.
    if matches!(mode, DiagramMode::Regen) && !is_dot_installed() {
        bail!(
            "GraphViz `dot` is not installed. Install via your package manager \
             (e.g. `brew install graphviz`, `apt install graphviz`)."
        );
    }

    let mut drift_count = 0;

    for (frs, svg) in DIAGRAMS {
        let frs_path = frame_dir.join(frs);
        let svg_path = systems_dir.join(svg);
        let dot_path = svg_path.with_extension("dot");
        // The DOT is the source of truth (deterministic from the .frs); it's what
        // we gate on. The SVG is a render committed only for docs display.
        let dot = generate_dot(&frs_path)?;

        match mode {
            DiagramMode::Regen => {
                std::fs::write(&dot_path, &dot)
                    .with_context(|| format!("failed to write {}", dot_path.display()))?;
                let svg = render_svg(&dot)?;
                std::fs::write(&svg_path, &svg)
                    .with_context(|| format!("failed to write {}", svg_path.display()))?;
                println!("wrote {} + {}", dot_path.display(), svg_path.display());
            }
            DiagramMode::Check => {
                let committed = std::fs::read(&dot_path).ok();
                if committed.as_deref() == Some(dot.as_slice()) {
                    println!("ok: {}", dot_path.display());
                } else {
                    drift_count += 1;
                    eprintln!(
                        "drift: {} differs from `framec {} -l graphviz`",
                        dot_path.display(),
                        frs_path.display()
                    );
                }
            }
        }
    }

    if matches!(mode, DiagramMode::Check) && drift_count > 0 {
        bail!(
            "{drift_count} diagram(s) out of date. Run `cargo xtask regen-diagrams` and commit \
             (the .dot is gated; the .svg is a render)."
        );
    }

    Ok(())
}

/// Generate the GraphViz **DOT** for an `.frs` (framec's `-l graphviz` output).
/// This is the *gated* artifact: it's a pure function of the `.frs` (and a stable
/// framec output — verified byte-identical across framec 4.2.1↔4.2.3), so it
/// captures the FSM's structure without depending on the GraphViz version. (The
/// drift check compares DOT, not the rendered SVG — see `diagrams`.)
fn generate_dot(frs_path: &Path) -> Result<Vec<u8>> {
    let out = Command::new("framec")
        .arg(frs_path)
        .arg("-l")
        .arg("graphviz")
        .output()
        .context("failed to invoke framec")?;
    if !out.status.success() {
        bail!(
            "framec failed for {}: {}",
            frs_path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out.stdout)
}

/// Render DOT to SVG via `dot -Tsvg`. The SVG is a *documentation render* only
/// (committed for GitHub display, NOT drift-gated): its bytes depend on the
/// GraphViz version, so byte-comparing it would pin the repo to one GraphViz.
fn render_svg(dot: &[u8]) -> Result<Vec<u8>> {
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
            .write_all(dot)
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
    /// A small raw image attached as a USB mass-storage device (R3b). Block 0
    /// carries a fixed magic so the kernel's SCSI READ(10) can verify it read
    /// real media. Attached read-only (the demo only reads), so it can be shared.
    usb_disk: PathBuf,
    /// Directory under `target/` where per-invocation vars copies live.
    qemu_dir: PathBuf,
}

// 4 MiB disk (B9.5). Big enough to exercise the multi-block bitmap (4 MiB /
// 512 = 8192 blocks → 2 bitmap blocks) and to hold the big-file test that
// spans double-indirect blocks, while staying small enough that copying the
// template per smoke test stays fast. The on-disk *format* scales to ~2 TB;
// this is just the default test image size.
const BLK_DISK_BLOCKS: u32 = 16384; // 8 MiB / 512 — holds the tcc sysroot
                                    // (/bin/tcc ~1.2 MiB dominates; the C-shim
                                    // libc.a is tiny) + the other ELFs, with room
                                    // to spare (B11-3d).

/// USB mass-storage backing image: 64 KiB (128 × 512-byte blocks), block 0
/// stamped with an 8-byte magic ("FRAMEOS!") the SCSI READ(10) test checks.
const USB_DISK_BLOCKS: u32 = 128;
const USB_DISK_MAGIC: &[u8; 8] = b"FRAMEOS!";

fn build_usb_disk_image() -> Vec<u8> {
    let mut img = vec![0u8; USB_DISK_BLOCKS as usize * 512];
    img[..8].copy_from_slice(USB_DISK_MAGIC);
    img
}

fn prepare_qemu_artifacts(workspace: &Path) -> Result<QemuArtifacts> {
    prepare_qemu_artifacts_features(workspace, false)
}

fn prepare_qemu_artifacts_features(workspace: &Path, interactive: bool) -> Result<QemuArtifacts> {
    let kernel_elf = build_kernel_features(workspace, interactive)?;
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
    let hello_elf = build_user_disk_elf(workspace, "hello")?;
    let argtest_elf = build_user_disk_elf(workspace, "argtest")?;
    let cmain_elf = build_user_disk_elf(workspace, "cmain")?;
    // S1: `ls` lists a directory via the readdir syscall (#21).
    let ls_elf = build_user_disk_elf(workspace, "ls")?;
    // S3 coreutils.
    let echo_elf = build_user_disk_elf(workspace, "echo")?;
    let rm_elf = build_user_disk_elf(workspace, "rm")?;
    let touch_elf = build_user_disk_elf(workspace, "touch")?;
    let cp_elf = build_user_disk_elf(workspace, "cp")?;
    // S4 text utilities.
    let wc_elf = build_user_disk_elf(workspace, "wc")?;
    let head_elf = build_user_disk_elf(workspace, "head")?;
    let tail_elf = build_user_disk_elf(workspace, "tail")?;
    let grep_elf = build_user_disk_elf(workspace, "grep")?;
    let date_elf = build_user_disk_elf(workspace, "date")?;
    // S7 directory ops.
    let mkdir_elf = build_user_disk_elf(workspace, "mkdir")?;
    let rmdir_elf = build_user_disk_elf(workspace, "rmdir")?;
    // B11-3e: the BuildDriver-FSM-driven build tool (compile→link→run via tcc).
    let build_elf = build_user_disk_elf(workspace, "buildc")?;
    // V1.0 capstone: the Hello Frame system (frame/hello.frs) transpiled to Rust
    // by framec and driven by the `fhello` bin. The SAME hello.frs is also
    // transpiled to C (staged at /fhello.c) and built on-device by tcc.
    let fhello_elf = build_user_disk_elf(workspace, "fhello")?;
    // B11-2: a C program cross-compiled (gcc + ld) against frame-libc.
    let chello_elf = build_c_disk_elf(workspace, "hello")?;
    // B11-3d: the on-device C compiler itself, cross-built against frame-libc.
    let tcc_elf = build_tcc_disk_elf(workspace)?;
    // B11-3d: the on-disk sysroot tcc compiles + links against (headers, crt
    // objects, libc.a, libtcc1.a) plus the /hello.c it compiles. Owned bytes,
    // held here so the `&[u8]` views pushed into `files` outlive build_fs_image.
    let sysroot = build_tcc_sysroot(workspace)?;
    // V1.0 capstone: frame/hello.frs transpiled to C by framec, plus a C main
    // harness, staged at /fhello.c. `buildc /fhello.c` compiles it with the
    // on-device tcc — the C half of "one Frame source, both backends".
    let fhello_c = build_fhello_c(workspace)?;
    let mut files: Vec<(&str, &[u8])> = FS_FILES.to_vec();
    files.push(("/bin/hello", &hello_elf));
    files.push(("/bin/argtest", &argtest_elf));
    files.push(("/bin/cmain", &cmain_elf));
    files.push(("/bin/ls", &ls_elf));
    files.push(("/bin/echo", &echo_elf));
    files.push(("/bin/rm", &rm_elf));
    files.push(("/bin/touch", &touch_elf));
    files.push(("/bin/cp", &cp_elf));
    files.push(("/bin/wc", &wc_elf));
    files.push(("/bin/head", &head_elf));
    files.push(("/bin/tail", &tail_elf));
    files.push(("/bin/grep", &grep_elf));
    files.push(("/bin/date", &date_elf));
    files.push(("/bin/mkdir", &mkdir_elf));
    files.push(("/bin/rmdir", &rmdir_elf));
    files.push(("/bin/chello", &chello_elf));
    files.push(("/bin/tcc", &tcc_elf));
    files.push(("/bin/buildc", &build_elf));
    files.push(("/bin/fhello", &fhello_elf));
    files.push(("/fhello.c", &fhello_c));
    for (path, data) in &sysroot {
        files.push((path.as_str(), data.as_slice()));
    }
    let image = build_fs_image(BLK_DISK_BLOCKS, &files);
    std::fs::write(&blk_template, &image)
        .with_context(|| format!("failed to write {}", blk_template.display()))?;

    // The USB mass-storage backing image (R3b): a small raw disk with a magic in
    // block 0. Written once and attached read-only, so a shared copy is safe.
    let usb_disk = qemu_dir.join("usb-msd.img");
    std::fs::write(&usb_disk, build_usb_disk_image())
        .with_context(|| format!("failed to write {}", usb_disk.display()))?;

    Ok(QemuArtifacts {
        esp_img,
        ovmf_code,
        ovmf_vars_template,
        blk_template,
        usb_disk,
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
    let layout = fs::Layout::for_total(total_blocks);
    let mut disk = vec![0u8; total_blocks as usize * fs::BLOCK_SIZE];

    let blk = |b: u32| {
        let s = b as usize * fs::BLOCK_SIZE;
        s..s + fs::BLOCK_SIZE
    };
    // Mark block `b` used in the (possibly multi-block) bitmap.
    let set_used = |disk: &mut [u8], b: u32| {
        let (bm_block, byte, bit) = layout.bitmap_loc(b);
        disk[bm_block as usize * fs::BLOCK_SIZE + byte] |= 1 << bit;
    };

    // Reserve the metadata region (superblock + bitmap + inode table).
    for b in 0..layout.data_start {
        set_used(&mut disk, b);
    }

    // Directory registry, keyed by full slash-path ("" = root). Root is inode 1.
    // Arbitrary nesting is supported: a file at `/usr/include/stdio.h`
    // auto-creates `/usr` then `/usr/include` (the B11-3d tcc sysroot needs
    // multi-level dirs; the kernel's `namei` already walks any depth). Each dir
    // grows across multiple 512-byte data blocks (16 dirents each) as entries
    // are added, so a directory like /bin can exceed 16 files.
    let mut next_ino = fs::ROOT_INODE; // 1
    let mut next_data = layout.data_start;
    let alloc_dir = |next_ino: &mut u32, next_data: &mut u32, disk: &mut [u8]| -> (u32, u32) {
        let ino = *next_ino;
        *next_ino += 1;
        let dblk = *next_data;
        *next_data += 1;
        set_used(disk, dblk);
        (ino, dblk)
    };
    let (root_ino, root_data) = alloc_dir(&mut next_ino, &mut next_data, &mut disk);
    // dirs: (full_path, ino, data_blocks, dirent_byte_len). A directory grows
    // into more blocks as entries are added (DIRENT_SIZE 32 evenly divides
    // BLOCK_SIZE 512 → 16/block), mirroring the kernel's multi-block fs::create —
    // so /bin can hold more than 16 programs.
    let mut dirs: Vec<(String, u32, Vec<u32>, usize)> =
        vec![(String::new(), root_ino, vec![root_data], 0)];

    for (path, data) in files {
        let p = path.trim_start_matches('/');
        let (dirname, name) = match p.rsplit_once('/') {
            Some((d, n)) => (d, n),
            None => ("", p),
        };
        // Walk/create the parent directory chain; `di` ends at the leaf dir.
        let mut di = 0usize; // root
        if !dirname.is_empty() {
            let mut acc = String::new();
            for comp in dirname.split('/') {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(comp);
                di = match dirs.iter().position(|d| d.0 == acc) {
                    Some(i) => i,
                    None => {
                        let (ino, dblk) = alloc_dir(&mut next_ino, &mut next_data, &mut disk);
                        // Link the new subdir's dirent into its parent (`di`),
                        // growing the parent into a new block if the current one
                        // is full.
                        let off = dirs[di].3 % fs::BLOCK_SIZE;
                        let bi = dirs[di].3 / fs::BLOCK_SIZE;
                        if bi >= dirs[di].2.len() {
                            let nb = next_data;
                            next_data += 1;
                            set_used(&mut disk, nb);
                            dirs[di].2.push(nb);
                        }
                        let pblk = dirs[di].2[bi];
                        fs::write_dirent(&mut disk[blk(pblk)], off, comp.as_bytes(), ino);
                        dirs[di].3 += fs::DIRENT_SIZE;
                        dirs.push((acc.clone(), ino, vec![dblk], 0));
                        dirs.len() - 1
                    }
                };
            }
        }

        // Allocate the file inode + data blocks.
        let ino = next_ino;
        next_ino += 1;
        let mut node = fs::Inode::empty();
        node.kind = fs::T_FILE;
        node.nlink = 1;
        node.size = data.len() as u32;
        let nb = data.len().div_ceil(fs::BLOCK_SIZE);
        // Allocate + fill the data blocks, collecting their numbers.
        let mut blocks: Vec<u32> = Vec::with_capacity(nb);
        for i in 0..nb {
            let b = next_data;
            next_data += 1;
            set_used(&mut disk, b);
            let lo = i * fs::BLOCK_SIZE;
            let hi = ((i + 1) * fs::BLOCK_SIZE).min(data.len());
            let r = blk(b);
            disk[r.start..r.start + (hi - lo)].copy_from_slice(&data[lo..hi]);
            blocks.push(b);
        }
        // Distribute the data blocks across direct + single/double indirect,
        // mirroring the kernel's `block_for` so logical block i lands at
        // blocks[i] either way (B9.5 format; host indirect staging). Lets the
        // host stage files up to ~8 MiB (tcc-scale, B11).
        let ptrs = fs::PTRS_PER_BLOCK;
        let le = |disk: &mut [u8], at: usize, v: u32| {
            disk[at..at + 4].copy_from_slice(&v.to_le_bytes());
        };
        for (i, &b) in blocks.iter().take(fs::NDIRECT).enumerate() {
            node.direct[i] = b;
        }
        if nb > fs::NDIRECT {
            let ind = next_data;
            next_data += 1;
            set_used(&mut disk, ind);
            node.indirect = ind;
            let base = blk(ind).start;
            for (k, &b) in blocks.iter().skip(fs::NDIRECT).take(ptrs).enumerate() {
                le(&mut disk, base + k * 4, b);
            }
        }
        if nb > fs::NDIRECT + ptrs {
            let dind = next_data;
            next_data += 1;
            set_used(&mut disk, dind);
            node.double_indirect = dind;
            let dbase = blk(dind).start;
            for (l1, chunk) in blocks[fs::NDIRECT + ptrs..].chunks(ptrs).enumerate() {
                let mid = next_data;
                next_data += 1;
                set_used(&mut disk, mid);
                le(&mut disk, dbase + l1 * 4, mid);
                let mbase = blk(mid).start;
                for (k, &b) in chunk.iter().enumerate() {
                    le(&mut disk, mbase + k * 4, b);
                }
            }
        }
        let (iblk, ioff) = layout.inode_loc(ino);
        node.write(&mut disk[blk(iblk)], ioff);

        // Add the file's dirent to its parent directory, growing into a new
        // data block when the current one fills (multi-block dirs).
        let off = dirs[di].3 % fs::BLOCK_SIZE;
        let bi = dirs[di].3 / fs::BLOCK_SIZE;
        if bi >= dirs[di].2.len() {
            let nb = next_data;
            next_data += 1;
            set_used(&mut disk, nb);
            dirs[di].2.push(nb);
        }
        let pblk = dirs[di].2[bi];
        fs::write_dirent(&mut disk[blk(pblk)], off, name.as_bytes(), ino);
        dirs[di].3 += fs::DIRENT_SIZE;
    }

    // Write every directory inode with its final size + all its data blocks
    // (directories use direct blocks only — NDIRECT × 16 = 448 entries, ample).
    for (_, ino, blocks, len) in &dirs {
        let mut d = fs::Inode::empty();
        d.kind = fs::T_DIR;
        d.nlink = 1;
        for (i, &b) in blocks.iter().take(fs::NDIRECT).enumerate() {
            d.direct[i] = b;
        }
        d.size = *len as u32;
        let (iblk, ioff) = layout.inode_loc(*ino);
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
fn build_user_disk_elf(workspace: &Path, bin: &str) -> Result<Vec<u8>> {
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
        .args(["build", "--release", "--bin", bin])
        .status()
        .with_context(|| {
            format!(
                "failed to invoke cargo for user crate at {}",
                user_dir.display()
            )
        })?;
    if !status.success() {
        bail!("user `{bin}` build failed");
    }

    let elf = user_target
        .join("x86_64-unknown-none")
        .join("release")
        .join(bin);
    std::fs::read(&elf).with_context(|| format!("failed to read user ELF {}", elf.display()))
}

/// Compile frame-libc's C shims (`libc/csrc/*.c`) into objects, returning their
/// paths. These are the parts of frame-libc that must be C, not Rust — currently
/// only `strtold` (80-bit `long double`, which Rust has no type for, B11-3c).
/// Built with the same freestanding flags as the C programs and linked into any
/// C program built against frame-libc (`--gc-sections` drops unused ones).
fn build_libc_cshims(workspace: &Path, include: &Path, out_dir: &Path) -> Result<Vec<PathBuf>> {
    let csrc = workspace.join("libc").join("csrc");
    let mut objs = Vec::new();
    // C shims that complete the frame-libc surface (currently just `strtold`,
    // which Rust can't express). A slice so the set can grow without churn.
    const SHIMS: &[&str] = &["strtold"];
    for name in SHIMS {
        let src = csrc.join(format!("{name}.c"));
        let obj = out_dir.join(format!("libc_{name}.o"));
        let status = Command::new("x86_64-linux-gnu-gcc")
            .args([
                "-ffreestanding",
                "-fno-stack-protector",
                "-fno-pie",
                "-fno-pic",
                "-O2",
                "-c",
            ])
            .arg("-I")
            .arg(include)
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .status()
            .with_context(|| format!("failed to invoke gcc for libc shim {name}"))?;
        if !status.success() {
            bail!("gcc compile of libc/csrc/{name}.c failed");
        }
        objs.push(obj);
    }
    Ok(objs)
}

/// Build frame-os-libc as a staticlib and return the path to its
/// `libframe_os_libc.a`. Shared by every C program build (chello, tcc): the `.a`
/// is the C/POSIX runtime + crt0 they link against. The nested cargo scrubs the
/// outer build's RUSTFLAGS / rustc wrapper so they can't leak into the user
/// link, and uses its own target dir (libc/.cargo/config sets target + reloc).
fn build_libc_staticlib(workspace: &Path) -> Result<PathBuf> {
    build_libc_staticlib_features(workspace, "libc-staticlib", true)
}

/// Build frame-os-libc as a staticlib with a chosen feature set, into its own
/// target subdir (so variants don't clobber each other's `.a`). `crt0` selects
/// whether the libc provides the program entry `_start`: ON for the
/// direct-link runtime (chello, baked programs); OFF for the tcc sysroot's
/// `libc.a`, where `crt1.o` owns `_start` instead (see libc/Cargo.toml).
fn build_libc_staticlib_features(workspace: &Path, subdir: &str, crt0: bool) -> Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let libc_dir = workspace.join("libc");
    let lib_target = target_dir(workspace).join(subdir);
    let mut cmd = Command::new(&cargo);
    cmd.current_dir(&libc_dir)
        .env("CARGO_TARGET_DIR", &lib_target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .args(["build", "--release"]);
    if !crt0 {
        cmd.arg("--no-default-features");
        // The tcc-sysroot libc.a is linked *by tcc* on-device. The x86_64
        // target defaults to GOT-based symbol access (thousands of
        // R_X86_64_GOTPCREL relocations); tcc's GOT construction for a fully
        // static executable produces wrong addresses, so the linked program
        // jumps into garbage. `relocation-model=static` makes LLVM emit direct
        // PC-relative relocations (PC32/32S) instead — no GOT, and the simplest
        // relocations tcc applies reliably. (The crt0 variant, linked by the
        // host ld into tcc.elf itself, is left untouched.)
        cmd.env("RUSTFLAGS", "-C relocation-model=static");
    }
    let status = cmd
        .status()
        .context("failed to invoke cargo for the frame-os-libc staticlib")?;
    if !status.success() {
        bail!("frame-os-libc staticlib build failed");
    }
    Ok(lib_target
        .join("x86_64-unknown-none")
        .join("release")
        .join("libframe_os_libc.a"))
}

/// Cross-compile a C program in `csrc/<name>.c` against frame-libc and return
/// the Frame-OS ELF bytes (B11-2). Three steps, all in the container:
///   1. build frame-os-libc as a staticlib (`libframe_os_libc.a`);
///   2. `gcc -ffreestanding -nostdlib …` compile the C to an object;
///   3. `ld` link the object + the `.a` with the user linker script (ENTRY
///      `_start` comes from the libc's crt0) into a non-PIE ET_EXEC.
///
/// This is exactly the toolchain flow tcc will perform on-device at B11-3.
fn build_c_disk_elf(workspace: &Path, name: &str) -> Result<Vec<u8>> {
    // 1. frame-os-libc staticlib (shared with the tcc build).
    let libc_dir = workspace.join("libc");
    let lib_a = build_libc_staticlib(workspace)?;

    let out_dir = target_dir(workspace).join("csrc");
    std::fs::create_dir_all(&out_dir)?;
    let obj = out_dir.join(format!("{name}.o"));
    let elf = out_dir.join(format!("{name}.elf"));
    let src = workspace.join("csrc").join(format!("{name}.c"));
    let include = libc_dir.join("include");

    // 2. Compile: freestanding, no builtins (don't lower printf→puts behind our
    //    back), non-PIE (matches the static-reloc layout the linker script wants).
    //    The container is arm64, so use the x86_64 cross compiler.
    let status = Command::new("x86_64-linux-gnu-gcc")
        .args([
            "-ffreestanding",
            "-fno-builtin",
            "-fno-stack-protector",
            "-fno-pie",
            "-fno-pic",
            "-O2",
            "-c",
        ])
        .arg("-I")
        .arg(&include)
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .status()
        .context("failed to invoke gcc")?;
    if !status.success() {
        bail!("gcc compile of csrc/{name}.c failed");
    }

    // 2b. Compile frame-libc's C shim(s) — the bits that must be C, not Rust
    //     (currently just `strtold`, 80-bit long double; Rust has no f80). Linked
    //     alongside the Rust staticlib; --gc-sections drops them if unused.
    let shims = build_libc_cshims(workspace, &include, &out_dir)?;

    // 3. Link with the Frame OS user linker script; gc unused .a sections.
    //    x86_64 cross ld (the .a + the linker script's OUTPUT_FORMAT are x86-64).
    let linker_script = workspace.join("user").join("linker.ld");
    let mut ld = Command::new("x86_64-linux-gnu-ld");
    ld.arg("-T")
        .arg(&linker_script)
        // gc unused .a sections + strip symbols → a lean ELF. (B11-2 briefly
        // shipped unstripped after `--strip-all` *appeared* to cause an in-kernel
        // fault; that was a misdiagnosis — the real bug was a kernel exec/trap-
        // frame race exposed by timing, fixed since. Stripping is fine: the
        // loader reads only program headers, which strip leaves untouched.)
        .args(["-z", "max-page-size=0x1000", "--gc-sections", "--strip-all"])
        .arg("-o")
        .arg(&elf)
        .arg(&obj);
    for shim in &shims {
        ld.arg(shim);
    }
    let status = ld.arg(&lib_a).status().context("failed to invoke ld")?;
    if !status.success() {
        bail!("ld link of {name} failed");
    }

    std::fs::read(&elf).with_context(|| format!("failed to read linked ELF {}", elf.display()))
}

/// Cross-compile the vendored TinyCC (`third_party/tcc`) against frame-libc into
/// a Frame OS user ELF, returned as bytes for staging at `/bin/tcc` (B11-3d) —
/// the on-device C toolchain. tcc 0.9.27 defaults to ONE_SOURCE, so it's two
/// translation units: `libtcc.c` (`-DONE_SOURCE=1`, which `#include`s the
/// preprocessor/codegen/ELF/asm/x86_64 backend) and `tcc.c` (`-DONE_SOURCE=0`,
/// the CLI driver, which `#include`s `tcctools.c`). Both are compiled
/// freestanding + `-nostdinc` against the gcc freestanding headers (for
/// `stdarg`/`stddef`/`stdint`) + `libc/include` + the tcc dir, then linked with
/// the user linker script + the frame-libc staticlib + the `strtold` C shim.
/// Build defines per third_party/tcc/README.frame-os.md: `TCC_TARGET_X86_64`,
/// `CONFIG_TCC_STATIC`, `CONFIG_TCCBOOT`. `-w` silences upstream warnings (the
/// vendored source is unmodified, so they aren't actionable here).
fn build_tcc_disk_elf(workspace: &Path) -> Result<Vec<u8>> {
    let tcc_dir = workspace.join("third_party").join("tcc");
    let include = workspace.join("libc").join("include");
    let out_dir = target_dir(workspace).join("tcc-build");
    std::fs::create_dir_all(&out_dir)?;

    // gcc's own freestanding header dir (stdarg.h/stddef.h/stdint.h) — queried so
    // the gcc version isn't hard-coded into a path.
    let freestd = {
        let out = Command::new("x86_64-linux-gnu-gcc")
            .args(["-print-file-name=include"])
            .output()
            .context("failed to query gcc freestanding include dir")?;
        if !out.status.success() {
            bail!("`gcc -print-file-name=include` failed");
        }
        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
    };

    // Compile one tcc translation unit. `one_source` is "-DONE_SOURCE=1" or "=0".
    let compile = |src: &str, one_source: &str, obj: &Path| -> Result<()> {
        let status = Command::new("x86_64-linux-gnu-gcc")
            .args([
                "-ffreestanding",
                "-nostdinc",
                "-fno-stack-protector",
                "-fno-pie",
                "-fno-pic",
                "-O2",
                "-w",
                "-c",
            ])
            // -DNDEBUG: build tcc with its internal asserts compiled out (the
            // conventional release build). frame-libc's <assert.h> is now a real
            // abort (B11-3 follow-up); without NDEBUG, a tcc-internal assert would
            // pull in __assert_fail and abort the compiler. tcc-compiled *user*
            // programs are unaffected — the on-device tcc never passes -DNDEBUG,
            // so their asserts stay live.
            .args([
                "-DTCC_TARGET_X86_64",
                "-DCONFIG_TCC_STATIC",
                "-DCONFIG_TCCBOOT",
                "-DNDEBUG",
            ])
            .arg(one_source)
            .arg("-isystem")
            .arg(&freestd)
            .arg("-I")
            .arg(&include)
            .arg("-I")
            .arg(&tcc_dir)
            .arg(tcc_dir.join(src))
            .arg("-o")
            .arg(obj)
            .status()
            .with_context(|| format!("failed to invoke gcc for tcc unit {src}"))?;
        if !status.success() {
            bail!("gcc compile of third_party/tcc/{src} failed");
        }
        Ok(())
    };
    let libtcc_o = out_dir.join("libtcc.o");
    let tcc_o = out_dir.join("tcc.o");
    compile("libtcc.c", "-DONE_SOURCE=1", &libtcc_o)?;
    compile("tcc.c", "-DONE_SOURCE=0", &tcc_o)?;

    // The frame-libc staticlib + its C shims (strtold) — same runtime chello links.
    let lib_a = build_libc_staticlib(workspace)?;
    let shims = build_libc_cshims(workspace, &include, &out_dir)?;

    // Link: user linker script (ENTRY `_start` from crt0), gc + strip.
    let elf = out_dir.join("tcc.elf");
    let linker_script = workspace.join("user").join("linker.ld");
    let mut ld = Command::new("x86_64-linux-gnu-ld");
    ld.arg("-T")
        .arg(&linker_script)
        .args(["-z", "max-page-size=0x1000", "--gc-sections", "--strip-all"])
        .arg("-o")
        .arg(&elf)
        .arg(&libtcc_o)
        .arg(&tcc_o);
    for shim in &shims {
        ld.arg(shim);
    }
    let status = ld
        .arg(&lib_a)
        .status()
        .context("failed to invoke ld for tcc")?;
    if !status.success() {
        bail!("ld link of tcc failed");
    }
    std::fs::read(&elf).with_context(|| format!("failed to read linked tcc ELF {}", elf.display()))
}

/// Stage a minimal on-disk sysroot so the on-device tcc can compile *and link* a
/// C program the standard way (B11-3d):
///
///     tcc -B/usr/lib/tcc -static /hello.c -o /out.elf
///
/// Returns (disk-path, bytes) pairs for `build_fs_image`. The `-B/usr/lib/tcc`
/// sets tcc's lib path so `{B}/include` (its intrinsic headers) and
/// `{B}/libtcc1.a` resolve there; the default crt prefix (`/usr/lib`) and lib
/// path (`/usr/lib`) cover the crt objects + `libc.a`; the default system
/// include path (`/usr/include`) covers frame-libc's headers. `-static` keeps
/// tcc from emitting a PT_INTERP / dynamic sections (Frame OS has no dynamic
/// loader). Layout:
///   /usr/include/**          frame-libc's system headers (stdio.h, sys/…)
///   /usr/lib/tcc/include/*   tcc's intrinsic headers (stdarg.h, stddef.h, …)
///   /usr/lib/{crt1,crti,crtn}.o   C startup objects (crt1.o owns `_start`)
///   /usr/lib/libc.a          frame-libc, built WITHOUT its own `_start`
///   /usr/lib/tcc/libtcc1.a   tcc runtime support — empty for now (only
///                            variadic-*defining* programs reference its symbols;
///                            hello.c just calls printf). A real one is a follow-up.
///   /hello.c                 the program to compile (csrc/tcchello.c)
fn build_tcc_sysroot(workspace: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let out_dir = target_dir(workspace).join("tcc-sysroot");
    std::fs::create_dir_all(&out_dir)?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();

    // frame-libc system headers -> /usr/include (one level of subdir for sys/).
    let inc = workspace.join("libc").join("include");
    for entry in std::fs::read_dir(&inc)? {
        let path = entry?.path();
        if path.is_dir() {
            let sub = path.file_name().unwrap().to_string_lossy().into_owned();
            for e2 in std::fs::read_dir(&path)? {
                let p2 = e2?.path();
                let name = p2.file_name().unwrap().to_string_lossy().into_owned();
                files.push((format!("/usr/include/{sub}/{name}"), std::fs::read(&p2)?));
            }
        } else {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            files.push((format!("/usr/include/{name}"), std::fs::read(&path)?));
        }
    }

    // tcc's intrinsic headers -> /usr/lib/tcc/include (= {B}/include).
    let tinc = workspace.join("third_party").join("tcc").join("include");
    for entry in std::fs::read_dir(&tinc)? {
        let p = entry?.path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        files.push((format!("/usr/lib/tcc/include/{name}"), std::fs::read(&p)?));
    }

    // crt startup objects: assemble crt1.s/crti.s/crtn.s with the cross gcc.
    let csrc = workspace.join("libc").join("csrc");
    for crt in ["crt1", "crti", "crtn"] {
        let src = csrc.join(format!("{crt}.s"));
        let obj = out_dir.join(format!("{crt}.o"));
        let status = Command::new("x86_64-linux-gnu-gcc")
            .args(["-ffreestanding", "-fno-pie", "-fno-pic", "-c"])
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .status()
            .with_context(|| format!("failed to invoke gcc to assemble {crt}.s"))?;
        if !status.success() {
            bail!("gcc assemble of libc/csrc/{crt}.s failed");
        }
        files.push((format!("/usr/lib/{crt}.o"), std::fs::read(&obj)?));
    }

    // libc.a — the C-shim (libc/cshim/cshim.c), NOT the Rust frame-libc.
    //
    // tcc 0.9.27's fully-static linker mishandles the GOT/PLT relocations a
    // Rust/LLVM staticlib is full of (broken PLT + unfilled GOT; see
    // third_party/tcc/README.frame-os.md). The C-shim sidesteps both:
    //   -fno-pic           → direct data addressing, ZERO GOT relocations.
    //   -fvisibility=hidden → every shim symbol hidden, so tcc's existing linker
    //                         resolves PLT32 calls to them directly (no PLT).
    // The result links to only the simple PC32/64 relocations tcc applies
    // reliably. `crt1.o` owns `_start` and calls the shim's `__libc_start`.
    let cshim_c = workspace.join("libc").join("cshim").join("cshim.c");
    let cshim_o = out_dir.join("cshim.o");
    let status = Command::new("x86_64-linux-gnu-gcc")
        .args([
            "-ffreestanding",
            "-nostdinc",
            "-fno-stack-protector",
            "-fno-pie",
            "-fno-pic",
            "-fvisibility=hidden",
            "-O2",
            "-c",
        ])
        .arg(&cshim_c)
        .arg("-o")
        .arg(&cshim_o)
        .status()
        .context("failed to invoke gcc for the C-shim libc")?;
    if !status.success() {
        bail!("gcc compile of libc/cshim/cshim.c failed");
    }
    let lib_a = out_dir.join("libc.a");
    let _ = std::fs::remove_file(&lib_a);
    let status = Command::new("x86_64-linux-gnu-ar")
        .arg("crs")
        .arg(&lib_a)
        .arg(&cshim_o)
        .status()
        .context("failed to archive cshim.o into libc.a")?;
    if !status.success() {
        bail!("ar of C-shim libc.a failed");
    }
    files.push(("/usr/lib/libc.a".into(), std::fs::read(&lib_a)?));

    // Empty tcc runtime-support archive. tcc auto-links libtcc1.a for every
    // executable and errors if it's missing, but a program that only *calls*
    // variadic functions (printf) references none of its symbols, so an empty
    // (but well-formed) archive satisfies the link. `ar` writes the `!<arch>`
    // header with zero members; tcc_load_archive iterates nothing and moves on.
    let libtcc1 = out_dir.join("libtcc1.a");
    let _ = std::fs::remove_file(&libtcc1);
    let status = Command::new("x86_64-linux-gnu-ar")
        .arg("crs")
        .arg(&libtcc1)
        .status()
        .context("failed to invoke ar for empty libtcc1.a")?;
    if !status.success() {
        bail!("ar create of empty libtcc1.a failed");
    }
    files.push(("/usr/lib/tcc/libtcc1.a".into(), std::fs::read(&libtcc1)?));

    // The C program the on-device tcc will compile.
    let hello_c = workspace.join("csrc").join("tcchello.c");
    files.push(("/hello.c".into(), std::fs::read(&hello_c)?));

    // A program whose assert fails — proves assert() is a real abort (B11-3
    // follow-up), compiled + run on-device by the console-test.
    let assert_c = workspace.join("csrc").join("tcc_assert.c");
    files.push(("/assert.c".into(), std::fs::read(&assert_c)?));

    // A second source so the console-test can prove `buildc <src>` reads its
    // source path from argv (compiles /hi.c -> /hi.elf, exit 3).
    let hi_c = workspace.join("csrc").join("tcc_hi.c");
    files.push(("/hi.c".into(), std::fs::read(&hi_c)?));

    Ok(files)
}

/// Transpile `frame/hello.frs` to C with framec and append the C `main` harness
/// (`csrc/fhello_main.c`), yielding the complete `/fhello.c` the on-device tcc
/// compiles (V1.0 capstone, C half). The Rust half (`/bin/fhello`) is generated
/// from the *same* hello.frs via `framec -l rust` in user/build.rs.
fn build_fhello_c(workspace: &Path) -> Result<Vec<u8>> {
    let frame_src = workspace.join("frame").join("hello.frs");
    let out_dir = target_dir(workspace).join("fhello-build");
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;
    let status = Command::new("framec")
        .arg("compile")
        .arg("-l")
        .arg("c")
        .arg("-o")
        .arg(&out_dir)
        .arg(&frame_src)
        .status()
        .context("failed to invoke framec -l c on hello.frs")?;
    if !status.success() {
        bail!("framec -l c failed for frame/hello.frs");
    }
    let gen = out_dir.join("hello.c");
    let mut c =
        std::fs::read(&gen).with_context(|| format!("framec did not produce {}", gen.display()))?;
    // Append the C main harness (concatenated, so it uses the generated prefix).
    let main_c = workspace.join("csrc").join("fhello_main.c");
    c.push(b'\n');
    c.extend_from_slice(
        &std::fs::read(&main_c).with_context(|| format!("read {}", main_c.display()))?,
    );
    Ok(c)
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
        // B11-3 follow-up: pin the CMOS RTC to a fixed wall-clock base so the
        // kernel's `time()` syscall (and thus libc time/localtime, and tcc's
        // __DATE__/__TIME__) is deterministic across test runs. `clock=vm`
        // advances the guest clock with VM time from this base (no real-time
        // drift). The kernel reads the RTC as UTC, matching the face value here.
        .args(["-rtc", "base=2026-05-24T12:00:00,clock=vm"])
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
                "user,id=net0,hostfwd=tcp::{TCP_PROBE_PORT}-:7,{MULTI_HOSTFWD},guestfwd=tcp:10.0.2.100:9-tcp:127.0.0.1:{TCP_ACTIVE_PORT}"
            ),
            NetMode::Slirp { guestfwd: false } => {
                format!("user,id=net0,hostfwd=tcp::{TCP_PROBE_PORT}-:7,{MULTI_HOSTFWD}")
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
        // Two HID devices on the controller (R3): a keyboard and a mouse on
        // separate root-hub ports. The kernel enumerates both *concurrently*
        // (one HubPort + UsbEnumeration instance per device, demuxed by slot).
        // The keyboard lands on the lower USB2 port (5), so it is device 0 — the
        // target of the interrupt-IN keypress transfer (B6-4). Harmless for
        // B0–B5 (nothing touches USB there).
        .args(["-device", "usb-kbd,bus=xhci.0"])
        .args(["-device", "usb-mouse,bus=xhci.0"])
        // R3b: a USB mass-storage device (Bulk-Only Transport / SCSI) on a third
        // port. The kernel enumerates it alongside the HID devices, then
        // configures its bulk IN/OUT endpoints and drives SCSI INQUIRY / READ
        // CAPACITY / READ(10) through the UsbMsd Frame system. Attached read-only
        // (the demo only reads), backed by a shared raw image with a magic in
        // block 0.
        .args(["-drive"])
        .arg(format!(
            "if=none,id=usbdisk,format=raw,readonly=on,file={}",
            artifacts.usb_disk.display()
        ))
        .args(["-device", "usb-storage,drive=usbdisk,bus=xhci.0"])
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

/// `cargo xtask qemu` — boot the demo kernel (runs the self-test demos, halts).
fn run_qemu() -> Result<()> {
    run_qemu_inner(false)
}

/// `cargo xtask qemu-interactive` — boot the interactive kernel and hand the
/// serial console to the user's terminal, so they get the `frameos$` prompt and
/// can type programs themselves (the human-typeable counterpart to
/// `console-test`, which drives the same kernel scripted).
fn run_qemu_interactive() -> Result<()> {
    run_qemu_inner(true)
}

/// Shared QEMU launch with serial on stdio. `interactive` selects the
/// `interactive`-feature kernel (boots straight to the ring-3 shell) vs. the
/// default demo kernel (runs the B0–B7 self-tests then halts).
fn run_qemu_inner(interactive: bool) -> Result<()> {
    let workspace = workspace_root()?;
    let artifacts = prepare_qemu_artifacts_features(&workspace, interactive)?;

    if interactive {
        eprintln!("booting INTERACTIVE Frame OS in QEMU. At the `frameos$ ` prompt, try:");
        eprintln!("  /bin/hello            # a Rust ELF");
        eprintln!("  /bin/tcc -v           # the on-device C compiler");
        eprintln!("  buildc /hi.c          # compile+link+run C via the BuildDriver FSM");
        eprintln!("  /bin/fhello           # the capstone: a Frame system -> Rust");
        eprintln!(
            "  buildc /fhello.c      # the capstone: the same Frame system -> C, built by tcc"
        );
        eprintln!("  exit                  # leave (or Ctrl-A x to quit QEMU)");
    } else {
        eprintln!("booting kernel in QEMU (Ctrl-C or Ctrl-A x to quit)...");
    }

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
// `cargo xtask console-test` — drive the interactive shell over serial (B8)
//
// Boots the `interactive`-feature kernel with `-serial stdio`, then scripts a
// session over QEMU's stdin/stdout: wait for the shell prompt, type a command,
// read the program's output, type `exit`. A reader thread drains stdout into a
// shared buffer so we can wait for prompts without blocking the writer.
// ---------------------------------------------------------------------------

/// Wait until `needle` appears in the shared capture buffer, or time out.
fn wait_for_output(
    buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    needle: &str,
    secs: u64,
) -> Result<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if String::from_utf8_lossy(&buf.lock().unwrap()).contains(needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "console-test: timed out waiting for {:?}. Captured so far:\n---\n{}\n---",
                needle,
                String::from_utf8_lossy(&buf.lock().unwrap())
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run_console_test() -> Result<()> {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let workspace = workspace_root()?;
    let artifacts = prepare_qemu_artifacts_features(&workspace, true)?;
    let ovmf_vars = fresh_ovmf_vars(&artifacts, "console")?;
    let blk_disk = fresh_blk_disk(&artifacts, "console")?;

    let mut cmd = qemu_base_command(
        &artifacts,
        &ovmf_vars,
        &blk_disk,
        &NetMode::Slirp { guestfwd: false },
    );
    cmd.args(["-serial", "stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    eprintln!("console-test: booting interactive kernel...");
    let mut child = cmd.spawn().context("failed to spawn qemu")?;
    let mut stdin = child.stdin.take().context("no qemu stdin")?;
    let mut stdout = child.stdout.take().context("no qemu stdout")?;

    // Reader thread: drain QEMU stdout into a shared buffer until EOF.
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let reader_buf = Arc::clone(&buf);
    let reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => reader_buf.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    });

    // Drive the session as a fallible block: prompt → run hello → exit. Any
    // failure is captured (not `?`-propagated) so QEMU is always cleaned up below.
    //
    // Each step's `wait_for_output` budget is 45s (compile-heavy tcc/buildc steps
    // get 90s). The waits gate on *deterministic* program output, so the only
    // variable is emulator speed: the dev image is arm64 emulating x86_64 via
    // TCG, where a busy host makes any single step occasionally exceed a tight
    // budget. 45s keeps the test reliably green without weakening any assertion.
    let drive: Result<()> = (|| {
        wait_for_output(&buf, "frameos$ ", 45)?;
        eprintln!("console-test: prompt is up; typing `/bin/hello`");
        stdin.write_all(b"/bin/hello\n").context("write hello")?;
        stdin.flush().ok();
        wait_for_output(&buf, "hello from ELF", 45)?;
        // S1/S2: shell navigation on the Frame OS filesystem — pwd, ls (readdir
        // syscall #21), and cd. `ls` at root shows /readme; after `cd /usr`, pwd
        // reports /usr and `ls` shows the staged `include` dir.
        eprintln!("console-test: typing pwd / ls / cd /usr");
        stdin
            .write_all(b"ls\ncd /usr\npwd\nls\ncd /\n")
            .context("write nav")?;
        stdin.flush().ok();
        wait_for_output(&buf, "readme", 45)?; // ls at root lists /readme
        wait_for_output(&buf, "/usr", 45)?; // pwd after `cd /usr`
        wait_for_output(&buf, "include", 45)?; // ls /usr lists `include`
                                               // S3 coreutils on the Frame OS filesystem: touch creates a file (ls
                                               // sees it), echo prints args, cp copies /readme → /b.txt (cat proves
                                               // the bytes), rm deletes. (cwd is / after the nav block's `cd /`.)
        eprintln!("console-test: typing touch / echo / cp / cat / rm");
        stdin
            .write_all(
                b"touch /a.txt\nls\necho hi-there\ncp /readme /b.txt\ncat /b.txt\nrm /a.txt\n",
            )
            .context("write coreutils")?;
        stdin.flush().ok();
        wait_for_output(&buf, "a.txt", 45)?; // ls after `touch /a.txt`
        wait_for_output(&buf, "hi-there", 45)?; // echo
        wait_for_output(&buf, "hello from the disk", 45)?; // cat of the cp'd /readme
                                                           // S4 text utilities: wc counts /readme ("hello from the disk\n" = 1 line,
                                                           // 4 words, 20 bytes), date prints the pinned RTC, and head/tail/grep/clear
                                                           // run without error. (clear's ANSI escape is harmless in the capture.)
        eprintln!("console-test: typing wc / head / tail / grep / date / clear");
        stdin
            .write_all(b"wc /readme\nhead /readme\ntail /readme\ngrep disk /readme\ndate\nclear\n")
            .context("write textutils")?;
        stdin.flush().ok();
        wait_for_output(&buf, "1 4 20 /readme", 45)?; // wc
        wait_for_output(&buf, "2026-05-24 12:", 45)?; // date (pinned RTC base)
                                                      // S5 I/O redirection: `>` creates /r.txt with echo's stdout, `>>` appends
                                                      // (must NOT truncate), and `wc < /r.txt` reads it back via stdin. The only
                                                      // assertion is `wc`'s "2 2 20": that exact count appears only if `>` made the
                                                      // file ("redir-out\n", 10B), `>>` appended ("redir-app\n", 10B → 2 lines, 2
                                                      // words, 20 bytes total, proving it didn't truncate), and `<` fed it to wc.
                                                      // (We can't assert on "redir-out"/"redir-app" themselves — the kernel echoes
                                                      // the typed command line, so those strings are in the stream regardless.)
        eprintln!("console-test: typing echo > / >> / wc < (redirection)");
        stdin
            .write_all(
                b"echo redir-out > /r.txt\ncat /r.txt\necho redir-app >> /r.txt\ncat /r.txt\nwc < /r.txt\n",
            )
            .context("write redirection")?;
        stdin.flush().ok();
        wait_for_output(&buf, "2 2 20", 45)?; // wc < /r.txt after > then >>
                                              // S6 pipe: connect echo's stdout to wc's stdin. `echo pipe one two` writes
                                              // "pipe one two\n" (13 bytes, 3 words, 1 line) into the pipe; wc reads it
                                              // from stdin and prints "1 3 13" — which is not in the typed command, so it
                                              // only appears if the pipe actually carried the bytes between the two procs.
        eprintln!("console-test: typing `echo pipe one two | wc` (pipe)");
        stdin
            .write_all(b"echo pipe one two | wc\n")
            .context("write pipe")?;
        stdin.flush().ok();
        wait_for_output(&buf, "1 3 13", 45)?; // wc counting echo's piped stdout
                                              // S7 directories: mkdir /zsub creates it; the *second* mkdir fails
                                              // ("cannot create directory") which proves the first one created it;
                                              // rmdir removes it, after which `cd /zsub` fails ("no such directory")
                                              // proving it's gone. Both asserted strings are program/shell output, not
                                              // in the typed commands, so they can't be satisfied by the input echo.
        eprintln!("console-test: typing mkdir / rmdir (directories)");
        stdin
            .write_all(b"mkdir /zsub\nmkdir /zsub\nrmdir /zsub\ncd /zsub\n")
            .context("write mkdir/rmdir")?;
        stdin.flush().ok();
        wait_for_output(&buf, "cannot create directory", 45)?; // 2nd mkdir → dir exists ⇒ 1st made it
        wait_for_output(&buf, "no such directory: /zsub", 45)?; // cd after rmdir ⇒ rmdir removed it
                                                                // B9-2: argv reaches the program. Type a command WITH arguments; argtest
                                                                // echoes argc + each argv string, so we can assert the args arrived.
        eprintln!("console-test: typing `/bin/argtest alpha beta`");
        stdin
            .write_all(b"/bin/argtest alpha beta\n")
            .context("write argtest")?;
        stdin.flush().ok();
        wait_for_output(&buf, "argv[1]=alpha", 45)?;
        wait_for_output(&buf, "argv[2]=beta", 45)?;
        // B10-1: a C-style program over frame-libc — crt0 (from the libc) parses
        // the argv stack and calls main, which echoes via the libc's write. Same
        // entry path a tcc-compiled C program will take.
        eprintln!("console-test: typing `/bin/cmain one two`");
        stdin
            .write_all(b"/bin/cmain one two\n")
            .context("write cmain")?;
        stdin.flush().ok();
        wait_for_output(&buf, "cmain: hello from frame-libc; argc=3", 45)?;
        wait_for_output(&buf, "argv[2]=two", 45)?;
        // B10-3a: printf via the format-scanner FSM — conversions + padding.
        wait_for_output(
            &buf,
            "cmain: d=-42 u=42 x=ff X=FF c=Q s=world p=0xdead pad=[    7][7    ][00007] pct=%",
            20,
        )?;
        // B11-1: the C-ABI variadic printf/fprintf (real varargs).
        wait_for_output(&buf, "cmain: va printf d=-7 x=beef s=hi c=Z", 45)?;
        wait_for_output(&buf, "cmain: va fprintf 20+22=42", 45)?;
        // B10-3b: buffered FILE* streams (fopen/fprintf/fwrite/fread/feof).
        wait_for_output(&buf, "cmain: fprintf to stdout: 2+3=5", 45)?;
        wait_for_output(&buf, "cmain: FILE* write/read/feof ok", 45)?;
        // B10-4: line input via fgets.
        wait_for_output(&buf, "cmain: fgets line-by-line ok", 45)?;
        // B10-2: the libc heap (malloc/realloc/free over brk).
        wait_for_output(&buf, "cmain: malloc/realloc/free ok", 45)?;
        // B11-3 follow-up: real wall clock. time() reads the CMOS RTC (pinned to
        // 2026-05-24T12:00:00 by `-rtc base=`), localtime() breaks it down. Assert
        // the date + hour prefix (boot consumes only seconds of VM time, so the
        // hour stays 12); the minute/second tail is left unasserted.
        wait_for_output(&buf, "cmain: clock 2026-05-24 12:", 45)?;
        // B11-3 follow-up: per-process cwd. getcwd/chdir (absolute, relative,
        // ".."), a failing chdir, and a relative fopen that honors the cwd.
        wait_for_output(&buf, "cmain: cwd chdir/getcwd + relative open ok", 45)?;
        // B11-2: a real C program (gcc-compiled, linked against frame-libc) runs.
        eprintln!("console-test: typing `/bin/chello`");
        stdin.write_all(b"/bin/chello\n").context("write chello")?;
        stdin.flush().ok();
        wait_for_output(&buf, "chello: hello from C on Frame OS! argc=1", 45)?;
        wait_for_output(&buf, "chello: malloc buf = ABCDEFGHIJ", 45)?;
        wait_for_output(&buf, "chello: read back: C wrote this: 1234", 45)?;
        wait_for_output(&buf, "chello: done", 45)?;
        // B11-3d: the on-device C compiler runs at all. `tcc -v` prints its
        // version banner — proof tcc's crt0 + libc startup + arg handling work.
        eprintln!("console-test: typing `/bin/tcc -v`");
        stdin.write_all(b"/bin/tcc -v\n").context("write tcc -v")?;
        stdin.flush().ok();
        wait_for_output(&buf, "tcc version 0.9.27", 45)?;
        // B11-3d: the full on-device compile + link + exec. tcc reads /hello.c
        // (`#include <stdio.h>` from the staged /usr/include), links it with
        // crt1.o + the C-shim libc.a from the /usr sysroot (gcc -fno-pic +
        // -fvisibility=hidden → no GOT, no PLT, only relocations tcc applies
        // correctly), writes /out.elf — then the shell execs that freshly
        // compiled program. Both lines are written back-to-back; ish's
        // fork+exec+wait serializes them (it reads the run line only after tcc
        // exits). The printf line proves compile+link+exec end-to-end.
        eprintln!("console-test: typing `/bin/tcc … /hello.c -o /out.elf` then `/out.elf`");
        stdin
            .write_all(b"/bin/tcc -B/usr/lib/tcc -static /hello.c -o /out.elf\n/out.elf\n")
            .context("write tcc compile + run")?;
        stdin.flush().ok();
        wait_for_output(&buf, "hello from a tcc-compiled program on Frame OS!", 90)?;
        // B11-3e: the BuildDriver Frame FSM drives the whole pipeline. `/bin/build`
        // runs compile→link→run (tcc -c, tcc -static, then exec /out.elf) as an
        // explicit state machine with a $Failed sink; the compiled program prints
        // its message and exits 7, which the driver reports. Proves the Frame-owned
        // toolchain lifecycle end to end.
        eprintln!("console-test: typing `/bin/buildc`");
        stdin.write_all(b"/bin/buildc\n").context("write buildc")?;
        stdin.flush().ok();
        // No-arg buildc defaults to /hello.c; output path is now derived
        // (/hello.c -> /hello.elf), proving the BuildDriver pipeline end to end.
        wait_for_output(
            &buf,
            "[build] pipeline ok; /hello.elf exited with code 7",
            90,
        )?;
        // B11-3 follow-up: `buildc <src>` takes the source path from argv. Build
        // a *different* source (/hi.c -> /hi.elf, exit 3) to prove it's argv, not
        // a hardcoded /hello.c.
        eprintln!("console-test: typing `/bin/buildc /hi.c`");
        stdin
            .write_all(b"/bin/buildc /hi.c\n")
            .context("write buildc /hi.c")?;
        stdin.flush().ok();
        wait_for_output(&buf, "[build] pipeline ok; /hi.elf exited with code 3", 90)?;
        // V1.0 CAPSTONE: one Frame system (frame/hello.frs) → both backends, both
        // run from the shell. (1) The Rust half: /bin/fhello drives the Hello FSM
        // generated by `framec -l rust`.
        eprintln!("console-test: typing `/bin/fhello`  (Frame→Rust)");
        stdin.write_all(b"/bin/fhello\n").context("write fhello")?;
        stdin.flush().ok();
        wait_for_output(
            &buf,
            "fhello: hello from a Frame system, transpiled to Rust!",
            20,
        )?;
        // (2) The C half: buildc compiles /fhello.c — the SAME hello.frs run
        // through `framec -l c` — with the on-device tcc, then runs it. Its
        // message proves the framec-generated C compiled + ran; buildc reports the
        // clean exit.
        eprintln!("console-test: typing `/bin/buildc /fhello.c`  (Frame→C→on-device tcc)");
        stdin
            .write_all(b"/bin/buildc /fhello.c\n")
            .context("write buildc /fhello.c")?;
        stdin.flush().ok();
        wait_for_output(
            &buf,
            "fhello: hello from a Frame system, transpiled to C!",
            120,
        )?;
        wait_for_output(
            &buf,
            "[build] pipeline ok; /fhello.elf exited with code 0",
            120,
        )?;
        // B11-3 follow-up: assert() is a real abort, not a no-op. Compile and run
        // /assert.c (a deliberately-false assert); __assert_fail prints the
        // diagnostic to the console and abort()s before the program's printf, so
        // the "Assertion `...' failed." line is the proof (the "unreachable"
        // printf never appears).
        eprintln!("console-test: typing `/bin/tcc … /assert.c -o /assert.elf` then `/assert.elf`");
        stdin
            .write_all(b"/bin/tcc -B/usr/lib/tcc -static /assert.c -o /assert.elf\n/assert.elf\n")
            .context("write tcc assert compile + run")?;
        stdin.flush().ok();
        wait_for_output(
            &buf,
            "/assert.c:15: main: Assertion `answer == 42' failed.",
            90,
        )?;
        eprintln!("console-test: C program ran on Frame OS; typing `exit`");
        stdin.write_all(b"exit\n").context("write exit")?;
        stdin.flush().ok();
        Ok(())
    })();

    // Bounded wait for QEMU to exit (the shell's `exit` → halt → isa-debug-exit).
    // Polls `try_wait` so this NEVER hangs: on a drive failure or a timeout we
    // kill QEMU rather than block forever. Generous (60s) because the dev image
    // runs on arm64 and emulates x86_64 via TCG — the final halt + OVMF/QEMU
    // teardown is slow there, and a 20s budget spuriously failed an otherwise
    // fully-passing run (every functional needle already matched).
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut status = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if drive.is_err() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = reader.join();
    let captured = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();

    // Surface a drive failure (its message includes the capture) after cleanup.
    drive?;

    if !captured.contains("hello from ELF") {
        bail!("console-test: missing hello output. Captured:\n---\n{captured}\n---");
    }
    match status {
        Some(s) if s.success() || s.code() == Some(33) => {}
        Some(s) => bail!("console-test: qemu exited with status {s}"),
        None => bail!(
            "console-test: qemu did not exit after `exit` (killed). Captured:\n---\n{captured}\n---"
        ),
    }
    eprintln!("console-test: PASS — typed a command and ran a program from the shell");
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

    tap_setup().context(
        "TAP setup failed — run via `TAP=1 docker/run.sh` (needs NET_ADMIN + /dev/net/tun)",
    )?;
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
        if child
            .try_wait()
            .context("failed to poll qemu process")?
            .is_some()
        {
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
        // Concurrent exec — the regression guard for the per-exec scratch buffers
        // (the shared ELF_BUF/ARGV_BUF race, same class as the B11 trap-frame
        // race). `coexec` forks two children that `exec_argv` *different* programs
        // from disk *at the same time*: child A → /bin/hello, child B →
        // /bin/argtest "Z". `exec` does a blocking virtio read that yields, so the
        // two loads interleave — each must read its ELF (and its argv) into its
        // own buffer or one child's read clobbers the other's image. The proof is
        // `argtest`'s "argv[1]=Z": it can only appear if child B loaded argtest
        // *and* its argv survived child A's concurrent exec uncorrupted (a clobber
        // or a swap would lose it). The parent then reaps both ("coexec: all
        // done"), and no process faulted. With the old shared statics this load
        // would race; with per-exec heap buffers both programs load cleanly.
        name: "concurrent_exec_buffers",
        expect_contains: &["[elf] loaded coexec", "argv[1]=Z", "coexec: all done"],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B9-1: the growable heap. `brktest` queries its program break, then
        // grows the heap by 1 MiB via the `brk` syscall (#10) — the kernel
        // demand-maps fresh USER|WRITABLE pages into the process's own address
        // space, well beyond the fixed 64 KiB program-image heap. The program
        // then writes a distinct value to every u64 across the new megabyte and
        // reads it all back; the "write/read-back ok" line only prints if every
        // freshly mapped page is real, writable, private memory. This is the
        // keystone for the on-device toolchains (B10+), which need MBs of heap.
        name: "brk_growable_heap_b9",
        expect_contains: &["brk: base 0x", "grew heap by 1024 KiB, write/read-back ok"],
        expect_absent: &[
            "brk: grow FAILED",
            "brk: VERIFY MISMATCH",
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
        ],
        timeout_secs: 20,
    },
    SmokeTest {
        // B9-3: the file write path. `fwtest` opens /tmp.txt for writing
        // (create+truncate), writes "Hello, Frame OS!", overwrites the middle
        // via lseek (random-access write_at), fstat's the size, then reopens for
        // reading and verifies the bytes — including a seek-and-read of the
        // overwritten region and a dup'd descriptor sharing the offset. The
        // "all ok" line only prints if write / lseek / fstat / dup / read all
        // round-trip through the on-disk filesystem. These are the syscalls a
        // libc/toolchain (B10+) needs to write its output and stat its inputs.
        name: "file_write_roundtrip_b9",
        expect_contains: &[
            "fwtest: wrote 16 bytes, fstat=16",
            "fwtest: unlink removed /tmp.txt (reopen fails): ok",
            "fwtest: all ok",
        ],
        expect_absent: &["FAIL:", "KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
    SmokeTest {
        // B11-3a: FPU/SSE state preserved across context switches. `fputest`
        // forks two processes that each pin distinct sentinels into xmm0..xmm7
        // and verify, over thousands of preemption windows, that their registers
        // survive — which only holds if the scheduler FXSAVEs/FXRSTORs the FPU on
        // every switch. Both must PASS; a clobber prints "FAIL". This is the
        // foundation for the on-device C toolchain (B11-3), whose compiled code
        // and tcc itself use SSE/x87.
        name: "fpu_preempt_b11",
        expect_contains: &["fputest: child PASS", "fputest: parent PASS"],
        expect_absent: &[
            "fputest: child FAIL",
            "fputest: parent FAIL",
            "xmm clobbered",
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
        ],
        timeout_secs: 30,
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
            // B9.5: a 128 KiB file round-trips through the double-indirect tier.
            "[fs] big file (128 KiB, double-indirect) round-trip: ok",
        ],
        expect_absent: &[
            "[fs] big file round-trip FAILED",
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
        ],
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
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
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
        expect_contains: &["[tcp] established", "[tcp] echoed 18 bytes", "[tcp] closed"],
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
        // R2a: per-event allocation at scale. 16 TcpConnection FSM instances are
        // created on the real kernel heap and driven through a full lifecycle
        // (7 dispatches each = 112), with a counting allocator measuring the heap
        // allocations — quantifying Frame's per-event allocation cost. All 16 must
        // transition correctly to $Closed with no OOM. (The alloc *count* depends
        // on the framec version, so the oracle pins the deterministic dispatch +
        // closed counts.)
        name: "tcp_scale_alloc_b5",
        expect_contains: &[
            "[tcp] scale: 16 conns, 112 dispatches,",
            "allocs/dispatch, closed 16/16 connections",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // R2b: live multi-connection TCP server. The kernel listens on :7–:10
        // (a connection table — one TcpConnection instance per port); the harness
        // opens all four simultaneously and echoes on each. All four must
        // handshake, echo, and close — four live FSM instances with independent
        // sequence state, validated by the harness reading each echo back AND the
        // kernel reporting "served 4 connections".
        name: "tcp_multi_conn_b5",
        expect_contains: &["[tcp] served 4 connections"],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // B6 Step 1: xHCI USB host-controller bring-up. The kernel discovers the
        // qemu-xhci controller (PCI class 0C0330), maps its MMIO window, resets
        // it, stands up the DCBAA/command-ring/event-ring, sets Run, and detects
        // the attached usb-kbd connected on a port.
        name: "usb_controller_b6",
        expect_contains: &["[usb] xHCI running", "[usb] device connected on port"],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // B6 Step 2: the HubPort Frame system drives the connected port through
        // connect → reset (a timed transition: PORTSC.PR + a settle deadline) →
        // enabled. The keyboard lands on port 5 in this qemu-xhci/q35 config.
        name: "usb_port_reset_b6",
        expect_contains: &["[usb] resetting port 5", "[usb] port 5 enabled"],
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
        // R3a: multi-port concurrent enumeration. Two HID devices (usb-kbd on
        // port 5, usb-mouse on port 6) are brought up *concurrently* — a HubPort
        // + UsbEnumeration instance per device coexist in the device table, and a
        // single driver loop demuxes each xHCI completion to the right instance by
        // slot (the connection-table pattern, now on real async hardware events).
        // Both reach $Configured (slots 1 and 2). The "orthogonal regions" /
        // many-concurrent-same-type-lifecycle-FSMs question, answered on hardware.
        name: "usb_multiport_r3a",
        expect_contains: &[
            "[usb] device configured (slot 1)",
            "[usb] device configured (slot 2)",
            "[usb] device configured (slot 3)",
            "[usb] enumerated 3 of 3 devices",
            // Classification order follows the device table (port order): the
            // USB3 mass-storage device sorts onto a lower port than the USB2 HID
            // devices, so it is slot 1, then keyboard, then mouse.
            "is mass storage",
            "is HID keyboard",
            "is HID mouse",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[usb] command failed during enumeration",
            "[usb] control transfer failed during enumeration",
            "[usb] enable slot timed out",
        ],
        timeout_secs: 30,
    },
    SmokeTest {
        // R3b: USB mass storage over Bulk-Only Transport + SCSI. The kernel
        // configures the storage device's bulk IN/OUT endpoints, then runs three
        // SCSI commands (INQUIRY, READ CAPACITY(10), READ(10) of block 0), each
        // through one UsbMsd Frame instance's CBW → data → CSW phase lifecycle.
        // A genuinely new device class + transfer type (bulk, not interrupt-IN).
        // The block-0 magic ("FRAMEOS!") proves a real media read.
        name: "usb_msd_r3b",
        expect_contains: &[
            "[usb] bulk endpoints configured (IN + OUT)",
            "[msd] INQUIRY vendor 'QEMU",
            "[msd] capacity: 128 blocks of 512 bytes",
            "[msd] block 0 first 8 bytes: FRAMEOS!",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "[msd] configure bulk endpoints failed",
            "[msd] command did not complete",
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
    SmokeTest {
        // B7 Step 5: TLB shootdown. The BSP unmaps a test page (flushing its own
        // TLB) and IPIs the other cores (the APs, idling interrupt-enabled) to
        // flush theirs; each core's shootdown ISR invlpg's the VA + acks. The BSP
        // waits for every core to ack — the barrier that makes it safe to reuse
        // the page. "ack barrier: ok" means all APs flushed.
        name: "smp_tlb_shootdown_b7",
        expect_contains: &[
            "[smp] TLB shootdown: 3 of 3 cores flushed",
            "[smp] TLB shootdown ack barrier: ok (safe to reuse page)",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // R1a: per-core Frame schedulers driven by cross-core posts. Each AP owns
        // its own Scheduler Frame instance; the BSP posts 3 task_ready + 3
        // task_unready into each core's MPSC queue, and that core drains them into
        // its Scheduler — which goes $Idle -> $Active (peak 3 runnable) -> $Idle.
        // Puts the B7 cross-core-post finding under N real Scheduler instances;
        // each instance stays pinned to its core (only SchedPost data crosses).
        name: "smp_percpu_sched_b7",
        expect_contains: &[
            "[smp] core 1 Frame scheduler: peak 3 runnable, ended idle=true",
            "[smp] core 3 Frame scheduler: peak 3 runnable, ended idle=true",
            "[smp] per-core Frame schedulers driven cross-core: ok",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // R1b: per-core context-switched execution. Each AP builds a run queue of
        // kernel threads and time-slices them under its own LAPIC timer, driving
        // its own Scheduler Frame instance through $Active -> $Idle as the workers
        // spawn and exit. Real preemptive multitasking per core: each of the 3 APs
        // runs all 3 workers to completion with multiple context switches and ends
        // $Idle. Confirms per-event allocation holds with N cores dispatching their
        // own Scheduler concurrently against the one shared, spin-locked heap.
        name: "smp_pcsched_r1b",
        expect_contains: &[
            "[r1b] core 1: sliced 3 threads",
            "[r1b] core 3: sliced 3 threads",
            "[r1b] per-core context-switched execution: ok",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 30,
    },
    SmokeTest {
        // R5a: nested-lock deadlock-avoidance stress. Every core acquires two
        // *ranked* locks in the documented global order (A rank 1 → B rank 2),
        // 20000 times, concurrently — nested locking beyond the B7 leaf-lock
        // stage. The counters end at exactly cores × 20000 (= 80000 with 4 cores)
        // on both locks iff every nested increment serialized with no lost update
        // and no deadlock. The SpinLock rank checker would have panicked at the
        // acquire had any core reversed the order (B → A).
        name: "smp_nested_lock_r5",
        expect_contains: &[
            "[smp] nested-lock stress: A=80000 B=80000 (expected 80000)",
            "[smp] nested-lock ordering: ok (no deadlock, no lost updates)",
        ],
        expect_absent: &[
            "KERNEL EXCEPTION",
            "KERNEL PANIC",
            "triple fault",
            "lock order violation",
        ],
        timeout_secs: 30,
    },
    SmokeTest {
        // R5b: per-CPU TSS + IST. Each core loads its own TSS (the BSP in
        // gdt::init, APs in load_on_ap), whose ist[0] points at that core's
        // double-fault stack; IDT vector 8 (#DF) routes through IST1. So a fault
        // on any core lands on a known-good per-core stack instead of escalating
        // to a triple fault. Each core verifies its own TR via `str`.
        name: "smp_percpu_tss_r5",
        expect_contains: &[
            "[smp] per-CPU TSS+IST: 4 of 4 cores armed (#DF -> IST1)",
            "[smp] per-CPU TSS+IST: ok",
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
/// R2b: the slirp `hostfwd` rules for the multi-connection server's extra ports
/// (:8/:9/:10 — :7 is `TCP_PROBE_PORT`). The kernel listens on :7–:10; the
/// multi-connection test opens one connection to each.
const MULTI_HOSTFWD: &str = "hostfwd=tcp::15585-:8,hostfwd=tcp::15586-:9,hostfwd=tcp::15587-:10";
/// The four host ports the multi-connection probe connects to → guest :7–:10.
const MULTI_HOST_PORTS: [u16; 4] = [TCP_PROBE_PORT, 15585, 15586, 15587];

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
    /// R2b: open four *simultaneous* connections (to guest :7–:10) and echo on
    /// each, exercising the kernel's connection table — four `TcpConnection`
    /// instances live at once, each with its own sequence state.
    Multi,
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
                if matches!(tcp_probe, TcpProbe::Handshake | TcpProbe::Echo) && !probed && serving {
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
                            TcpProbe::None | TcpProbe::Active | TcpProbe::Multi => {}
                        }
                    }
                }
                // R2b: open four *simultaneous* connections to guest :7–:10 and
                // echo on each, once all four listeners are up (the kernel logs
                // ":10" last). Connecting all four before reading exercises the
                // connection table with four live `TcpConnection` instances.
                if tcp_probe == TcpProbe::Multi && !probed {
                    let up = std::fs::read_to_string(serial_path)
                        .map(|s| s.contains("[tcp] listening on :10"))
                        .unwrap_or(false);
                    if up {
                        use std::io::{Read, Write};
                        let mut streams: Vec<std::net::TcpStream> = Vec::new();
                        for &port in &MULTI_HOST_PORTS {
                            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                            if let Ok(s) = std::net::TcpStream::connect_timeout(
                                &addr,
                                Duration::from_millis(500),
                            ) {
                                streams.push(s);
                            }
                        }
                        if streams.len() == MULTI_HOST_PORTS.len() {
                            // All four open simultaneously — now echo on each.
                            let mut ok = 0;
                            for mut s in streams {
                                s.set_read_timeout(Some(Duration::from_millis(2500))).ok();
                                let mut buf = vec![0u8; TCP_ECHO_REQUEST.len()];
                                if s.write_all(TCP_ECHO_REQUEST).is_ok()
                                    && s.read_exact(&mut buf).is_ok()
                                    && buf == TCP_ECHO_REQUEST
                                {
                                    ok += 1;
                                }
                            }
                            if ok == MULTI_HOST_PORTS.len() {
                                echo_ok = true;
                            } else {
                                eprintln!("    (multi: only {ok}/4 connections echoed)");
                            }
                            probed = true;
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

    // The echo / multi probes gate the test: the harness must have read its
    // request back on each connection (proving each connection's outbound data
    // segment — its own sequence + checksum — was well-formed).
    if matches!(tcp_probe, TcpProbe::Echo | TcpProbe::Multi) && !echo_ok {
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
        "tcp_multi_conn_b5" => TcpProbe::Multi,
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
/// Build the kernel, optionally with the `interactive` feature (B8). The output
/// path is the same; cargo rebuilds when the feature set changes.
fn build_kernel_features(workspace: &Path, interactive: bool) -> Result<PathBuf> {
    let mut args = vec![
        "build",
        "-p",
        "frame-os-kernel",
        "--target",
        "x86_64-unknown-none",
    ];
    if interactive {
        args.push("--features");
        args.push("interactive");
    }
    let status = Command::new("cargo")
        .current_dir(workspace)
        .args(&args)
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
        // Ubuntu 24.04+ `ovmf` package renamed the firmware to the 4 MB
        // variants and dropped the plain `OVMF_CODE.fd` — check these first so
        // CI (ubuntu-latest = Noble) finds them.
        ("/usr/share/OVMF", "OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"),
        // Older Ubuntu / Debian (incl. the bookworm dev container) `ovmf` package
        ("/usr/share/OVMF", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        // Fedora / Arch `edk2-ovmf` package (4M and legacy names)
        ("/usr/share/edk2/ovmf", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        ("/usr/share/edk2/x64", "OVMF_CODE.4m.fd", "OVMF_VARS.4m.fd"),
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
