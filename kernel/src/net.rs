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

use crate::frame_systems::{ArpResolver, RxPipeline, UdpSocket};
use crate::{interrupts, serial, virtio_net};

/// The small parsed summary of a received frame that flows down the
/// `RxPipeline` classify→dispatch graph as an enter parameter. The frame bytes
/// themselves stay in the native `RX_FRAME` buffer; this carries just what the
/// pipeline routes on. (`#[derive(Clone, Default)]` — required for framec's
/// typed enter-arg context.)
#[derive(Clone, Copy, Default, Debug)]
pub struct RxDescriptor {
    pub ethertype: u16,
    pub ip_proto: u8,
}

const ETHERTYPE_ARP: [u8; 2] = [0x08, 0x06];
const ETHERTYPE_IPV4: [u8; 2] = [0x08, 0x00];
const ARP_OPER_REQUEST: [u8; 2] = [0x00, 0x01];
const ARP_OPER_REPLY: [u8; 2] = [0x00, 0x02];

const IP_PROTO_ICMP: u8 = 1;
const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_ECHO_REQUEST: u8 = 8;

const GUEST_IP: [u8; 4] = [10, 0, 2, 15]; // QEMU slirp default guest address
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2]; // QEMU slirp gateway (answers ARP + ping)

/// Retransmit interval, in PIT ticks. The gateway answers within a tick or two;
/// this is a generous re-send window.
const ARP_RETRANSMIT_TICKS: u64 = 100;

// Single-flight ARP resolution state (Step 2a resolves one address: the gateway).
static mut GATEWAY_MAC: [u8; 6] = [0; 6];
static mut ARP_DEADLINE: u64 = 0;
static mut ARP_FAILED: bool = false;

// RX pipeline state (B5 Step 3). Single-flight: the network code is driven from
// one boot-context loop with IRQs used only to wake it, so one buffer + a set
// of dispatch flags suffice.
static mut RX_FRAME: [u8; virtio_net::MAX_FRAME] = [0; virtio_net::MAX_FRAME];
static mut RX_LEN: usize = 0;
static mut ARP_GATEWAY_SEEN: bool = false; // on_arp saw the gateway's reply
static mut ICMP_REPLY_SEEN: bool = false; // on_icmp saw our ping's reply
static mut EXPECTED_PING_SEQ: u16 = 0;
static mut PIPELINE: Option<RxPipeline> = None;

// UDP socket state (B5 Step 3b). One socket, single-flight, like the rest of
// the demo. DHCP uses xid to match its own reply; the offered IP is latched.
static mut UDP_SOCK: Option<UdpSocket> = None;
static mut DHCP_XID: [u8; 4] = [0x39, 0x03, 0xF3, 0x26];
static mut DHCP_OFFER_SEEN: bool = false;
static mut OFFERED_IP: [u8; 4] = [0; 4];

fn pipeline() -> &'static mut RxPipeline {
    let p = &raw mut PIPELINE;
    unsafe { (*p).get_or_insert_with(RxPipeline::__create) }
}

fn udp_sock() -> &'static mut UdpSocket {
    let p = &raw mut UDP_SOCK;
    unsafe { (*p).get_or_insert_with(UdpSocket::__create) }
}

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

// --- RX pipeline dispatch (B5 Step 3) --------------------------------------

/// The current frame in the native RX buffer (`RX_FRAME[..RX_LEN]`).
fn rx_frame() -> &'static [u8] {
    let n = unsafe { (&raw const RX_LEN).read() };
    let p = &raw const RX_FRAME;
    let full: &'static [u8] = unsafe { &*p }; // bind raw ptr, then deref+borrow
    &full[..n]
}

/// Parse the classification descriptor from a received frame (ethertype, and
/// for IPv4 the protocol). The `RxPipeline` routes on these; the per-protocol
/// handlers re-read `RX_FRAME` for their specifics.
fn parse_descriptor(frame: &[u8]) -> RxDescriptor {
    let mut d = RxDescriptor::default();
    if frame.len() >= 14 {
        d.ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        if d.ethertype == 0x0800 && frame.len() >= 14 + 20 {
            d.ip_proto = frame[14 + 9];
        }
    }
    d
}

/// Pipeline leaf (`$Arp`): if this is the gateway's ARP reply, store its MAC
/// and flag it for the resolution loop.
pub fn on_arp(_pkt: RxDescriptor) {
    let frame = rx_frame();
    if is_gateway_arp_reply(frame) {
        store_gateway_mac(frame);
        unsafe { (&raw mut ARP_GATEWAY_SEEN).write(true) };
    }
}

