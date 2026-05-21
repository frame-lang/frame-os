// kernel/src/paging.rs
//
// 4-level x86_64 paging (B2 Step 2). Pure native — page-table manipulation.
//
// We extend the page tables Limine already set up (read CR3 for the active
// PML4) rather than building our own from scratch — far lower triple-fault
// risk. Tables are reached through Limine's HHDM: a table at physical
// address P is read/written at virtual `P + hhdm_offset` (see frames.rs).
//
// A virtual address splits into four 9-bit table indices + a 12-bit offset:
//   [47:39] PML4  [38:30] PDPT  [29:21] PD  [20:12] PT  [11:0] offset
//
// `map` walks PML4→PDPT→PD→PT, allocating + zeroing intermediate tables on
// demand, and writes the leaf PTE. `translate` walks read-only. `unmap`
// clears the leaf PTE. Single-threaded at B2 (init + the page-fault path,
// IF=0); a lock joins when a preemptible context maps (B3).

use core::arch::asm;

use crate::frames;

/// PTE flag: the entry is present (maps something).
pub const PRESENT: u64 = 1 << 0;
/// PTE flag: writable.
pub const WRITABLE: u64 = 1 << 1;

/// Mask selecting the physical frame address out of a page-table entry or
/// CR3 (bits 12..52).
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

fn read_cr3() -> u64 {
    let v: u64;
    unsafe {
        asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags));
    }
    v
}

fn flush_tlb(virt: u64) {
    unsafe {
        asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
    }
}

/// Pointer to entry `index` of the page table at physical address
/// `table_phys`, via the HHDM.
fn entry_ptr(table_phys: u64, index: u64) -> *mut u64 {
    let table = frames::phys_to_virt(table_phys) as *mut u64;
    unsafe { table.add(index as usize) }
}

/// Return the physical address of the next-level table under `table_phys`
/// at `index`, allocating + zeroing a fresh one (and installing it
/// present+writable) if not present.
///
/// # Safety
/// `table_phys` must be a valid page-table frame.
unsafe fn next_table(table_phys: u64, index: u64) -> u64 {
    let e = entry_ptr(table_phys, index);
    if *e & PRESENT == 0 {
        let frame = frames::alloc_frame().expect("out of frames building page table");
        // Zero the new table.
        let t = frames::phys_to_virt(frame) as *mut u64;
        let mut k = 0;
        while k < 512 {
            t.add(k).write(0);
            k += 1;
        }
        *e = (frame & ADDR_MASK) | PRESENT | WRITABLE;
        frame
    } else {
        *e & ADDR_MASK
    }
}

fn indices(virt: u64) -> (u64, u64, u64, u64) {
    (
        (virt >> 39) & 0x1FF,
        (virt >> 30) & 0x1FF,
        (virt >> 21) & 0x1FF,
        (virt >> 12) & 0x1FF,
    )
}

/// Map `virt` → `phys` with `flags` (PRESENT is added automatically).
///
/// # Safety
/// Changes the active address space. Caller must ensure `virt` isn't a
/// mapping in use, and `phys` is a valid frame. 4 KiB pages only.
pub unsafe fn map(virt: u64, phys: u64, flags: u64) {
    let (i4, i3, i2, i1) = indices(virt);
    let pml4 = read_cr3() & ADDR_MASK;
    let pdpt = next_table(pml4, i4);
    let pd = next_table(pdpt, i3);
    let pt = next_table(pd, i2);
    *entry_ptr(pt, i1) = (phys & ADDR_MASK) | flags | PRESENT;
    flush_tlb(virt);
}

/// Remove the mapping for `virt` (clears the leaf PTE). Intermediate tables
/// are left in place.
///
/// # Safety
/// Changes the active address space.
pub unsafe fn unmap(virt: u64) {
    let (i4, i3, i2, i1) = indices(virt);
    let pml4 = read_cr3() & ADDR_MASK;
    let e4 = entry_ptr(pml4, i4);
    if *e4 & PRESENT == 0 {
        return;
    }
    let e3 = entry_ptr(*e4 & ADDR_MASK, i3);
    if *e3 & PRESENT == 0 {
        return;
    }
    let e2 = entry_ptr(*e3 & ADDR_MASK, i2);
    if *e2 & PRESENT == 0 {
        return;
    }
    let e1 = entry_ptr(*e2 & ADDR_MASK, i1);
    *e1 = 0;
    flush_tlb(virt);
}

/// Translate a virtual address to its physical address, or None if unmapped.
pub fn translate(virt: u64) -> Option<u64> {
    let (i4, i3, i2, i1) = indices(virt);
    let pml4 = read_cr3() & ADDR_MASK;
    unsafe {
        let e4 = *entry_ptr(pml4, i4);
        if e4 & PRESENT == 0 {
            return None;
        }
        let e3 = *entry_ptr(e4 & ADDR_MASK, i3);
        if e3 & PRESENT == 0 {
            return None;
        }
        let e2 = *entry_ptr(e3 & ADDR_MASK, i2);
        if e2 & PRESENT == 0 {
            return None;
        }
        let e1 = *entry_ptr(e2 & ADDR_MASK, i1);
        if e1 & PRESENT == 0 {
            return None;
        }
        Some((e1 & ADDR_MASK) | (virt & 0xFFF))
    }
}
