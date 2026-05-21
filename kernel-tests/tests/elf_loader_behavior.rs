// kernel-tests/tests/elf_loader_behavior.rs
//
// Level 3 (behavioral) tests for the ElfLoader phase FSM, on the host. The
// `elf` test-double does real ELF64 header parsing (so corrupt/truncated cases
// are meaningful) but stubs the actual segment mapping. The phases cascade from
// construction, so each test prepares an image, constructs the loader, and
// inspects where it settled ($Done vs $Failed) + the error message.
//
// The system under test is the "$Failed funnel": every phase routes its own
// failure to one $Failed sink that runs cleanup.

use frame_os_kernel_tests::{elf, ElfLoader};

/// A minimal but valid ELF64 header (+ a 56-byte program header) that passes
/// read_header and validate_header. `magic1` is the second magic byte (corrupt
/// it to fail validation); `e_type` (16) and `machine_lo` (18) drive the other
/// validation checks. Entry is 0x1000_0000, phoff=64, phentsize=56, phnum=1.
const fn elf_with(e_type: u8, machine_lo: u8, magic1: u8) -> [u8; 120] {
    let mut e = [0u8; 120];
    e[0] = 0x7f;
    e[1] = magic1;
    e[2] = b'L';
    e[3] = b'F';
    e[4] = 2; // EI_CLASS = ELFCLASS64
    e[5] = 1; // EI_DATA  = ELFDATA2LSB
    e[6] = 1; // EI_VERSION
    e[16] = e_type; // e_type (2 = ET_EXEC)
    e[18] = machine_lo; // e_machine (0x3E = x86-64)
    e[27] = 0x10; // e_entry = 0x1000_0000 (LE: byte 27 = 0x10)
    e[32] = 64; // e_phoff = 64
    e[54] = 56; // e_phentsize = 56
    e[56] = 1; // e_phnum = 1
    e
}

static VALID: [u8; 120] = elf_with(2, 0x3E, b'E');
static BAD_MAGIC: [u8; 120] = elf_with(2, 0x3E, b'X');
static WRONG_MACHINE: [u8; 120] = elf_with(2, 0x00, b'E'); // not x86-64
static NON_EXEC: [u8; 120] = elf_with(3, 0x3E, b'E'); // ET_DYN, not ET_EXEC

// Too short to even read the header fields (phoff lives at offset 32..40).
static TRUNCATED: [u8; 32] = {
    let mut e = [0u8; 32];
    e[0] = 0x7f;
    e[1] = b'E';
    e[2] = b'L';
    e[3] = b'F';
    e[4] = 2;
    e[5] = 1;
    e
};

#[test]
fn valid_elf_loads_to_done() {
    elf::prepare(&VALID);
    let mut l = ElfLoader::__create();
    assert!(l.is_done(), "a valid ELF should reach $Done");
    assert!(!l.is_failed());
    assert_eq!(l.entry(), 0x1000_0000, "entry point parsed from the header");
    assert_ne!(l.user_stack_top(), 0, "a user stack was built");
}

#[test]
fn truncated_header_fails_in_reading() {
    elf::prepare(&TRUNCATED);
    let mut l = ElfLoader::__create();
    assert!(l.is_failed(), "a truncated header can't be read");
    assert!(!l.is_done());
    assert!(
        l.error().contains("truncated"),
        "error should name the truncated header, got: {:?}",
        l.error()
    );
}

#[test]
fn bad_magic_fails_validation() {
    elf::prepare(&BAD_MAGIC);
    let mut l = ElfLoader::__create();
    assert!(l.is_failed());
    assert!(
        l.error().contains("not a valid"),
        "error should flag an invalid executable, got: {:?}",
        l.error()
    );
}

#[test]
fn wrong_machine_fails_validation() {
    elf::prepare(&WRONG_MACHINE);
    let mut l = ElfLoader::__create();
    assert!(l.is_failed(), "a non-x86-64 ELF must be rejected");
}

#[test]
fn non_executable_type_fails_validation() {
    elf::prepare(&NON_EXEC);
    let mut l = ElfLoader::__create();
    assert!(l.is_failed(), "ET_DYN (not ET_EXEC) must be rejected");
}

#[test]
fn fresh_loader_on_valid_image_is_not_failed() {
    // Sanity: the default query answers before any override fire correctly
    // once settled — a valid image is done and not errored.
    elf::prepare(&VALID);
    let mut l = ElfLoader::__create();
    assert!(l.error().is_empty(), "no error on the success path");
}