/// Pipeline leaf (`$Icmp`): if this is our ping's echo reply, flag it.
pub fn on_icmp(_pkt: RxDescriptor) {
    let seq = unsafe { (&raw const EXPECTED_PING_SEQ).read() };
    if is_icmp_echo_reply(rx_frame(), seq) {
        unsafe { (&raw mut ICMP_REPLY_SEEN).write(true) };
    }
}

/// Pipeline leaf (`$Udp`): deliver an inbound UDP datagram to the socket bound
/// on its destination port. If it's our DHCP OFFER (port 68, BOOTREPLY, our
/// xid), latch the offered IP. The bound-socket check is the `UdpSocket`'s job
/// — `recv()` is only handled in `$Bound`.
pub fn on_udp(_pkt: RxDescriptor) {
    let frame = rx_frame();
    let ihl = (frame[14] & 0x0F) as usize * 4;
    let udp = 14 + ihl;
    if frame.len() < udp + 8 {
        return;
    }
    let dst_port = u16::from_be_bytes([frame[udp + 2], frame[udp + 3]]);

    let sock = udp_sock();
    if !sock.is_bound() || sock.port() != dst_port {
        return; // no socket bound on this port → dropped
    }

    // DHCP OFFER? (UDP :68, BOOTREPLY op=2, magic cookie, our xid.)
    let dhcp = udp + 8;
    let xid = unsafe { (&raw const DHCP_XID).read() };
    if dst_port == 68
        && frame.len() >= dhcp + 240
        && frame[dhcp] == 2
        && frame[dhcp + 4..dhcp + 8] == xid
        && frame[dhcp + 236..dhcp + 240] == [0x63, 0x82, 0x53, 0x63]
    {
        let p = &raw mut OFFERED_IP;
        unsafe { (*p).copy_from_slice(&frame[dhcp + 16..dhcp + 20]) }; // yiaddr
        unsafe { (&raw mut DHCP_OFFER_SEEN).write(true) };
    }

    sock.recv(); // gated: only $Bound counts it
}

/// Pipeline leaf (`$Tcp`): hand the segment to the TCP layer, which parses it
/// and drives the `TcpConnection` FSM.
pub fn on_tcp(_pkt: RxDescriptor) {
    crate::tcp::on_segment(rx_frame());
}

/// Pump one received frame through the `RxPipeline`: copy it into the native
/// RX buffer, parse its descriptor, and `deliver` it to the Frame classifier
/// (which dispatches to `on_arp`/`on_icmp`/`on_udp`). Returns false if no frame
/// was waiting.
fn pump() -> bool {
    let p = &raw mut RX_FRAME;
    let buf = unsafe { &mut *p };
    match virtio_net::poll_rx(buf) {
        Some(n) => {
            unsafe { (&raw mut RX_LEN).write(n) };
            let desc = parse_descriptor(rx_frame());
            pipeline().deliver(desc);
            true
        }
        None => false,
    }
}

// --- IPv4 + ICMP echo (B5 Step 2b) -----------------------------------------

/// Internet checksum (RFC 1071): one's-complement sum of 16-bit big-endian
/// words, with the carries folded in, then complemented. Used for the IPv4
/// header and the ICMP message.
fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8; // odd trailing byte, high-padded
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

const PING_PAYLOAD: [u8; 8] = *b"frameos!";
const PING_ID: u16 = 0x1234;

/// Send one ICMP echo request to the gateway (Ethernet → IPv4 → ICMP), using
/// the gateway MAC resolved by `ArpResolver`. 50 bytes: 14 + 20 + 8 + 8.
fn send_ping(seq: u16) {
    let gw = gateway_mac();
    let mac = virtio_net::mac();
    let mut f = [0u8; 14 + 20 + 8 + 8];

    // Ethernet header.
    f[0..6].copy_from_slice(&gw);
    f[6..12].copy_from_slice(&mac);
    f[12..14].copy_from_slice(&ETHERTYPE_IPV4);

    // IPv4 header (offset 14, no options → IHL 5).
    let total_len = (20 + 8 + PING_PAYLOAD.len()) as u16;
    f[14] = 0x45; // version 4, IHL 5
    f[16..18].copy_from_slice(&total_len.to_be_bytes());
    f[20..22].copy_from_slice(&0x4000u16.to_be_bytes()); // don't-fragment
    f[22] = 64; // TTL
    f[23] = IP_PROTO_ICMP;
    f[26..30].copy_from_slice(&GUEST_IP); // src
    f[30..34].copy_from_slice(&GATEWAY_IP); // dst
    let ip_csum = checksum(&f[14..34]); // checksum field (24..26) still zero
    f[24..26].copy_from_slice(&ip_csum.to_be_bytes());

    // ICMP echo request (offset 34).
    f[34] = ICMP_ECHO_REQUEST;
    f[38..40].copy_from_slice(&PING_ID.to_be_bytes());
    f[40..42].copy_from_slice(&seq.to_be_bytes());
    f[42..50].copy_from_slice(&PING_PAYLOAD);
    let icmp_csum = checksum(&f[34..50]); // checksum field (36..38) still zero
    f[36..38].copy_from_slice(&icmp_csum.to_be_bytes());

    virtio_net::tx_frame(&f);
}

