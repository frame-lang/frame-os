// kernel-tests/src/lib.rs
//
// Host-target build of the kernel's `Kernel` Frame system, wired to a
// capturing `serial` module so behavioral tests can assert on what the
// HSM's actions print.
//
// The generated `Kernel` actions call `serial::writeln(...)` /
// `serial::write_str(...)`. The generated module is wrapped in
// `mod _kernel_framec { use super::*; ... }`, so its glob import pulls in
// whatever names are visible here — including the `serial` module below.
// On the host, `String`/`Vec`/`Box`/`ToString` come from the std prelude
// (already in scope everywhere), so unlike the no_std kernel crate we
// don't re-export them from `alloc`.

/// Capturing serial sink for tests. Mirrors the public API of the
/// kernel's real `crate::serial` (COM1 port I/O) but appends to a
/// thread-local buffer instead.
///
/// Thread-local (not a global) so tests — which libtest runs each on its
/// own thread — are isolated from each other. Each test should call
/// `serial::clear()` before constructing a `Kernel` to start from a known
/// state, then `serial::captured()` to read what the HSM printed.
pub mod serial {
    use std::cell::RefCell;

    thread_local! {
        static CAPTURED: RefCell<String> = const { RefCell::new(String::new()) };
    }

    /// Host stand-in for the 16550 init sequence. No UART on the host, so
    /// this is a no-op — SerialDriver's $Uninitialized → $Ready transition
    /// still happens; we just don't program nonexistent hardware. (If a
    /// test ever needs to assert init ran, capture a marker here.)
    pub fn init_uart() {}

    /// Append a single byte (interpreted as an ASCII/Latin-1 char). The
    /// kernel's panic handler uses this for the `:` separator and digits.
    pub fn write_byte(b: u8) {
        CAPTURED.with(|c| c.borrow_mut().push(b as char));
    }

    pub fn write_str(s: &str) {
        CAPTURED.with(|c| c.borrow_mut().push_str(s));
    }

    pub fn writeln(s: &str) {
        CAPTURED.with(|c| {
            let mut buf = c.borrow_mut();
            buf.push_str(s);
            buf.push('\n');
        });
    }

    pub fn write_u32_decimal(n: u32) {
        CAPTURED.with(|c| c.borrow_mut().push_str(&n.to_string()));
    }

    /// Append a u64 as 16 hex digits (matches the kernel's serial).
    pub fn write_hex_u64(n: u64) {
        CAPTURED.with(|c| c.borrow_mut().push_str(&format!("{n:016x}")));
    }

    /// Return a copy of everything captured on this thread so far.
    pub fn captured() -> String {
        CAPTURED.with(|c| c.borrow().clone())
    }

    /// Reset the capture buffer for this thread.
    pub fn clear() {
        CAPTURED.with(|c| c.borrow_mut().clear());
    }
}

/// Host test-double for the kernel's `vm` module. The generated
/// `PageFaultHandler` actions call `crate::vm::{is_lazy_region, lazy_map}`;
/// in the kernel those touch real page tables, here they're controllable
/// thread-locals so behavioral tests can drive each classification path.
/// Thread-local (libtest runs each test on its own thread) so concurrent
/// tests don't clobber each other's settings.
pub mod vm {
    use core::cell::Cell;

    thread_local! {
        static LAZY: Cell<bool> = const { Cell::new(false) };
        static MAP_OK: Cell<bool> = const { Cell::new(true) };
    }

    /// Set whether the next `is_lazy_region` reports the address as lazy.
    pub fn set_lazy(b: bool) {
        LAZY.with(|c| c.set(b));
    }

    /// Set whether the next `lazy_map` succeeds (false simulates OOM).
    pub fn set_map_ok(b: bool) {
        MAP_OK.with(|c| c.set(b));
    }

    pub fn is_lazy_region(_addr: u64) -> bool {
        LAZY.with(|c| c.get())
    }

    pub fn lazy_map(_addr: u64) -> bool {
        MAP_OK.with(|c| c.get())
    }
}

/// Host test-doubles for the native modules the Kernel HSM's init phases
/// call. On the host there's no hardware to program, so each is a no-op —
/// the boot chain still runs to `$Running` in the behavioral tests. Same
/// "shared `.frs`, different native actions per target" pattern as `serial`
/// and `vm`.
pub mod frames {
    pub fn init() {}
}

pub mod interrupts {
    pub fn init() {}
}

