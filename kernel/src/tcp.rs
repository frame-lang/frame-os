// kernel/src/tcp.rs
//
// Native TCP mechanism (B5 Step 4): segment parse/encode + checksum (with the
// IPv4 pseudo-header), the single connection's sequence state + peer info, the
// retransmit / TIME_WAIT timers, and the actions the `TcpConnection` FSM calls.
// Frame owns *which state and what each segment means*; this owns the *bytes*
// and the *arithmetic*.
//
// Step 4a wires one connection in `$Listen` (no client yet — the live handshake
// is 4b); the FSM is validated by host behavioral tests. The senders build real
// segments and are first exercised live at 4b.

use crate::frame_systems::TcpConnection;
use crate::{interrupts, serial, virtio_net};

/// A parsed TCP segment — the descriptor the `RxPipeline` `$Tcp` leaf hands to
/// the FSM via `segment(seg)`. Flags + lengths are what the FSM routes on; the
/// payload bytes stay in the native RX buffer at `payload_off`.
#[derive(Clone, Copy, Default, Debug)]
pub struct TcpSegment {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack_num: u32,
    pub syn: bool,
    pub ack: bool,
    pub fin: bool,
    pub rst: bool,
    pub window: u16,
    pub payload_off: usize,
    pub payload_len: usize,
}

// TCP flag bits (in the data-offset/flags byte at TCP header offset 13).
const F_FIN: u8 = 0x01;
const F_SYN: u8 = 0x02;
const F_RST: u8 = 0x04;
const F_ACK: u8 = 0x10;

const IP_PROTO_TCP: u8 = 6;
const LOCAL_IP: [u8; 4] = [10, 0, 2, 15]; // QEMU slirp guest address
const RCV_WINDOW: u16 = 4096;
const INITIAL_SND: u32 = 0x1000; // our ISN (fixed; fine for one connection)
const RETRANSMIT_TICKS: u64 = 100;
const TIMEWAIT_TICKS: u64 = 25; // short MSL for the test (real TCP is 2*MSL)

// Single-connection native state (B5 Step 4a; a table arrives later).
static mut CONN: Option<TcpConnection> = None;
static mut LOCAL_PORT: u16 = 0;
static mut PEER_MAC: [u8; 6] = [0; 6];
static mut PEER_IP: [u8; 4] = [0; 4];
static mut PEER_PORT: u16 = 0;
static mut SND_NXT: u32 = INITIAL_SND; // our next sequence number
static mut RCV_NXT: u32 = 0; // next sequence we expect from the peer (what we ACK)
static mut RETRANSMIT_AT: u64 = 0; // 0 = disarmed
static mut TIMEWAIT_AT: u64 = 0;
static mut SAW_TCP: bool = false; // any inbound TCP segment seen (serve loop)

// The most recent received payload, stashed by on_segment so the echo "app"
// (deliver_data) can send it back. Single-flight, like the rest.
const MAX_PAYLOAD: usize = 512;
static mut LAST_PAYLOAD: [u8; MAX_PAYLOAD] = [0; MAX_PAYLOAD];
static mut LAST_PAYLOAD_LEN: usize = 0;
static mut ECHOES: u32 = 0; // total payloads echoed (lets the serve loop know it got the live conn)

fn conn() -> &'static mut TcpConnection {
    let p = &raw mut CONN;
    unsafe { (*p).get_or_insert_with(TcpConnection::__create) }
}

// --- helpers ---------------------------------------------------------------

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn rd<T: Copy>(p: *const T) -> T {
    unsafe { p.read() }
}
fn wr<T>(p: *mut T, v: T) {
    unsafe { p.write(v) }
}

