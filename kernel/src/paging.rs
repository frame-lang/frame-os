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
/// PTE flag: user-accessible (ring 3). Required on the leaf *and* every
/// intermediate table on the walk for a ring-3 access to succeed.
pub const USER: u64 = 1 << 2;

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
        // Intermediate tables are present + writable + user. The leaf PTE's
        // USER bit is what actually gates ring-3 access (a kernel leaf with
        // USER clear is still protected); permissive intermediates just let
        // user *leaves* underneath them be reachable.
        *e = (frame & ADDR_MASK) | PRESENT | WRITABLE | USER;
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

/// Physical address of the active PML4 (the current address space handle).
pub fn current_pml4() -> u64 {
    read_cr3() & ADDR_MASK
}

/// Map `virt` → `phys` with `flags` in the address space rooted at
/// `pml4` (a PML4 physical address). PRESENT is added automatically.
///
/// # Safety
/// Mutates the given address space. `pml4` must be a valid PML4 frame and
/// `phys` a valid frame. 4 KiB pages only.
pub unsafe fn map_in(pml4: u64, virt: u64, phys: u64, flags: u64) {
    let (i4, i3, i2, i1) = indices(virt);
    let pdpt = next_table(pml4, i4);
    let pd = next_table(pdpt, i3);
    let pt = next_table(pd, i2);
    *entry_ptr(pt, i1) = (phys & ADDR_MASK) | flags | PRESENT;
    // Only the active space has live TLB entries to flush.
    if pml4 == current_pml4() {
        flush_tlb(virt);
    }
}

/// Map `virt` → `phys` with `flags` in the active address space.
///
/// # Safety
/// As `map_in`, on the active space.
pub unsafe fn map(virt: u64, phys: u64, flags: u64) {
    map_in(current_pml4(), virt, phys, flags);
}

/// Build a fresh address space: a new PML4 whose lower half (user space) is
/// empty and whose higher half (kernel + HHDM, indices 256..512) mirrors
/// the current PML4 — so the kernel stays mapped after a `switch`. Returns
/// the new PML4's physical address.
///
/// # Safety
/// Allocates a frame; the result is only safe to `switch` to while the
/// kernel higher-half it copied remains valid (always, at B2).
pub unsafe fn new_address_space() -> u64 {
    let frame = frames::alloc_frame().expect("out of frames for new address space");
    let new_pml4 = frames::phys_to_virt(frame) as *mut u64;
    let cur_pml4 = frames::phys_to_virt(current_pml4()) as *const u64;
    let mut i = 0;
    while i < 256 {
        new_pml4.add(i).write(0); // lower half: empty user space
        i += 1;
    }
    while i < 512 {
        new_pml4.add(i).write(*cur_pml4.add(i)); // higher half: kernel + HHDM
        i += 1;
    }
    frame
}

/// Build a child address space that eager-copies `parent_pml4`'s *user* space:
/// the kernel higher-half is mirrored (shared), and every present user page
/// (PML4 indices 0..256) is duplicated into a fresh frame with the same
/// contents + flags. Returns the child PML4 physical address. Used by `fork`.
/// (Eager copy now; copy-on-write is a later optimization.)
///
/// # Safety
/// `parent_pml4` must be the active address space (so `new_address_space`'s
/// higher-half mirror is the parent's kernel mapping). Allocates frames.
pub unsafe fn fork_address_space(parent_pml4: u64) -> u64 {
    let child = new_address_space(); // higher-half mirrored from the parent
    let parent = frames::phys_to_virt(parent_pml4) as *const u64;
    for i4 in 0..256u64 {
        let e4 = *parent.add(i4 as usize);
        if e4 & PRESENT == 0 {
            continue;
        }
        let pdpt = frames::phys_to_virt(e4 & ADDR_MASK) as *const u64;
        for i3 in 0..512u64 {
            let e3 = *pdpt.add(i3 as usize);
            if e3 & PRESENT == 0 {
                continue;
            }
            let pd = frames::phys_to_virt(e3 & ADDR_MASK) as *const u64;
            for i2 in 0..512u64 {
                let e2 = *pd.add(i2 as usize);
                // Skip absent and 2 MiB large pages (we only map 4 KiB in user
                // space, so large pages never appear here).
                if e2 & PRESENT == 0 || e2 & (1 << 7) != 0 {
                    continue;
                }
                let pt = frames::phys_to_virt(e2 & ADDR_MASK) as *const u64;
                for i1 in 0..512u64 {
                    let e1 = *pt.add(i1 as usize);
                    if e1 & PRESENT == 0 {
                        continue;
                    }
                    let va = (i4 << 39) | (i3 << 30) | (i2 << 21) | (i1 << 12);
                    let dst_phys = frames::alloc_frame().expect("out of frames in fork");
                    core::ptr::copy_nonoverlapping(
                        frames::phys_to_virt(e1 & ADDR_MASK),
                        frames::phys_to_virt(dst_phys),
                        4096,
                    );
                    map_in(child, va, dst_phys, e1 & (WRITABLE | USER));
                }
            }
        }
    }
    child
}

/// Free an address space's *user* half (B3 Step 5d teardown): every user leaf
/// frame, the user page-table frames (PT/PD/PDPT under PML4 indices 0..256),
/// and the PML4 frame itself. The shared kernel higher-half (256..512) is left
/// untouched. Call only on a space no longer active (CR3 points elsewhere).
///
/// # Safety
/// `pml4` must be a PML4 not currently loaded in CR3, with a private user half
/// (as produced by `new_address_space` / `fork_address_space`).
pub unsafe fn free_address_space(pml4: u64) {
    let p4 = frames::phys_to_virt(pml4) as *const u64;
    for i4 in 0..256u64 {
        let e4 = *p4.add(i4 as usize);
        if e4 & PRESENT == 0 {
            continue;
        }
        let pdpt_phys = e4 & ADDR_MASK;
        let pdpt = frames::phys_to_virt(pdpt_phys) as *const u64;
        for i3 in 0..512u64 {
            let e3 = *pdpt.add(i3 as usize);
            if e3 & PRESENT == 0 {
                continue;
            }
            let pd_phys = e3 & ADDR_MASK;
            let pd = frames::phys_to_virt(pd_phys) as *const u64;
            for i2 in 0..512u64 {
                let e2 = *pd.add(i2 as usize);
                if e2 & PRESENT == 0 || e2 & (1 << 7) != 0 {
                    continue;
                }
                let pt_phys = e2 & ADDR_MASK;
                let pt = frames::phys_to_virt(pt_phys) as *const u64;
                for i1 in 0..512u64 {
                    let e1 = *pt.add(i1 as usize);
                    if e1 & PRESENT != 0 {
                        frames::free_frame(e1 & ADDR_MASK); // user leaf page
                    }
                }
                frames::free_frame(pt_phys);
            }
            frames::free_frame(pd_phys);
        }
        frames::free_frame(pdpt_phys);
    }
    frames::free_frame(pml4);
}

/// Switch the active address space (load CR3). Flushes the whole TLB.
///
/// # Safety
/// `pml4_phys` must root an address space that maps the currently-executing
/// code, the stack, and the HHDM — else the next instruction faults.
pub unsafe fn switch(pml4_phys: u64) {
    asm!("mov cr3, {}", in(reg) pml4_phys, options(nostack, preserves_flags));
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