pub mod pic {
    pub fn remap() {}
}

pub mod pit {
    pub fn init(_hz: u32) {}
}

/// Host test-double for the kernel's `usermode` module. The
/// `SyscallDispatcher` actions call `crate::usermode::{is_known_syscall,
/// perform_syscall}`; here they're simple deterministic stubs (no ring-3 /
/// longjmp) so behavioral tests can drive the validate / execute / reject
/// paths. `perform_syscall` echoes `a0` so a test can assert the value
/// flowed through `$Executing`.
pub mod usermode {
    pub fn is_known_syscall(num: u64) -> bool {
        num < 2
    }

    pub fn perform_syscall(_num: u64, a0: u64, _a1: u64) -> u64 {
        a0
    }
}

/// Host test-double for the kernel's `elf` module. The `ElfLoader` actions
/// call `crate::elf::{read_header, validate_header, map_segments, build_stack,
/// cleanup, entry_va, stack_top}`. Here the *header parsing* is real (so
/// corrupt / truncated ELF tests are meaningful), but mapping is stubbed
/// (`map_segments`/`build_stack` succeed without touching paging). A test sets
/// the input with `prepare(&BYTES)`, then constructs an `ElfLoader`.
pub mod elf {
    use std::cell::Cell;

    /// Mirror of the kernel's `crate::elf::ElfHeader` — the descriptor threaded
    /// down the ElfLoader phase pipeline as an enter param.
    #[derive(Clone, Copy, Default)]
    pub struct ElfHeader {
        pub phoff: u64,
        pub phentsize: u16,
        pub phnum: u16,
    }

    thread_local! {
        static BYTES: Cell<&'static [u8]> = const { Cell::new(&[]) };
        static ENTRY: Cell<u64> = const { Cell::new(0) };
        static STACK_TOP: Cell<u64> = const { Cell::new(0) };
    }

    fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
        let s = b.get(o..o + 2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
        let s = b.get(o..o + 8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Some(u64::from_le_bytes(a))
    }

    /// Set the ELF image for the load that follows (test-only entry point).
    pub fn prepare(bytes: &'static [u8]) {
        BYTES.with(|c| c.set(bytes));
        ENTRY.with(|c| c.set(0));
        STACK_TOP.with(|c| c.set(0));
    }

    pub fn read_header() -> Option<ElfHeader> {
        BYTES.with(|c| {
            let b = c.get();
            match (rd_u64(b, 24), rd_u64(b, 32), rd_u16(b, 54), rd_u16(b, 56)) {
                (Some(entry), Some(phoff), Some(phentsize), Some(phnum)) => {
                    ENTRY.with(|e| e.set(entry));
                    Some(ElfHeader {
                        phoff,
                        phentsize,
                        phnum,
                    })
                }
                _ => None,
            }
        })
    }

    pub fn validate_header(hdr: ElfHeader) -> bool {
        BYTES.with(|c| {
            let b = c.get();
            if b.len() < 64 || &b[0..4] != b"\x7fELF" || b[4] != 2 || b[5] != 1 {
                return false;
            }
            if !matches!((rd_u16(b, 16), rd_u16(b, 18)), (Some(2), Some(0x3E))) {
                return false;
            }
            let ph_end = hdr.phoff + hdr.phnum as u64 * hdr.phentsize as u64;
            (ph_end as usize) <= b.len() && hdr.phentsize >= 56
        })
    }

    // Mapping is stubbed on the host (no paging). Succeeds.
    pub fn map_segments(_hdr: ElfHeader) -> bool {
        true
    }

    pub fn build_stack() -> bool {
        // Mirrors kernel::elf USER_STACK_PAGES = 32 (the value is inert for the
        // loader-phase FSM tests; kept consistent so the double doesn't mislead).
        STACK_TOP.with(|c| c.set(0x2000_0000 + 32 * 4096 - 16));
        true
    }

    pub fn entry_va() -> u64 {
        ENTRY.with(|c| c.get())
    }

    pub fn stack_top() -> u64 {
        STACK_TOP.with(|c| c.get())
    }

    pub fn cleanup() {}
}

/// Host test-double for the kernel's `net` module. The generated `ArpResolver`
/// actions call `crate::net::{arp_send_request, arp_arm_timer, arp_on_failed}`;
/// in the kernel those build/send Ethernet frames + arm the retransmit
/// deadline, here they record call counts in thread-locals so behavioral tests
/// can assert "one request + one timer armed per attempt" and "failed after the
/// retry cap." Thread-local (libtest runs each test on its own thread).
pub mod net {
    use std::cell::Cell;

