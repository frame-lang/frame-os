// kernel/src/net.rs
//
// The networking protocol layer (B5 Step 2a). Native Ethernet/ARP encode +
// decode, plus the glue the `ArpResolver` Frame system calls. Frame owns the
// resolution *lifecycle* (incomplete → resolved, with a retransmit timer and a
// retry cap); this module owns the *bytes* (frame layout) and the *mechanism*
// (sending the request, the retransmit deadline, storing the resolved MAC).
//
// Step 2a resolves the QEMU slirp gateway's MAC via `ArpResolver`. The timer is
// the project's first "armed in the enter handler, fired by a native deadline
// through the receive loop" instance — the pattern the B5 plan uses for TCP's
// timers, rehearsed small here. ARP/timer state is single-flight (one
// resolution at a time at Step 2a), so plain statics suffice.

use crate::frame_systems::ArpResolver;
use crate::{interrupts, serial, virtio_net};

const ETHERTYPE_ARP: [u8; 2] = [0x08, 0x06];
const ARP_OPER_REQUEST: [u8; 2] = [0x00, 0x01];
const ARP_OPER_REPLY: [u8; 2] = [0x00, 0x02];

const GUEST_IP: [u8; 4] = [10, 0, 2, 15]; // QEMU slirp default guest address
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2]; // QEMU slirp gateway (answers ARP)

/// Retransmit interval, in PIT ticks. The gateway answers within a tick or two;
/// this is a generous re-send window.
const ARP_RETRANSMIT_TICKS: u64 = 100;

// Single-flight ARP resolution state (Step 2a resolves one address: the gateway).
static mut GATEWAY_MAC: [u8; 6] = [0; 6];
static mut ARP_DEADLINE: u64 = 0;
static mut ARP_FAILED: bool = false;

// --- helpers called by the ArpResolver Frame system ------------------------

/// Send a broadcast ARP request for the gateway. Called by `$Incomplete.$>()`.
pub fn arp_send_request() {
    let mac = virtio_net::mac();
    let mut f = [0u8; 42];
    // Ethernet header.
    f[0..6].copy_from_slice(&[0xFF; 6]); // dst = broadcast
    f[6..12].copy_from_slice(&mac); // src = our MAC
    f[12..14].copy_from_slice(&ETHERTYPE_ARP);
    // ARP payload.
    f[14..16].copy_from_slice(&[0x00, 0x01]); // htype = Ethernet
    f[16..18].copy_from_slice(&[0x08, 0x00]); // ptype = IPv4
    f[18] = 6; // hlen
    f[19] = 4; // plen
    f[20..22].copy_from_slice(&ARP_OPER_REQUEST);
    f[22..28].copy_from_slice(&mac); // sender hardware addr
    f[28..32].copy_from_slice(&GUEST_IP); // sender protocol addr
    f[32..38].copy_from_slice(&[0x00; 6]); // target hardware addr (unknown)
    f[38..42].copy_from_slice(&GATEWAY_IP); // target protocol addr
    serial::writeln("[arp] who-has 10.0.2.2 (gateway)");
    virtio_net::tx_frame(&f);
}

/// Arm the retransmit timer. Called by `$Incomplete.$>()`.
pub fn arp_arm_timer() {
    unsafe {
        (&raw mut ARP_DEADLINE).write(interrupts::ticks() + ARP_RETRANSMIT_TICKS);
    }
}

/// Whether the retransmit deadline has passed (checked by the receive loop).
pub fn arp_timer_expired() -> bool {
    interrupts::ticks() >= unsafe { (&raw const ARP_DEADLINE).read() }
}

/// Record that resolution gave up (called by `$Failed.$>()`).
pub fn arp_on_failed() {
    unsafe {
        (&raw mut ARP_FAILED).write(true);
    }
    serial::writeln("[arp] resolution failed (no reply after retries)");
}

// --- frame inspection ------------------------------------------------------

/// Is `frame` an ARP reply from the gateway addressed to us?
fn is_gateway_arp_reply(frame: &[u8]) -> bool {
    frame.len() >= 42
        && frame[12..14] == ETHERTYPE_ARP
        && frame[20..22] == ARP_OPER_REPLY
        && frame[28..32] == GATEWAY_IP // sender protocol addr = gateway
        && frame[38..42] == GUEST_IP // target protocol addr = us
}

/// Store the gateway MAC from an ARP reply's sender-hardware-address field.
fn store_gateway_mac(frame: &[u8]) {
    let p = &raw mut GATEWAY_MAC;
    unsafe { (*p).copy_from_slice(&frame[22..28]) };
}

// --- demo ------------------------------------------------------------------

/// B5 Step 2a demo: bring up virtio-net, then resolve the gateway's MAC through
/// the `ArpResolver` Frame system. Creating the resolver fires `$Incomplete`'s
/// enter handler, which sends the first request + arms the timer; the loop
/// drives `reply()` on a matching ARP reply and `timeout()` when the deadline
/// passes (re-send via re-enter, or `-> $Failed` after the retry cap).
pub fn run_demo() {
    if !virtio_net::init() {
        return;
    }
    serial::write_str("[net] MAC ");
    print_mac(&virtio_net::mac());
    serial::writeln("");

    unsafe { (&raw mut ARP_FAILED).write(false) };

    interrupts::enable();
    // Construction runs $Incomplete.$>() → first request sent + timer armed.
    let mut arpr = ArpResolver::__create();
    let overall = interrupts::ticks() + 1000;
    let mut buf = [0u8; virtio_net::MAX_FRAME];
    while !arpr.is_resolved() && !arpr.is_failed() && interrupts::ticks() < overall {
        if let Some(n) = virtio_net::poll_rx(&mut buf) {
            if is_gateway_arp_reply(&buf[..n]) {
                store_gateway_mac(&buf[..n]);
                arpr.reply(); // -> $Resolved
            }
        } else if arp_timer_expired() {
            arpr.timeout(); // re-send + re-arm, or -> $Failed at the retry cap
        } else {
            interrupts::wait_for_interrupt();
        }
    }
    interrupts::disable();

    if arpr.is_resolved() {
        serial::write_str("[arp] resolved 10.0.2.2 -> ");
        print_mac(&gateway_mac());
        serial::writeln("");
        serial::writeln("[net] gateway resolved via ArpResolver: ok");
    } else {
        serial::writeln("[net] gateway resolution did not complete");
    }
}

fn gateway_mac() -> [u8; 6] {
    unsafe { (&raw const GATEWAY_MAC).read() }
}

/// Print a MAC address as `aa:bb:cc:dd:ee:ff`.
fn print_mac(mac: &[u8; 6]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in mac.iter().enumerate() {
        if i != 0 {
            serial::write_byte(b':');
        }
        serial::write_byte(HEX[(b >> 4) as usize]);
        serial::write_byte(HEX[(b & 0xF) as usize]);
    }
}