/// Internet checksum over `data` seeded with `init` (for the pseudo-header).
fn sum16(data: &[u8], mut sum: u32) -> u16 {
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

// --- parse -----------------------------------------------------------------

/// Parse the TCP segment in `frame` (Ethernet → IPv4 → TCP). On a SYN, latch
/// the peer's MAC/IP/port + initial sequence so our replies can address it.
pub fn parse_segment(frame: &[u8]) -> TcpSegment {
    let mut s = TcpSegment::default();
    if frame.len() < 14 + 20 {
        return s;
    }
    let ihl = (frame[14] & 0x0F) as usize * 4;
    let t = 14 + ihl;
    if frame.len() < t + 20 {
        return s;
    }
    let data_off = ((frame[t + 12] >> 4) & 0x0F) as usize * 4;
    let flags = frame[t + 13];
    s.src_port = be16(frame, t);
    s.dst_port = be16(frame, t + 2);
    s.seq = be32(frame, t + 4);
    s.ack_num = be32(frame, t + 8);
    s.window = be16(frame, t + 14);
    s.syn = flags & F_SYN != 0;
    s.ack = flags & F_ACK != 0;
    s.fin = flags & F_FIN != 0;
    s.rst = flags & F_RST != 0;
    s.payload_off = t + data_off;
    s.payload_len = frame.len().saturating_sub(t + data_off);

    // Latch the peer + advance our receive sequence on a SYN (it occupies 1 seq).
    if s.syn {
        wr(&raw mut PEER_MAC, {
            let mut m = [0u8; 6];
            m.copy_from_slice(&frame[6..12]);
            m
        });
        wr(&raw mut PEER_IP, {
            let mut ip = [0u8; 4];
            ip.copy_from_slice(&frame[26..30]);
            ip
        });
        wr(&raw mut PEER_PORT, s.src_port);
        wr(&raw mut RCV_NXT, s.seq.wrapping_add(1));
    } else {
        // Account for any in-order payload we're about to deliver/ACK.
        let rcv = rd(&raw const RCV_NXT);
        if s.seq == rcv && s.payload_len > 0 {
            wr(&raw mut RCV_NXT, rcv.wrapping_add(s.payload_len as u32));
        }
        if s.fin {
            wr(&raw mut RCV_NXT, rd(&raw const RCV_NXT).wrapping_add(1));
        }
    }
    s
}

// --- segment senders (the FSM's actions) -----------------------------------

/// Build + transmit a TCP segment with `flags` and `payload` to the peer.
fn send(flags: u8, payload: &[u8]) {
    let mac = virtio_net::mac();
    let peer_mac = rd(&raw const PEER_MAC);
    let peer_ip = rd(&raw const PEER_IP);
    let local_port = rd(&raw const LOCAL_PORT);
    let peer_port = rd(&raw const PEER_PORT);
    let snd = rd(&raw const SND_NXT);
    let rcv = rd(&raw const RCV_NXT);

    let tcp_len = 20 + payload.len();
    let mut f = [0u8; 14 + 20 + 20 + MAX_PAYLOAD];
    let total = 14 + 20 + tcp_len;

    // Ethernet.
    f[0..6].copy_from_slice(&peer_mac);
    f[6..12].copy_from_slice(&mac);
    f[12..14].copy_from_slice(&[0x08, 0x00]);

    // IPv4.
    f[14] = 0x45;
    f[16..18].copy_from_slice(&((20 + tcp_len) as u16).to_be_bytes());
    f[22] = 64;
    f[23] = IP_PROTO_TCP;
    f[26..30].copy_from_slice(&LOCAL_IP);
    f[30..34].copy_from_slice(&peer_ip);
    let ip_csum = sum16(&f[14..34], 0);
    f[24..26].copy_from_slice(&ip_csum.to_be_bytes());

    // TCP.
    let t = 34;
    f[t..t + 2].copy_from_slice(&local_port.to_be_bytes());
    f[t + 2..t + 4].copy_from_slice(&peer_port.to_be_bytes());
    f[t + 4..t + 8].copy_from_slice(&snd.to_be_bytes());
    f[t + 8..t + 12].copy_from_slice(&rcv.to_be_bytes());
    f[t + 12] = 0x50; // data offset = 5 words (20 bytes)
    f[t + 13] = flags;
    f[t + 14..t + 16].copy_from_slice(&RCV_WINDOW.to_be_bytes());
    f[t + 20..t + 20 + payload.len()].copy_from_slice(payload);

    // TCP checksum over the pseudo-header + segment.
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&LOCAL_IP);
    pseudo[4..8].copy_from_slice(&peer_ip);
    pseudo[9] = IP_PROTO_TCP;
    pseudo[10..12].copy_from_slice(&(tcp_len as u16).to_be_bytes());
    let mut psum: u32 = 0;
    let mut i = 0;
    while i + 1 < pseudo.len() {
        psum += u16::from_be_bytes([pseudo[i], pseudo[i + 1]]) as u32;
        i += 2;
    }
    let tcp_csum = sum16(&f[t..t + tcp_len], psum);
    f[t + 16..t + 18].copy_from_slice(&tcp_csum.to_be_bytes());

    // SYN and FIN each consume one sequence number.
    if flags & (F_SYN | F_FIN) != 0 {
        wr(&raw mut SND_NXT, snd.wrapping_add(1));
    }
    wr(&raw mut SND_NXT, rd(&raw const SND_NXT).wrapping_add(payload.len() as u32));

    virtio_net::tx_frame(&f[..total]);
}