    /// Mirror of the kernel's `crate::net::RxDescriptor` — the parsed summary
    /// threaded down the RxPipeline classify→dispatch graph.
    #[derive(Clone, Copy, Default, Debug)]
    pub struct RxDescriptor {
        pub ethertype: u16,
        pub ip_proto: u8,
    }

    thread_local! {
        static REQUESTS: Cell<u32> = const { Cell::new(0) };
        static ARMS: Cell<u32> = const { Cell::new(0) };
        static FAILED: Cell<bool> = const { Cell::new(false) };
        // Which RxPipeline leaf last fired (for the pipeline behavioral tests).
        static DISPATCH: Cell<&'static str> = const { Cell::new("") };
    }

    pub fn arp_send_request() {
        REQUESTS.with(|c| c.set(c.get() + 1));
    }
    pub fn arp_arm_timer() {
        ARMS.with(|c| c.set(c.get() + 1));
    }
    pub fn arp_on_failed() {
        FAILED.with(|c| c.set(true));
    }

    // RxPipeline leaves — record which protocol the descriptor was dispatched to.
    pub fn on_arp(_pkt: RxDescriptor) {
        DISPATCH.with(|c| c.set("arp"));
    }
    pub fn on_icmp(_pkt: RxDescriptor) {
        DISPATCH.with(|c| c.set("icmp"));
    }
    pub fn on_udp(_pkt: RxDescriptor) {
        DISPATCH.with(|c| c.set("udp"));
    }
    pub fn on_tcp(_pkt: RxDescriptor) {
        DISPATCH.with(|c| c.set("tcp"));
    }

    // Test inspectors.
    pub fn requests_sent() -> u32 {
        REQUESTS.with(|c| c.get())
    }
    pub fn timers_armed() -> u32 {
        ARMS.with(|c| c.get())
    }
    pub fn failed() -> bool {
        FAILED.with(|c| c.get())
    }
    pub fn last_dispatch() -> &'static str {
        DISPATCH.with(|c| c.get())
    }
    pub fn reset() {
        REQUESTS.with(|c| c.set(0));
        ARMS.with(|c| c.set(0));
        FAILED.with(|c| c.set(false));
        DISPATCH.with(|c| c.set(""));
    }
}

/// Host test-double for the kernel's `tcp` module. The generated
/// `TcpConnection` actions call `crate::tcp::{send_syn, send_syn_ack, send_ack,
/// send_fin, deliver_data, arm_retransmit, cancel_retransmit, arm_timewait,
/// on_reset}`, and its `segment(seg)` handlers read `seg.{syn,ack,fin,
/// payload_len}`. Here the segment is a plain struct the tests build, and the
/// actions push their names onto a thread-local log so a test can assert "the
/// SYN-ACK was sent on this transition."
pub mod tcp {
    use std::cell::RefCell;

    /// Mirror of the kernel's `crate::tcp::TcpSegment` — the parsed segment the
    /// FSM routes on. (The kernel's carries more fields for actually building
    /// replies; the FSM only reads these.)
    #[derive(Clone, Copy, Default, Debug)]
    pub struct TcpSegment {
        pub syn: bool,
        pub ack: bool,
        pub fin: bool,
        pub rst: bool,
        pub payload_len: usize,
    }

    thread_local! {
        static ACTIONS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }
    fn rec(a: &'static str) {
        ACTIONS.with(|c| c.borrow_mut().push(a));
    }

    pub fn send_syn() {
        rec("send_syn");
    }
    pub fn send_syn_ack() {
        rec("send_syn_ack");
    }
    pub fn send_ack() {
        rec("send_ack");
    }
    pub fn send_fin() {
        rec("send_fin");
    }
    pub fn deliver_data() {
        rec("deliver_data");
    }
    pub fn arm_retransmit() {
        rec("arm_retransmit");
    }
    pub fn cancel_retransmit() {
        rec("cancel_retransmit");
    }
    pub fn arm_timewait() {
        rec("arm_timewait");
    }
    pub fn on_reset() {
        rec("on_reset");
    }

