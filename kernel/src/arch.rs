// kernel/src/arch.rs
//
// Per-architecture HAL implementations (B-HAL.1). Each submodule here provides
// the *mechanism* behind the traits in `hal.rs` for one ISA, and is selected at
// **build time** by `cfg(target_arch)` — exactly one is compiled into any given
// binary. The platform-agnostic kernel never names these modules directly; it
// goes through `hal::console()` / `hal::cpu()` / … which forward to the active
// `arch::<isa>` impl.
//
// Today only x86_64 exists. A future AArch64 port adds `arch/aarch64/` here,
// `cfg`-gated the same way; nothing above the HAL changes.

#[cfg(target_arch = "x86_64")]
pub mod x86_64;