/// Is `frame` an ICMP echo reply from the gateway to us, matching `seq`?
fn is_icmp_echo_reply(frame: &[u8], seq: u16) -> bool {
    if frame.len() < 14 + 20 + 8 || frame[12..14] != ETHERTYPE_IPV4 {
        return false;
    }
    if frame[14] >> 4 != 4 || frame[14 + 9] != IP_PROTO_ICMP {
        return false;
    }
    if frame[26..30] != GATEWAY_IP || frame[30..34] != GUEST_IP {
        return false; // not from the gateway to us
    }
    let icmp = 14 + (frame[14] & 0x0F) as usize * 4; // honor IHL
    frame.len() >= icmp + 8
        && frame[icmp] == ICMP_ECHO_REPLY
        && frame[icmp + 4..icmp + 6] == PING_ID.to_be_bytes()
        && frame[icmp + 6..icmp + 8] == seq.to_be_bytes()
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

    unsafe {
        (&raw mut ARP_FAILED).write(false);
        (&raw mut ARP_GATEWAY_SEEN).write(false);
    }

    interrupts::enable();
    // Construction runs $Incomplete.$>() → first request sent + timer armed.
    let mut arpr = ArpResolver::__create();
    let overall = interrupts::ticks() + 1000;
    while !arpr.is_resolved() && !arpr.is_failed() && interrupts::ticks() < overall {
        if pump() {
            // The RxPipeline classified the frame; on_arp flags a gateway reply.
            if unsafe { (&raw const ARP_GATEWAY_SEEN).read() } {
                arpr.reply(); // -> $Resolved
            }
        } else if arp_timer_expired() {
            arpr.timeout(); // re-send + re-arm, or -> $Failed at the retry cap
        } else {
            interrupts::wait_for_interrupt();
        }
    }
    interrupts::disable();

    if !arpr.is_resolved() {
        serial::writeln("[net] gateway resolution did not complete");
        return;
    }
    serial::write_str("[arp] resolved 10.0.2.2 -> ");
    print_mac(&gateway_mac());
    serial::writeln("");
    serial::writeln("[net] gateway resolved via ArpResolver: ok");

    // B5 Step 2b: ICMP echo (ping) the gateway, now that we have its MAC.
    ping_gateway();

    // B5 Step 3b: bind a UDP socket and do a DHCP DISCOVER → OFFER round-trip
    // (slirp always answers its DHCP server), exercising UDP + the UdpSocket
    // lifecycle + the RxPipeline's $Udp leaf on a real inbound datagram.
    dhcp_exchange();

    // B5 Step 4b–4d: passive-open a TcpConnection on :7 and serve — handshake,
    // echo, and clean close against a client connecting via slirp hostfwd.
    tcp_serve();

    // B5 Step 4e: active open. Connect *out* to 10.0.2.2:9 (the gateway, whose
    // MAC we already resolved), which slirp guestfwd forwards to a host
    // listener — exercising the $SynSent path. Reaches $Established if the
    // harness is listening; a fast RST (no listener) just falls through.
    tcp_connect();
}

