// kernel/build.rs
//
// At B0 Step 1: tell rustc to use our custom linker script that lays out
// the kernel for Limine's higher-half load address. Future steps will
// extend this to also invoke framec on kernel/*.frs sources (the
// Kernel HSM at Step 2, SerialDriver at Step 3, etc.), mirroring the
// shell crate's build.rs pattern.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let linker_script = manifest.join("linker.ld");

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
}
