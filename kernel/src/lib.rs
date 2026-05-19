// kernel/src/lib.rs
//
// Frame OS — bare-metal kernel.
//
// This crate is intentionally empty at H0. The real content lands at B0, when
// the kernel boots in QEMU and prints a banner over serial.
//
// Until then this crate exists only to:
//   - Reserve its name in the workspace
//   - Provide a place to land bare-metal work without restructuring the
//     workspace later
//
// See docs/roadmap.md for the B0 milestone scope.

#![no_std]