    /// The actions fired since the last `reset()`.
    pub fn actions() -> Vec<&'static str> {
        ACTIONS.with(|c| c.borrow().clone())
    }
    /// Whether `a` was among the fired actions.
    pub fn fired(a: &'static str) -> bool {
        ACTIONS.with(|c| c.borrow().contains(&a))
    }
    pub fn reset() {
        ACTIONS.with(|c| c.borrow_mut().clear());
    }
}

/// Host test-double for the kernel's `ip_reasm` module. The generated
/// `IpReassembly` actions call `crate::ip_reasm::{store, is_complete,
/// on_complete, on_expired}`, and `fragment(frag)` reads a `Fragment`. Here the
/// fragment is a plain struct the tests build; `store` counts calls,
/// `is_complete` is a settable flag (so a test drives the `$Reassembling →
/// $Complete` guard), and `on_complete`/`on_expired` latch so a test can assert
/// the terminal action fired. The real reassembly *algorithm* (coverage map,
/// reconstruction) lives in the kernel and is validated end-to-end by the
/// `qemu-tap` fragmented ping; these tests pin the FSM's *transitions*.
pub mod ip_reasm {
    use std::cell::Cell;

    /// Mirror of the kernel's `crate::ip_reasm::Fragment` — the parsed fragment
    /// summary threaded into the IpReassembly FSM as an enter param.
    #[derive(Clone, Copy, Default, Debug)]
    pub struct Fragment {
        pub offset: usize,
        pub len: usize,
        pub more: bool,
        pub ident: u16,
    }

    thread_local! {
        static STORED: Cell<u32> = const { Cell::new(0) };
        static COMPLETE: Cell<bool> = const { Cell::new(false) };
        static COMPLETED: Cell<bool> = const { Cell::new(false) };
        static EXPIRED: Cell<bool> = const { Cell::new(false) };
    }

    pub fn store(_frag: Fragment) {
        STORED.with(|c| c.set(c.get() + 1));
    }
    pub fn is_complete() -> bool {
        COMPLETE.with(|c| c.get())
    }
    pub fn on_complete() {
        COMPLETED.with(|c| c.set(true));
    }
    pub fn on_expired() {
        EXPIRED.with(|c| c.set(true));
    }

    /// Test control: what the next `is_complete()` guard reports.
    pub fn set_complete(b: bool) {
        COMPLETE.with(|c| c.set(b));
    }
    /// Test inspectors.
    pub fn stored() -> u32 {
        STORED.with(|c| c.get())
    }
    pub fn completed() -> bool {
        COMPLETED.with(|c| c.get())
    }
    pub fn expired() -> bool {
        EXPIRED.with(|c| c.get())
    }
    pub fn reset() {
        STORED.with(|c| c.set(0));
        COMPLETE.with(|c| c.set(false));
        COMPLETED.with(|c| c.set(false));
        EXPIRED.with(|c| c.set(false));
    }
}

/// Host test-double for the kernel's `xhci` module. The generated `HubPort`
/// actions call `crate::xhci::{begin_port_reset, on_port_enabled}` (both take the
/// 1-based port). Here they record the call + port so a behavioral test can
/// assert "the reset was begun on port 5" and "the enabled action fired." The
/// real PORTSC pokes live in the kernel; these tests pin the FSM transitions.
pub mod xhci {
    use core::cell::Cell;

    thread_local! {
        static RESET_PORT: Cell<u8> = const { Cell::new(0) };
        static RESETS: Cell<u32> = const { Cell::new(0) };
        static ENABLED_PORT: Cell<u8> = const { Cell::new(0) };
        // Enumeration actions (UsbEnumeration).
        static ENABLE_SLOTS: Cell<u32> = const { Cell::new(0) };
        static ADDR_SLOT: Cell<u8> = const { Cell::new(0) };
        static GET_DESC_SLOT: Cell<u8> = const { Cell::new(0) };
        static DESC_READS: Cell<u32> = const { Cell::new(0) };
        static SET_CONFIG_SLOT: Cell<u8> = const { Cell::new(0) };
        static CONFIGURED_SLOT: Cell<u8> = const { Cell::new(0) };
        // Transfer actions (UsbTransfer).
        static QUEUED_TRANSFERS: Cell<u32> = const { Cell::new(0) };
        static REPORTS_READ: Cell<u32> = const { Cell::new(0) };
        // Mass-storage actions (UsbMsd).
        static CBW_CMD: Cell<u8> = const { Cell::new(0) };
        static CBWS_SENT: Cell<u32> = const { Cell::new(0) };
        static DATA_RECVS: Cell<u32> = const { Cell::new(0) };
        static CSW_RECVS: Cell<u32> = const { Cell::new(0) };
    }

