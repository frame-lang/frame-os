// shared/src/lib.rs
//
// Placeholder for types and traits used by both the hosted shell and the
// bare-metal kernel. Currently empty.
//
// The shared crate exists so that:
//   - When the kernel uses the same Frame system as the shell (e.g. Shell,
//     Parser at B2), the type lives here and both depend on it
//   - Cross-crate types (like a Command struct returned by the Parser) have
//     a clear home that isn't either shell or kernel
//
// Until B2, this crate has nothing in it. The module declaration below keeps
// rustc happy when the workspace builds.

#![no_std]
