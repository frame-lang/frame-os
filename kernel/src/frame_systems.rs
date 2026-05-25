// kernel/src/frame_systems.rs
//
// Pulls in the Rust code framec generates from frame/kernel.frs (written
// to OUT_DIR by build.rs) and makes the `Kernel` system available to the
// rest of the crate.
//
// Mirror of shell/src/frame_systems.rs, with one extra wrinkle: framec's
// generated code refers to `String`, `Vec`, `Box`, and `to_string`
// unqualified (it expects them from the std prelude). The kernel is
// no_std, so those names aren't automatically in scope. We re-export them
// from `alloc` here so the generated `mod _kernel_framec { use super::*; }`
// wrapper picks them up via its glob import.
//
// (`Rc`, `format!`, and `vec!` don't need re-exporting: the generated
// code uses fully-qualified `alloc::rc::Rc` and the wrapper module
// imports `alloc::{vec, format}` itself.)

extern crate alloc;
pub use alloc::boxed::Box;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;

// The generated `Kernel` and `SerialDriver` actions call `serial::*`
// (writeln / write_str / write_byte / init_uart). The glob import in each
// generated module resolves `serial` through this private `use`.
use crate::serial;

// SerialDriver first: the Kernel holds a `SerialDriver` in its domain
// (`console: SerialDriver = @@SerialDriver()`). Rust items are
// order-independent within a module, but generating the dependency first
// keeps the include order matching the dependency direction.
include!(concat!(env!("OUT_DIR"), "/serial_driver.rs"));
include!(concat!(env!("OUT_DIR"), "/kernel.rs"));
// Scheduler ($Idle/$Active): the native preemptive scheduler (sched.rs)
// holds one and reads is_idle() to decide when the kernel halts. Its
// actions are pure counter arithmetic — no native deps beyond the heap
// types already re-exported above.
include!(concat!(env!("OUT_DIR"), "/scheduler.rs"));
// PageFaultHandler: the #PF classifier HSM. Its actions call
// `crate::vm::{is_lazy_region,lazy_map}` (full paths, resolved per crate)
// and `serial::*` (imported above).
include!(concat!(env!("OUT_DIR"), "/page_fault_handler.rs"));
// SyscallDispatcher: validate + execute syscalls, errors funneled to its
// $Active parent via `=> $^`. Actions call crate::usermode::{is_known_syscall,
// perform_syscall}.
include!(concat!(env!("OUT_DIR"), "/syscall_dispatcher.rs"));
// Process + ProcessTable (B3 Step 3): the per-process lifecycle HSM and the
// manager that holds Vec<Process>. Process must be included before
// ProcessTable (the latter's domain holds Vec<Process> and instantiates
// @@Process). Both are pure (no native action deps); usermode.rs drives the
// live ring-3 process through a global ProcessTable.
include!(concat!(env!("OUT_DIR"), "/process.rs"));
include!(concat!(env!("OUT_DIR"), "/process_table.rs"));
// ElfLoader (B3 Step 4): the load-phase FSM. Actions call crate::elf::* (the
// native byte parser + segment mapper); $Failed funnels cleanup.
include!(concat!(env!("OUT_DIR"), "/elf_loader.rs"));
// BlockRequest (B4 Step 1): the block-I/O request lifecycle, driven by the
// drained virtio-blk completion. Pure (no native action deps).
include!(concat!(env!("OUT_DIR"), "/block_request.rs"));
// Mount (B4 Step 2): the filesystem mount/unmount lifecycle. Pure.
include!(concat!(env!("OUT_DIR"), "/mount.rs"));
// OpenFile (B4 Step 3): per-fd lifecycle (access mode as state). Pure.
include!(concat!(env!("OUT_DIR"), "/open_file.rs"));
// Pipe (S6): per-pipe lifecycle (writer presence as state → read blocks vs EOF).
// Reader/writer counts live in its domain; `pipe.rs` owns the ring buffer. Pure.
include!(concat!(env!("OUT_DIR"), "/pipe.rs"));
// IoScheduler (S6 follow-up): the single supervisor that sequences blocking I/O.
// Owns the single-flight disk engine's access state ($Idle/$Busy) + waiter queue;
// `sched.rs` drives it (acquire→block-until-owner, release→hand-off+wake).
include!(concat!(env!("OUT_DIR"), "/io_scheduler.rs"));
// ArpResolver (B5 Step 2a): one IPv4→MAC resolution's lifecycle. Its actions
// call crate::net::{arp_send_request,arp_arm_timer,arp_on_failed}; the enter
// handler arms the retransmit timer (the native timer-wheel pattern).
include!(concat!(env!("OUT_DIR"), "/arp_resolver.rs"));
// RxPipeline (B5 Step 3): classify a received frame and dispatch to a protocol
// handler, threading an RxDescriptor down the classify→dispatch graph via enter
// params. Actions call crate::net::{on_arp,on_icmp,on_udp}.
include!(concat!(env!("OUT_DIR"), "/rx_pipeline.rs"));
// UdpSocket (B5 Step 3b): one UDP socket's bind lifecycle ($Unbound → $Bound).
// Pure (no native deps); net.rs holds one and the $Udp leaf delivers to it.
include!(concat!(env!("OUT_DIR"), "/udp_socket.rs"));
// TcpConnection (B5 Step 4): the RFC-793 state machine. Actions call crate::tcp::*
// (segment encode/parse + the connection's seq state + timers). tcp.rs holds the
// instance; the RxPipeline $Tcp leaf delivers segments to it.
include!(concat!(env!("OUT_DIR"), "/tcp_connection.rs"));
// IpReassembly (B5 Step 6): stitch a fragmented IPv4 datagram back together,
// threading a Fragment descriptor via enter params (self-transition re-store).
// Actions call crate::ip_reasm::{store,is_complete,on_complete,on_expired};
// ip_reasm.rs holds the instance + buffer + coverage map.
include!(concat!(env!("OUT_DIR"), "/ip_reassembly.rs"));
// HubPort (B6 Step 2): one xHCI root-hub port's connect/reset/enable lifecycle,
// with disconnect funneled to $Disconnected via the $Attached parent (=> $^).
// Actions call crate::xhci::{begin_port_reset,on_port_enabled}; the reset is a
// timed transition (settle timer armed in $Resetting's enter handler).
include!(concat!(env!("OUT_DIR"), "/hub_port.rs"));
// UsbEnumeration (B6 Step 3): a device's enumeration lifecycle ($Powered →
// $SlotEnabled → $AddressAssigned → …). Enter handlers issue the next xHCI
// command (crate::xhci::{cmd_enable_slot,address_device,...}); the native driver
// dispatches the milestone events on command completions. slot threads via domain.
include!(concat!(env!("OUT_DIR"), "/usb_enumeration.rs"));
// UsbTransfer (B6 Step 4): one transfer's lifecycle ($Idle → $InFlight →
// $Complete|$Failed). $InFlight's enter handler queues the transfer
// (crate::xhci::queue_interrupt_in); the driver dispatches complete()/fail() on
// the Transfer Event; $Complete reads the result (crate::xhci::on_report).
include!(concat!(env!("OUT_DIR"), "/usb_transfer.rs"));
// UsbMsd (R3b): one Bulk-Only Transport transaction's phase lifecycle ($Idle →
// $CommandPhase → $DataPhase → $StatusPhase → $Complete|$Failed). Each phase's
// enter handler issues the next bulk transfer (crate::xhci::msd_*); the driver
// dispatches the phase events on the bulk Transfer Events. Run once per SCSI
// command (INQUIRY → READ CAPACITY → READ(10)).
include!(concat!(env!("OUT_DIR"), "/usb_msd.rs"));
// EventCounter (B7): a tiny system driven by cross-core posts. Pure (no native
// deps). Its instance is pinned to one core; other cores post tick(n) events
// into an MPSC queue (crosscore.rs) that the owning core drains + dispatches.
include!(concat!(env!("OUT_DIR"), "/event_counter.rs"));
