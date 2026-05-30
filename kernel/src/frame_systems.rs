// kernel/src/frame_systems.rs
//
// Pulls in the Rust code framec generates from the kernel's .frs sources
// (written to OUT_DIR by build.rs) and makes the systems available to the
// rest of the crate.
//
// Mirror of shell/src/frame_systems.rs, with one extra wrinkle: framec's
// generated code refers to `String`, `Vec`, `Box`, and `to_string`
// unqualified (it expects them from the std prelude). The kernel is
// no_std, so those names aren't automatically in scope. We re-export them
// from `alloc` here so the generated `mod _<sys>_framec { use super::*; }`
// wrappers pick them up via their glob import.
//
// (`Rc`, `format!`, and `vec!` don't need re-exporting: the generated
// code uses fully-qualified `alloc::rc::Rc` and the wrappers import
// `alloc::{vec, format}` themselves.)
//
// As of B-HAL.4.4 the systems are split into two tiers:
//   - the *pure* ones — actions are heap-typed arithmetic + (at most)
//     `serial::*` calls — which compile on every arch the kernel targets;
//   - the *x86-only* ones — actions reach into kernel modules currently
//     only on the x86 boot path (vm, usermode, elf, net, xhci, …) — which
//     stay gated to `target_arch = "x86_64"` until those subsystems
//     themselves grow aarch64 legs.
// The two tiers live in nested `mod` blocks (rather than top-level
// `include!`s) so the gate can be applied to a whole tier at once.

extern crate alloc;
pub use alloc::boxed::Box;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;

// The generated `Kernel` and `SerialDriver` actions call `serial::*`
// (writeln / write_str / write_byte / init_uart). The glob import in each
// generated module resolves `serial` through this private `use`.
#[allow(unused_imports)]
use crate::serial;

// ---------------------------------------------------------------------------
// Pure systems — built on every arch. Actions touch only the heap re-exports
// above and (for serial_driver) the arch-agnostic `serial` text layer.
// ---------------------------------------------------------------------------
mod pure {
    // Re-import everything the generated wrappers' `use super::*;` needs.
    use super::*;

    // SerialDriver: the kernel-console FSM ($Uninit → $Up). Its actions are
    // `serial::*` calls — already arch-agnostic via `hal::Console`.
    include!(concat!(env!("OUT_DIR"), "/serial_driver.rs"));
    // Scheduler ($Idle/$Active): the run/halt mode of the kernel. Pure counter
    // arithmetic — no native deps beyond the heap types re-exported above.
    include!(concat!(env!("OUT_DIR"), "/scheduler.rs"));
    // Process + ProcessTable (B3 Step 3): per-process lifecycle HSM + the
    // table holding them. Pure.
    include!(concat!(env!("OUT_DIR"), "/process.rs"));
    include!(concat!(env!("OUT_DIR"), "/process_table.rs"));
    // BlockRequest (B4 Step 1): block-I/O request lifecycle. Pure.
    include!(concat!(env!("OUT_DIR"), "/block_request.rs"));
    // Mount (B4 Step 2): filesystem mount/unmount lifecycle. Pure.
    include!(concat!(env!("OUT_DIR"), "/mount.rs"));
    // OpenFile (B4 Step 3): per-fd lifecycle (access mode as state). Pure.
    include!(concat!(env!("OUT_DIR"), "/open_file.rs"));
    // Pipe (S6): per-pipe lifecycle (writer presence as state). Pure.
    include!(concat!(env!("OUT_DIR"), "/pipe.rs"));
    // IoScheduler (S6 follow-up): the single supervisor that sequences blocking
    // I/O ($Idle/$Busy + waiter queue). Pure.
    include!(concat!(env!("OUT_DIR"), "/io_scheduler.rs"));
    // UdpSocket (B5 Step 3b): one UDP socket's bind lifecycle. Pure.
    include!(concat!(env!("OUT_DIR"), "/udp_socket.rs"));
    // EventCounter (B7): tiny system driven by cross-core posts. Pure.
    include!(concat!(env!("OUT_DIR"), "/event_counter.rs"));
}
pub use pure::*;

// ---------------------------------------------------------------------------
// x86-only systems — actions reach into kernel modules that are themselves
// x86-gated (vm, usermode, elf, net, tcp, ip_reasm, xhci). The corresponding
// Frame *sources* (.frs) are arch-agnostic; only their *native action
// implementations* are x86-only at the moment. As those subsystems grow
// aarch64 legs the gates here move with them.
// ---------------------------------------------------------------------------
#[cfg(target_arch = "x86_64")]
mod x86_only {
    use super::*;
    #[allow(unused_imports)]
    use crate::serial;

    // Kernel: the boot HSM — orchestrates the kernel's init chain ($InitMemory
    // → $InitIDT → $InitTimer → $InitConsole → $LaunchInit → $Running). Holds
    // a SerialDriver (defined in `pure` above) in its domain.
    include!(concat!(env!("OUT_DIR"), "/kernel.rs"));
    // PageFaultHandler: the #PF classifier HSM. Calls crate::vm.
    include!(concat!(env!("OUT_DIR"), "/page_fault_handler.rs"));
    // SyscallDispatcher: validate + execute syscalls. Calls crate::usermode.
    include!(concat!(env!("OUT_DIR"), "/syscall_dispatcher.rs"));
    // ElfLoader (B3 Step 4): the load-phase FSM. Calls crate::elf::*.
    include!(concat!(env!("OUT_DIR"), "/elf_loader.rs"));
    // ArpResolver (B5 Step 2a): one IPv4→MAC resolution. Calls crate::net::*.
    include!(concat!(env!("OUT_DIR"), "/arp_resolver.rs"));
    // RxPipeline (B5 Step 3): receive-frame classify + dispatch. crate::net::*.
    include!(concat!(env!("OUT_DIR"), "/rx_pipeline.rs"));
    // TcpConnection (B5 Step 4): RFC-793 state machine. crate::tcp::*.
    include!(concat!(env!("OUT_DIR"), "/tcp_connection.rs"));
    // IpReassembly (B5 Step 6): fragmented-IPv4-datagram reassembly. crate::ip_reasm.
    include!(concat!(env!("OUT_DIR"), "/ip_reassembly.rs"));
    // HubPort (B6 Step 2): one xHCI root-hub port's lifecycle. crate::xhci.
    include!(concat!(env!("OUT_DIR"), "/hub_port.rs"));
    // UsbEnumeration (B6 Step 3): per-device enumeration lifecycle. crate::xhci.
    include!(concat!(env!("OUT_DIR"), "/usb_enumeration.rs"));
    // UsbTransfer (B6 Step 4): one transfer's lifecycle. crate::xhci.
    include!(concat!(env!("OUT_DIR"), "/usb_transfer.rs"));
    // UsbMsd (R3b): a Bulk-Only-Transport transaction's phase lifecycle. crate::xhci.
    include!(concat!(env!("OUT_DIR"), "/usb_msd.rs"));
}
#[cfg(target_arch = "x86_64")]
pub use x86_only::*;