pub fn send_syn() {
    send(F_SYN, &[]);
}
pub fn send_syn_ack() {
    send(F_SYN | F_ACK, &[]);
}
pub fn send_ack() {
    send(F_ACK, &[]);
}
pub fn send_fin() {
    send(F_FIN | F_ACK, &[]);
}

/// The demo's echo "application": send the most recently received payload
/// (stashed by `on_segment`) back to the peer. The data segment piggybacks the
/// ACK (its `ack_num` = `RCV_NXT`, already advanced past the received bytes).
pub fn deliver_data() {
    let n = rd(&raw const LAST_PAYLOAD_LEN);
    if n == 0 {
        return;
    }
    let buf = {
        let p = &raw const LAST_PAYLOAD;
        let full: &[u8] = unsafe { &*p };
        &full[..n]
    };
    send(F_ACK, buf);
    wr(&raw mut ECHOES, rd(&raw const ECHOES) + 1);
    serial::write_str("[tcp] echoed ");
    serial::write_u32_decimal(n as u32);
    serial::writeln(" bytes");
}

/// Total payloads echoed so far (cumulative across connections).
pub fn echoes() -> u32 {
    rd(&raw const ECHOES)
}

/// Recycle the connection: force it to `$Closed` (via the RST funnel — no
/// packet sent, just `on_reset` + the transition), reset the sequence state,
/// and passive-open again. Used by the serve loop to drop an idle/dead
/// connection (slirp accepts host connections locally before the guest
/// handshakes, so abandoned retries can leave us stuck `$Established`) and
/// accept the live one.
pub fn relisten() {
    conn().rst(); // any active state -> $Closed via the $Open funnel
    on_reset();
    wr(&raw mut SND_NXT, INITIAL_SND);
    conn().open_passive(); // $Closed -> $Listen
}

/// On reset, clear the timers + sequence state for the next connection.
pub fn on_reset() {
    wr(&raw mut RETRANSMIT_AT, 0);
    wr(&raw mut TIMEWAIT_AT, 0);
    wr(&raw mut SND_NXT, INITIAL_SND);
}

// --- timers (armed in enter handlers; fired by the wheel via the net loop) --

pub fn arm_retransmit() {
    wr(&raw mut RETRANSMIT_AT, interrupts::ticks() + RETRANSMIT_TICKS);
}
pub fn cancel_retransmit() {
    wr(&raw mut RETRANSMIT_AT, 0);
}
pub fn arm_timewait() {
    wr(&raw mut TIMEWAIT_AT, interrupts::ticks() + TIMEWAIT_TICKS);
}

// (The timer-wheel drain that reads RETRANSMIT_AT/TIMEWAIT_AT and fires
// `timeout()` is wired into the net loop at Step 4d, where a live connection
// actually runs its timers.)

// --- delivery (called by the RxPipeline $Tcp leaf via net::on_tcp) ----------

/// Drive the FSM from a received TCP segment: an RST funnels to `$Closed`;
/// anything else is processed per-state by `segment()`.
pub fn on_segment(frame: &[u8]) {
    let seg = parse_segment(frame);
    if rd(&raw const LOCAL_PORT) != 0 && seg.dst_port != rd(&raw const LOCAL_PORT) {
        return; // not for our listening port
    }
    wr(&raw mut SAW_TCP, true);

    // Stash any payload so the echo app (deliver_data) can send it back. Only
    // in-order data (seq == RCV_NXT before parse advanced it) is echoed.
    let n = seg.payload_len.min(MAX_PAYLOAD);
    if n > 0 && seg.payload_off + n <= frame.len() {
        let p = &raw mut LAST_PAYLOAD;
        let dst: &mut [u8] = unsafe { &mut *p };
        dst[..n].copy_from_slice(&frame[seg.payload_off..seg.payload_off + n]);
        wr(&raw mut LAST_PAYLOAD_LEN, n);
    } else {
        wr(&raw mut LAST_PAYLOAD_LEN, 0);
    }

    if seg.rst {
        conn().rst();
    } else {
        conn().segment(seg);
    }
}

/// Whether any inbound TCP segment has been seen (lets the serve loop bail
/// fast on a boot where no client connects).
pub fn saw_tcp() -> bool {
    rd(&raw const SAW_TCP)
}

/// Whether the connection is in `$Established`.
pub fn is_established() -> bool {
    conn().state() == "Established"
}

/// Passive-open the connection on `port` ($Closed → $Listen).
pub fn listen(port: u16) {
    wr(&raw mut LOCAL_PORT, port);
    conn().open_passive();
    serial::write_str("[tcp] listening on :");
    serial::write_u32_decimal(port as u32);
    serial::write_str(" (");
    serial::writeln(&conn().state());
    serial::writeln(")");
}
