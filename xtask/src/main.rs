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
    ("kernel.frs", "kernel.svg"),
    ("serial_driver.frs", "serial_driver.svg"),
    ("task.frs", "task.svg"),
    ("scheduler.frs", "scheduler.svg"),
    ("page_fault_handler.frs", "page_fault_handler.svg"),
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
    /// A writable copy of QEMU's read-only vars template. QEMU needs
    /// this writable because UEFI mutates NVRAM during boot.
    ovmf_vars: PathBuf,
}

fn prepare_qemu_artifacts(workspace: &Path) -> Result<QemuArtifacts> {
    let kernel_elf = build_kernel(workspace)?;
    let limine_dir = ensure_limine_binaries(workspace)?;
    let esp_img = build_esp_image(workspace, &kernel_elf, &limine_dir)?;

    let (ovmf_code, ovmf_vars_template) = find_ovmf()?;
    let ovmf_vars = workspace.join("target").join("qemu").join("ovmf-vars.fd");
    std::fs::create_dir_all(ovmf_vars.parent().unwrap())?;
    if !ovmf_vars.exists() {
        std::fs::copy(&ovmf_vars_template, &ovmf_vars)
            .with_context(|| format!("failed to copy {}", ovmf_vars_template.display()))?;
    }

    Ok(QemuArtifacts {
        esp_img,
        ovmf_code,
        ovmf_vars,
    })
}

/// Build a QEMU `Command` with the standard machine + firmware + disk
/// arguments. Callers add the serial routing they want
/// (`-serial stdio` for interactive, `-serial file:<path>` for smoke
/// tests) and then either spawn or run.
fn qemu_base_command(artifacts: &QemuArtifacts) -> Command {
    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args(["-machine", "q35", "-cpu", "qemu64", "-m", "256M"])
        // UEFI firmware (split into read-only code + writable NVRAM).
        .args(["-drive"])
        .arg(format!(
            "if=pflash,format=raw,readonly=on,file={}",
            artifacts.ovmf_code.display()
        ))
        .args(["-drive"])
        .arg(format!(
            "if=pflash,format=raw,file={}",
            artifacts.ovmf_vars.display()
        ))
        // Boot drive — real FAT image with our ESP layout.
        .args(["-drive"])
        .arg(format!("format=raw,file={}", artifacts.esp_img.display()))
        .args(["-display", "none"])
        // Don't reboot on triple fault; hold QEMU open after halt so
        // smoke tests can read the full serial log instead of racing
        // QEMU's exit.
        .args(["-no-reboot", "-no-shutdown"]);
    cmd
}

fn run_qemu() -> Result<()> {
    let workspace = workspace_root()?;
    let artifacts = prepare_qemu_artifacts(&workspace)?;

    eprintln!("booting kernel in QEMU (Ctrl-C or Ctrl-A x to quit)...");
    let mut cmd = qemu_base_command(&artifacts);
    cmd.args(["-serial", "stdio"]);
    let status = cmd
        .status()
        .context("failed to invoke qemu-system-x86_64")?;

    if !status.success() {
        bail!("qemu exited with status: {status}");
    }
    Ok(())
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
            "[#PF] FATAL unhandled fault at 0x0000600000000000",
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
        // B3 Step 1b: the user/kernel boundary. A hand-crafted ring-3
        // program writes 'A'/'B' via write_char syscalls and exits(42); the
        // exit syscall longjmps back to the kernel. "AB" proves syscalls
        // from ring 3 reach the kernel; the exit + back-in-kernel lines
        // prove sysret-less exit + the longjmp work. No exception/fault.
        name: "ring3_syscall_b3",
        expect_contains: &[
            "[user] entering ring 3",
            "AB",
            "[user] exited with code 42",
            "[user] back in kernel after user exit",
        ],
        expect_absent: &["KERNEL EXCEPTION", "KERNEL PANIC", "triple fault"],
        timeout_secs: 20,
    },
];

fn run_qemu_test() -> Result<()> {
    let workspace = workspace_root()?;
    let artifacts = prepare_qemu_artifacts(&workspace)?;

    let serial_dir = workspace.join("target").join("qemu-smoke");
    std::fs::create_dir_all(&serial_dir)
        .with_context(|| format!("failed to create {}", serial_dir.display()))?;

    let mut failures: Vec<String> = Vec::new();
    let total = SMOKE_TESTS.len();

    for test in SMOKE_TESTS {
        eprintln!("smoke: {} ...", test.name);
        let serial_path = serial_dir.join(format!("{}.log", test.name));
        // Truncate any previous run's log; QEMU appends with `-serial
        // file:`, which we want fresh per run.
        if serial_path.exists() {
            std::fs::remove_file(&serial_path).with_context(|| {
                format!("failed to clear previous log {}", serial_path.display())
            })?;
        }

        match run_smoke_test(test, &artifacts, &serial_path) {
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

fn run_smoke_test(test: &SmokeTest, artifacts: &QemuArtifacts, serial_path: &Path) -> Result<()> {
    // Spawn QEMU with serial routed to a file so we can read it after
    // the timeout. `-display none` (set in qemu_base_command) keeps it
    // headless. We discard QEMU's own stdout/stderr — the interesting
    // stream is the captured serial output.
    let mut cmd = qemu_base_command(artifacts);
    cmd.args(["-serial"])
        .arg(format!("file:{}", serial_path.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());

    let mut child = cmd.spawn().context("failed to invoke qemu-system-x86_64")?;

    // Poll for QEMU's natural exit, otherwise force-kill at timeout.
    // The kernel sits in `hlt` forever at B0 Step 1 so the natural
    // exit basically never happens — but checking anyway lets us
    // shorten future tests once `isa-debug-exit` is wired up.
    let deadline = Instant::now() + Duration::from_secs(test.timeout_secs);
    loop {
        match child.try_wait().context("failed to poll qemu process")? {
            Some(status) => {
                if !status.success() {
                    return Err(anyhow!("qemu exited non-zero: {status}"));
                }
                break;
            }
            None => {
                if Instant::now() >= deadline {
                    // Force-kill QEMU. SIGKILL guarantees exit even if
                    // QEMU's monitor is wedged; we don't need a clean
                    // shutdown because we already captured serial to
                    // file synchronously as the kernel wrote it.
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

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