/// Active-open a connection to the gateway:9 (slirp guestfwd → host listener)
/// and pump until it reaches $Established ($SynSent → … ), or $Closed (no
/// listener → RST), or a timeout. Exercises the client side of the FSM.
fn tcp_connect() {
    // slirp uses one MAC for all its virtual addresses, so we reach the
    // guestfwd address (10.0.2.100) via the gateway MAC we already resolved.
    let gw = gateway_mac();
    serial::writeln("[tcp] connecting to 10.0.2.100:9 (active open)");
    crate::tcp::connect(gw, [10, 0, 2, 100], 9, 50000); // $Closed → $SynSent (SYN sent)
    interrupts::enable();
    let start = interrupts::ticks();
    let mut announced = false;
    loop {
        if !pump() {
            interrupts::wait_for_interrupt();
        }
        crate::tcp::drain_timers(); // retransmit the SYN if it's lost
        let now = interrupts::ticks();
        if crate::tcp::is_established() && !announced {
            serial::writeln("[tcp] connected (active open)");
            announced = true;
        }
        if announced {
            if now - start > 100 {
                break; // linger ~1s after connecting
            }
        } else if crate::tcp::is_closed() {
            break; // RST'd (no listener / no guestfwd) — nothing to wait for
        } else if now - start > 150 {
            break; // cap (~1.5s): no guestfwd on this boot (every non-active test)
        }
    }
    interrupts::disable();
}

/// Listen on :7 and serve: pump received frames through the RxPipeline (→ the
/// $Tcp leaf → the TcpConnection FSM), completing the handshake and echoing the
/// client's data. slirp accepts host connections locally before the guest
/// handshakes, so a client's abandoned retries can leave us `$Established` on a
/// dead connection — if a connection establishes but echoes nothing within
/// ~0.5s, we recycle it (rst → re-listen) to accept the live one. We bail ~1s
/// after the echo, or ~1s after seeing no TCP at all (every non-TCP boot).
fn tcp_serve() {
    crate::tcp::listen(7);
    interrupts::enable();
    let start = interrupts::ticks();
    let mut announced = false;
    let mut conn_at: u64 = 0; // when the current connection established (0 = none)
    let mut done_at: u64 = 0; // when our echo went out (0 = not yet)
    let mut closing = false; // we've actively closed after the echo
    let mut closed_logged = false;
    loop {
        if !pump() {
            interrupts::wait_for_interrupt();
        }
        crate::tcp::drain_timers(); // fire retransmit / TIME_WAIT timeouts
        let now = interrupts::ticks();

        if crate::tcp::is_established() {
            if conn_at == 0 {
                conn_at = now;
                if !announced {
                    serial::writeln("[tcp] established");
                    announced = true;
                }
            }
            if done_at == 0 && crate::tcp::echoes() > 0 {
                done_at = now;
                // Actively close after the echo — drives $FinWait1 → … →
                // $TimeWait → (2·MSL timer) → $Closed, exercising the timer wheel.
                crate::tcp::close();
                closing = true;
            } else if done_at == 0 && now - conn_at > 50 {
                crate::tcp::relisten(); // idle/dead connection → accept the next
                conn_at = 0;
            }
        }

        if closing && !closed_logged && crate::tcp::is_closed() {
            serial::writeln("[tcp] closed");
            closed_logged = true;
        }

        if closed_logged {
            break; // clean close complete (handshake + echo + close)
        } else if done_at != 0 {
            if now - done_at > 200 {
                break; // safety: close didn't settle within ~2s
            }
        } else if !crate::tcp::saw_tcp() {
            if now - start > 100 {
                break; // no client knocked within ~1s → not a TCP test
            }
        } else if now - start > 400 {
            break; // overall cap (~4s): handshake-only client, or recycling window
        }
    }
    interrupts::disable();
}

/// B5 Step 2b: send an ICMP echo request to the gateway and wait for the reply
/// (Ethernet → IPv4 → ICMP, with both checksums). slirp answers ping to its
/// gateway address (10.0.2.2) deterministically. This is the ICMP *client*
/// path; answering inbound pings (the responder, B5-3) lands with TAP, where
/// inbound ICMP can actually reach the guest.
fn ping_gateway() {
    serial::writeln("[icmp] ping 10.0.2.2 seq 0");
    unsafe {
        (&raw mut EXPECTED_PING_SEQ).write(0);
        (&raw mut ICMP_REPLY_SEEN).write(false);
    }
    send_ping(0);

    interrupts::enable();
    let deadline = interrupts::ticks() + 300;
    while interrupts::ticks() < deadline {
        if pump() {
            // The RxPipeline classified the frame; on_icmp flags our echo reply.
            if unsafe { (&raw const ICMP_REPLY_SEEN).read() } {
                break;
            }
        } else {
            interrupts::wait_for_interrupt();
        }
    }
    interrupts::disable();

    let got = unsafe { (&raw const ICMP_REPLY_SEEN).read() };
    if got {
        serial::writeln("[icmp] reply from 10.0.2.2 seq 0");
        serial::writeln("[net] ping ok");
    } else {
        serial::writeln("[icmp] no reply (timeout)");
    }
}