    pub fn begin_port_reset(port: u8) {
        RESET_PORT.with(|c| c.set(port));
        RESETS.with(|c| c.set(c.get() + 1));
    }
    pub fn on_port_enabled(port: u8) {
        ENABLED_PORT.with(|c| c.set(port));
    }
    pub fn cmd_enable_slot() {
        ENABLE_SLOTS.with(|c| c.set(c.get() + 1));
    }
    pub fn address_device(slot: u8) {
        ADDR_SLOT.with(|c| c.set(slot));
    }
    pub fn get_device_descriptor(slot: u8) {
        GET_DESC_SLOT.with(|c| c.set(slot));
    }
    pub fn read_device_descriptor() {
        DESC_READS.with(|c| c.set(c.get() + 1));
    }
    pub fn set_configuration(slot: u8) {
        SET_CONFIG_SLOT.with(|c| c.set(slot));
    }
    pub fn on_configured(slot: u8) {
        CONFIGURED_SLOT.with(|c| c.set(slot));
    }
    pub fn queue_interrupt_in() {
        QUEUED_TRANSFERS.with(|c| c.set(c.get() + 1));
    }
    pub fn on_report() {
        REPORTS_READ.with(|c| c.set(c.get() + 1));
    }
    pub fn msd_send_cbw(cmd: u8) {
        CBW_CMD.with(|c| c.set(cmd));
        CBWS_SENT.with(|c| c.set(c.get() + 1));
    }
    pub fn msd_recv_data(_cmd: u8) {
        DATA_RECVS.with(|c| c.set(c.get() + 1));
    }
    pub fn msd_recv_csw() {
        CSW_RECVS.with(|c| c.set(c.get() + 1));
    }

    /// Test inspectors.
    pub fn reset_port() -> u8 {
        RESET_PORT.with(|c| c.get())
    }
    pub fn resets() -> u32 {
        RESETS.with(|c| c.get())
    }
    pub fn enabled_port() -> u8 {
        ENABLED_PORT.with(|c| c.get())
    }
    pub fn enable_slots() -> u32 {
        ENABLE_SLOTS.with(|c| c.get())
    }
    pub fn addr_slot() -> u8 {
        ADDR_SLOT.with(|c| c.get())
    }
    pub fn get_desc_slot() -> u8 {
        GET_DESC_SLOT.with(|c| c.get())
    }
    pub fn desc_reads() -> u32 {
        DESC_READS.with(|c| c.get())
    }
    pub fn set_config_slot() -> u8 {
        SET_CONFIG_SLOT.with(|c| c.get())
    }
    pub fn configured_slot() -> u8 {
        CONFIGURED_SLOT.with(|c| c.get())
    }
    pub fn queued_transfers() -> u32 {
        QUEUED_TRANSFERS.with(|c| c.get())
    }
    pub fn reports_read() -> u32 {
        REPORTS_READ.with(|c| c.get())
    }
    pub fn cbw_cmd() -> u8 {
        CBW_CMD.with(|c| c.get())
    }
    pub fn cbws_sent() -> u32 {
        CBWS_SENT.with(|c| c.get())
    }
    pub fn data_recvs() -> u32 {
        DATA_RECVS.with(|c| c.get())
    }
    pub fn csw_recvs() -> u32 {
        CSW_RECVS.with(|c| c.get())
    }
    pub fn reset() {
        RESET_PORT.with(|c| c.set(0));
        RESETS.with(|c| c.set(0));
        ENABLED_PORT.with(|c| c.set(0));
        ENABLE_SLOTS.with(|c| c.set(0));
        ADDR_SLOT.with(|c| c.set(0));
        GET_DESC_SLOT.with(|c| c.set(0));
        DESC_READS.with(|c| c.set(0));
        SET_CONFIG_SLOT.with(|c| c.set(0));
        CONFIGURED_SLOT.with(|c| c.set(0));
        QUEUED_TRANSFERS.with(|c| c.set(0));
        REPORTS_READ.with(|c| c.set(0));
        CBW_CMD.with(|c| c.set(0));
        CBWS_SENT.with(|c| c.set(0));
        DATA_RECVS.with(|c| c.set(0));
        CSW_RECVS.with(|c| c.set(0));
    }
}

