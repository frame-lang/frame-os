// kernel/src/arch/aarch64/fdt.rs
//
// Minimal flattened-device-tree (FDT / "dtb") reader (B-HAL.3.3). QEMU's `virt`
// machine passes a pointer to the FDT blob in x0 at boot; `_start` preserves it
// and `kmain` hands it here. This is the AArch64 half of the `Boot` contract's
// "give me a memory map" — the x86 side reads Limine's memory-map response;
// here we read the `/memory` node's `reg` (RAM base + size) from the FDT.
//
// The FDT is big-endian. Layout: a header (magic, totalsize, struct/strings
// offsets), then a *structure block* of tokens — FDT_BEGIN_NODE (name),
// FDT_PROP (len, name-offset-into-strings, value), FDT_END_NODE, FDT_NOP,
// FDT_END — and a *strings block* holding the property names.
//
// Scope note: this assumes `#address-cells = #size-cells = 2` (the QEMU `virt`
// default), so a `/memory` `reg` is base:u64, size:u64. A general reader would
// track the cell counts from the parent node; that's deferred until a board
// needs it. (This parser is also the natural first `@@fsm` target — the struct
// block is a token stream — when that construct is folded in.)

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// Defensive cap on the number of struct-block tokens walked (a well-formed
/// QEMU DTB is far smaller); prevents a runaway read on a malformed blob.
const MAX_TOKENS: usize = 100_000;

unsafe fn be32(p: *const u8) -> u32 {
    u32::from_be_bytes([*p, *p.add(1), *p.add(2), *p.add(3)])
}

unsafe fn be64(p: *const u8) -> u64 {
    ((be32(p) as u64) << 32) | (be32(p.add(4)) as u64)
}

/// Whether `dtb` points at a valid FDT (magic matches).
///
/// # Safety
/// `dtb` must point at readable memory of at least the FDT header size.
pub unsafe fn valid(dtb: *const u8) -> bool {
    be32(dtb) == FDT_MAGIC
}

/// The FDT's declared total size (header `totalsize`).
///
/// # Safety
/// As [`valid`].
pub unsafe fn total_size(dtb: *const u8) -> u32 {
    be32(dtb.add(4))
}

/// Scan `[start, start + len)` (8-byte stride — the DTB is 8-byte aligned) for
/// the FDT magic and return the first hit, or `None`. QEMU `virt` always loads a
/// DTB into RAM but, for a bare `-kernel <ELF>` entered at its entry, doesn't
/// pass its address in x0 — so we locate it by scanning the RAM window. `len`
/// must stay within mapped RAM (reads past the end fault with the MMU off).
///
/// # Safety
/// `[start, start + len)` must be readable memory.
pub unsafe fn find(start: usize, len: usize) -> Option<*const u8> {
    let mut a = start;
    let end = start + len;
    while a < end {
        let p = a as *const u8;
        if be32(p) == FDT_MAGIC {
            return Some(p);
        }
        a += 8;
    }
    None
}

/// Find the `/memory` node's `reg` and return `(base, size)`, or `None` if the
/// blob is invalid or has no `/memory` node.
///
/// # Safety
/// `dtb` must point at a valid FDT blob for its whole `totalsize`.
pub unsafe fn memory_region(dtb: *const u8) -> Option<(u64, u64)> {
    if !valid(dtb) {
        return None;
    }
    let off_struct = be32(dtb.add(8)) as usize;
    let off_strings = be32(dtb.add(12)) as usize;
    let strings = dtb.add(off_strings);
    let mut p = dtb.add(off_struct);

    // Depth of nesting under a `/memory` node (so a `reg` on `/memory` itself,
    // not on a grandchild, is the one we read). `> 0` means "inside /memory".
    let mut memory_depth: i32 = -1; // -1 = not in /memory
    let mut depth: i32 = 0;

    for _ in 0..MAX_TOKENS {
        let tok = be32(p);
        p = p.add(4);
        match tok {
            FDT_BEGIN_NODE => {
                let name = p;
                let mut len = 0usize;
                while *name.add(len) != 0 {
                    len += 1;
                }
                // A node named "memory" or "memory@<addr>".
                let is_memory = len >= 6
                    && *name == b'm'
                    && *name.add(1) == b'e'
                    && *name.add(2) == b'm'
                    && *name.add(3) == b'o'
                    && *name.add(4) == b'r'
                    && *name.add(5) == b'y';
                if is_memory && memory_depth < 0 {
                    memory_depth = depth;
                }
                depth += 1;
                p = p.add((len + 1 + 3) & !3); // name + NUL, padded to 4
            }
            FDT_END_NODE => {
                depth -= 1;
                if memory_depth >= 0 && depth <= memory_depth {
                    memory_depth = -1; // left the /memory node
                }
            }
            FDT_PROP => {
                let len = be32(p) as usize;
                let nameoff = be32(p.add(4)) as usize;
                let val = p.add(8);
                let nm = strings.add(nameoff);
                let is_reg =
                    *nm == b'r' && *nm.add(1) == b'e' && *nm.add(2) == b'g' && *nm.add(3) == 0;
                // Read `reg` only when directly inside /memory (depth just below).
                if memory_depth >= 0 && depth == memory_depth + 1 && is_reg && len >= 16 {
                    return Some((be64(val), be64(val.add(8))));
                }
                p = val.add((len + 3) & !3); // value, padded to 4
            }
            FDT_NOP => {}
            FDT_END => return None,
            _ => return None, // unknown token — malformed
        }
    }
    None
}