// --- UDP + DHCP (B5 Step 3b) -----------------------------------------------

/// Send a broadcast DHCP DISCOVER (Ethernet → IPv4 → UDP :68→:67 → BOOTP). The
/// UDP checksum is left 0 (optional for IPv4); the broadcast flag asks slirp to
/// broadcast its reply (we have no IP yet).
fn send_dhcp_discover() {
    let mac = virtio_net::mac();
    let xid = unsafe { (&raw const DHCP_XID).read() };
    // 14 eth + 20 ip + 8 udp + 244 dhcp (236 BOOTP + 4 cookie + 3 opt53 + 1 end).
    let mut f = [0u8; 14 + 20 + 8 + 244];

    // Ethernet: broadcast.
    f[0..6].copy_from_slice(&[0xFF; 6]);
    f[6..12].copy_from_slice(&mac);
    f[12..14].copy_from_slice(&[0x08, 0x00]); // IPv4

    // IPv4: 0.0.0.0 → 255.255.255.255, proto UDP.
    let total_len = (20 + 8 + 244) as u16;
    f[14] = 0x45;
    f[16..18].copy_from_slice(&total_len.to_be_bytes());
    f[22] = 64; // TTL
    f[23] = 17; // UDP
    f[30..34].copy_from_slice(&[255, 255, 255, 255]); // dst
    let ip_csum = checksum(&f[14..34]);
    f[24..26].copy_from_slice(&ip_csum.to_be_bytes());

    // UDP: 68 → 67, checksum 0 (optional for IPv4).
    let udp = 34;
    f[udp..udp + 2].copy_from_slice(&68u16.to_be_bytes());
    f[udp + 2..udp + 4].copy_from_slice(&67u16.to_be_bytes());
    f[udp + 4..udp + 6].copy_from_slice(&((8 + 244) as u16).to_be_bytes());

    // BOOTP/DHCP.
    let d = udp + 8;
    f[d] = 1; // op = BOOTREQUEST
    f[d + 1] = 1; // htype = Ethernet
    f[d + 2] = 6; // hlen
    f[d + 4..d + 8].copy_from_slice(&xid);
    f[d + 10..d + 12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast flag
    f[d + 28..d + 34].copy_from_slice(&mac); // chaddr
    f[d + 236..d + 240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]); // magic cookie
    f[d + 240..d + 243].copy_from_slice(&[53, 1, 1]); // option 53: DHCP DISCOVER
    f[d + 243] = 0xFF; // option 255: end

    virtio_net::tx_frame(&f);
}

/// B5 Step 3b: bind a UDP socket on the DHCP client port and DISCOVER. slirp's
/// DHCP server answers with an OFFER, which the RxPipeline classifies (IPv4 →
/// UDP) and delivers to the bound socket (`on_udp` → `recv()`); we latch + log
/// the offered IP.
fn dhcp_exchange() {
    udp_sock().bind(68); // $Unbound → $Bound
    serial::writeln("[udp] socket bound on :68");
    unsafe { (&raw mut DHCP_OFFER_SEEN).write(false) };
    serial::writeln("[dhcp] DISCOVER");
    send_dhcp_discover();

    interrupts::enable();
    let deadline = interrupts::ticks() + 300;
    while interrupts::ticks() < deadline {
        if pump() {
            if unsafe { (&raw const DHCP_OFFER_SEEN).read() } {
                break;
            }
        } else {
            interrupts::wait_for_interrupt();
        }
    }
    interrupts::disable();

    if unsafe { (&raw const DHCP_OFFER_SEEN).read() } {
        let ip = unsafe { (&raw const OFFERED_IP).read() };
        serial::write_str("[dhcp] OFFER: ");
        serial::write_u32_decimal(ip[0] as u32);
        serial::write_byte(b'.');
        serial::write_u32_decimal(ip[1] as u32);
        serial::write_byte(b'.');
        serial::write_u32_decimal(ip[2] as u32);
        serial::write_byte(b'.');
        serial::write_u32_decimal(ip[3] as u32);
        serial::writeln("");
        serial::write_str("[udp] datagram delivered to socket :68 (count ");
        serial::write_u32_decimal(udp_sock().received());
        serial::writeln(")");
        serial::writeln("[net] DHCP offer via UdpSocket: ok");
    } else {
        serial::writeln("[dhcp] no OFFER (timeout)");
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