// Pull in the framec-generated systems. Each generated file ends with
// `pub use _<name>_framec::*;`, re-exporting the system type at this crate's
// root. SerialDriver first (Kernel holds one in its domain). Task and
// Scheduler (B1) are independent — the native scheduler composes them with
// a ready-queue; the Frame systems don't reference each other.
include!(concat!(env!("OUT_DIR"), "/serial_driver.rs"));
include!(concat!(env!("OUT_DIR"), "/kernel.rs"));
include!(concat!(env!("OUT_DIR"), "/task.rs"));
include!(concat!(env!("OUT_DIR"), "/scheduler.rs"));
include!(concat!(env!("OUT_DIR"), "/page_fault_handler.rs"));
include!(concat!(env!("OUT_DIR"), "/syscall_dispatcher.rs"));
// Process before ProcessTable: ProcessTable's domain holds Vec<Process> and
// instantiates @@Process, so the Process type must be in scope first.
include!(concat!(env!("OUT_DIR"), "/process.rs"));
include!(concat!(env!("OUT_DIR"), "/process_table.rs"));
// ElfLoader (B3 Step 4): the load-phase FSM. Actions call crate::elf::* (the
// host double above does real header parsing, stubs the mapping).
include!(concat!(env!("OUT_DIR"), "/elf_loader.rs"));
// BlockRequest (B4 Step 1): I/O request lifecycle. Pure (no native deps).
include!(concat!(env!("OUT_DIR"), "/block_request.rs"));
// Mount (B4 Step 2): filesystem mount/unmount lifecycle. Pure (no native deps).
include!(concat!(env!("OUT_DIR"), "/mount.rs"));
// OpenFile (B4 Step 3): per-fd access-mode lifecycle. Pure (no native deps).
include!(concat!(env!("OUT_DIR"), "/open_file.rs"));
// ArpResolver (B5 Step 2a): one IPv4→MAC resolution's lifecycle. Actions call
// crate::net::* (the host double above counts requests/arms/failure).
include!(concat!(env!("OUT_DIR"), "/arp_resolver.rs"));
// RxPipeline (B5 Step 3): classify→dispatch a received frame, threading an
// RxDescriptor via enter params. Actions call crate::net::{on_arp,on_icmp,on_udp}.
include!(concat!(env!("OUT_DIR"), "/rx_pipeline.rs"));
// UdpSocket (B5 Step 3b): one UDP socket's bind lifecycle. Pure (no native deps).
include!(concat!(env!("OUT_DIR"), "/udp_socket.rs"));
// TcpConnection (B5 Step 4): the RFC-793 state machine. Actions call
// crate::tcp::* (the host double above records them); segment() reads seg fields.
include!(concat!(env!("OUT_DIR"), "/tcp_connection.rs"));
// IpReassembly (B5 Step 6): the fragment-reassembly lifecycle. Actions call
// crate::ip_reasm::* (the host double above counts/controls them); fragment()
// threads a Fragment via enter params (self-transition re-store).
include!(concat!(env!("OUT_DIR"), "/ip_reassembly.rs"));
// HubPort (B6 Step 2): one xHCI port's connect/reset/enable lifecycle. Actions
// call crate::xhci::{begin_port_reset,on_port_enabled} (the host double above
// records them); disconnect funnels to $Disconnected via the $Attached parent.
include!(concat!(env!("OUT_DIR"), "/hub_port.rs"));
// UsbEnumeration (B6 Step 3): the device enumeration lifecycle. Actions call
// crate::xhci::{cmd_enable_slot,address_device,on_address_assigned} (the host
// double above records them); slot threads via the FSM domain.
include!(concat!(env!("OUT_DIR"), "/usb_enumeration.rs"));
// UsbTransfer (B6 Step 4): one transfer's lifecycle. Actions call
// crate::xhci::{queue_interrupt_in,on_report} (the host double above counts them).
include!(concat!(env!("OUT_DIR"), "/usb_transfer.rs"));
// UsbMsd (R3b): one Bulk-Only Transport transaction's phase lifecycle. Actions
// call crate::xhci::{msd_send_cbw,msd_recv_data,msd_recv_csw} (the host double
// above counts them); the SCSI command threads via the FSM domain.
include!(concat!(env!("OUT_DIR"), "/usb_msd.rs"));
// EventCounter (B7): the cross-core-post demo system. Pure (no native deps).
include!(concat!(env!("OUT_DIR"), "/event_counter.rs"));
