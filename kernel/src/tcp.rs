// kernel/src/tcp.rs
//
// Native TCP mechanism (B5 Step 4; connection table at R2b). Segment
// parse/encode + checksum (with the IPv4 pseudo-header), each connection's
// sequence state + peer info, the retransmit / TIME_WAIT timers, and the actions
// the `TcpConnection` FSM calls. Frame owns *which state and what each segment
// means*; this owns the *bytes* and the *arithmetic*.
//
// R2b makes this a **connection table**: an array of slots, each a full
// `TcpConnection` FSM instance + its own per-connection state. The FSM's actions
// (`send_syn_ack`, `deliver_data`, …) operate on the *current* connection — a
// slot index set by `on_segment` before it dispatches, and used by the action
// functions. Dispatch is single-threaded on the BSP's serve loop, so an ambient
// "current connection" is sound and keeps the FSM unchanged. Connections are
// keyed by local (destination) port: the kernel listens on a small range of
// ports, one connection per port — robust over slirp (each forwarded port is its
// own slirp connection). The active-open client (B5 Step 4e) uses a dedicated
// slot.

use crate::frame_systems::TcpConnection;
use crate::{interrupts, serial, virtio_net};

/// A parsed TCP segment — the descriptor the `RxPipeline` `$Tcp` leaf hands to
/// the FSM via `segment(seg)`.
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

const F_FIN: u8 = 0x01;
const F_SYN: u8 = 0x02;
const F_RST: u8 = 0x04;
const F_ACK: u8 = 0x10;

const IP_PROTO_TCP: u8 = 6;
const LOCAL_IP: [u8; 4] = [10, 0, 2, 15];
const RCV_WINDOW: u16 = 4096;
const INITIAL_SND: u32 = 0x1000;
const RETRANSMIT_TICKS: u64 = 100;
const TIMEWAIT_TICKS: u64 = 25;
const MAX_PAYLOAD: usize = 512;

/// Connection table size: 4 server slots (a port range) + 1 active-open client.
pub const SERVER_SLOTS: usize = 4;
const CLIENT_SLOT: usize = SERVER_SLOTS;
const MAX_CONN: usize = SERVER_SLOTS + 1;
/// First local port the server listens on; server slot `i` owns `BASE_PORT + i`.
pub const BASE_PORT: u16 = 7;

/// One connection's state: its FSM instance + sequence/peer info + timers + the
/// last received payload (for the echo "app").
struct Conn {
    fsm: Option<TcpConnection>,
    in_use: bool,    // a peer is bound (vs. an idle listener / free slot)
    listening: bool, // an unbound listener waiting for a SYN on `local_port`
    local_port: u16,
    peer_mac: [u8; 6],
    peer_ip: [u8; 4],
    peer_port: u16,
    snd_nxt: u32,
    rcv_nxt: u32,
    retransmit_at: u64,
    timewait_at: u64,
    last_payload: [u8; MAX_PAYLOAD],
    last_payload_len: usize,
    echoes: u32,
}

const CONN_INIT: Conn = Conn {
    fsm: None,
    in_use: false,
    listening: false,
    local_port: 0,
    peer_mac: [0; 6],
    peer_ip: [0; 4],
    peer_port: 0,
    snd_nxt: INITIAL_SND,
    rcv_nxt: 0,
    retransmit_at: 0,
    timewait_at: 0,
    last_payload: [0; MAX_PAYLOAD],
    last_payload_len: 0,
    echoes: 0,
};

static mut CONNS: [Conn; MAX_CONN] = [const { CONN_INIT }; MAX_CONN];
static mut CURRENT: usize = 0; // the connection the FSM action functions operate on
static mut SAW_TCP: bool = false;

fn slot(i: usize) -> &'static mut Conn {
    let base = &raw mut CONNS as *mut Conn;
    unsafe { &mut *base.add(i) }
}
/// The current connection (set by `on_segment` / the demo before dispatching).
fn cur() -> &'static mut Conn {
    slot(unsafe { (&raw const CURRENT).read() })
}
fn set_current(i: usize) {
    unsafe { (&raw mut CURRENT).write(i) };
}
fn fsm(i: usize) -> &'static mut TcpConnection {
    slot(i).fsm.get_or_insert_with(TcpConnection::__create)
}

// --- helpers ---------------------------------------------------------------

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

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

/// Parse the TCP fields of `frame` (Ethernet → IPv4 → TCP) into a `TcpSegment`.
/// Pure: no connection state is touched (the slot is resolved afterward).
fn parse(frame: &[u8]) -> TcpSegment {
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
    s
}

// --- segment senders (the FSM's actions, operating on the current conn) -----

/// Build + transmit a TCP segment with `flags` and `payload` from the current
/// connection to its peer.
fn send(flags: u8, payload: &[u8]) {
    let c = cur();
    let mac = virtio_net::mac();
    let tcp_len = 20 + payload.len();
    let mut f = [0u8; 14 + 20 + 20 + MAX_PAYLOAD];
    let total = 14 + 20 + tcp_len;

    f[0..6].copy_from_slice(&c.peer_mac);
    f[6..12].copy_from_slice(&mac);
    f[12..14].copy_from_slice(&[0x08, 0x00]);

    f[14] = 0x45;
    f[16..18].copy_from_slice(&((20 + tcp_len) as u16).to_be_bytes());
    f[22] = 64;
    f[23] = IP_PROTO_TCP;
    f[26..30].copy_from_slice(&LOCAL_IP);
    f[30..34].copy_from_slice(&c.peer_ip);
    let ip_csum = sum16(&f[14..34], 0);
    f[24..26].copy_from_slice(&ip_csum.to_be_bytes());

    let t = 34;
    f[t..t + 2].copy_from_slice(&c.local_port.to_be_bytes());
    f[t + 2..t + 4].copy_from_slice(&c.peer_port.to_be_bytes());
    f[t + 4..t + 8].copy_from_slice(&c.snd_nxt.to_be_bytes());
    f[t + 8..t + 12].copy_from_slice(&c.rcv_nxt.to_be_bytes());
    f[t + 12] = 0x50;
    f[t + 13] = flags;
    f[t + 14..t + 16].copy_from_slice(&RCV_WINDOW.to_be_bytes());
    f[t + 20..t + 20 + payload.len()].copy_from_slice(payload);

    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&LOCAL_IP);
    pseudo[4..8].copy_from_slice(&c.peer_ip);
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

    if flags & (F_SYN | F_FIN) != 0 {
        c.snd_nxt = c.snd_nxt.wrapping_add(1);
    }
    c.snd_nxt = c.snd_nxt.wrapping_add(payload.len() as u32);

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

/// The echo "application": send the current connection's most recently received
/// payload back to its peer (piggybacking the ACK).
pub fn deliver_data() {
    let c = cur();
    let n = c.last_payload_len;
    if n == 0 {
        return;
    }
    let buf = {
        let p = &raw const c.last_payload;
        let full: &[u8] = unsafe { &*p };
        let mut tmp = [0u8; MAX_PAYLOAD];
        tmp[..n].copy_from_slice(&full[..n]);
        tmp
    };
    send(F_ACK, &buf[..n]);
    c.echoes += 1;
    serial::write_str("[tcp] echoed ");
    serial::write_u32_decimal(n as u32);
    serial::writeln(" bytes");
}

/// On reset, clear the current connection's timers + sequence state.
pub fn on_reset() {
    let c = cur();
    c.retransmit_at = 0;
    c.timewait_at = 0;
    c.snd_nxt = INITIAL_SND;
}

// --- timers (armed in enter handlers; fired by the wheel via the net loop) --

pub fn arm_retransmit() {
    cur().retransmit_at = interrupts::ticks() + RETRANSMIT_TICKS;
}
pub fn cancel_retransmit() {
    cur().retransmit_at = 0;
}
pub fn arm_timewait() {
    cur().timewait_at = interrupts::ticks() + TIMEWAIT_TICKS;
}

/// Check every connection's timers and fire `timeout()` on any that expired —
/// the native "wheel," driven from the serve loop (post/drain), never an ISR.
pub fn drain_timers() {
    let now = interrupts::ticks();
    for i in 0..MAX_CONN {
        let (rt, tw, used) = {
            let c = slot(i);
            (c.retransmit_at, c.timewait_at, c.in_use)
        };
        if !used {
            continue;
        }
        if rt != 0 && now >= rt {
            slot(i).retransmit_at = now + RETRANSMIT_TICKS;
            set_current(i);
            fsm(i).timeout();
        }
        if tw != 0 && now >= tw {
            slot(i).timewait_at = 0;
            set_current(i);
            fsm(i).timeout();
        }
    }
}

// --- per-connection queries (slot-indexed) ---------------------------------

/// Whether server slot `i` is in `$Established`.
pub fn is_established(i: usize) -> bool {
    set_current(i);
    fsm(i).state() == "Established"
}
/// Whether slot `i` is back in `$Closed`.
pub fn is_closed(i: usize) -> bool {
    set_current(i);
    fsm(i).state() == "Closed"
}
/// Payloads echoed on slot `i`.
pub fn echoes(i: usize) -> u32 {
    slot(i).echoes
}
/// Actively close slot `i` (the local "app" closes).
pub fn close(i: usize) {
    set_current(i);
    fsm(i).close();
}
/// Whether any inbound TCP segment has been seen at all.
pub fn saw_tcp() -> bool {
    unsafe { (&raw const SAW_TCP).read() }
}

// --- delivery (called by the RxPipeline $Tcp leaf via net::on_tcp) ----------

/// Resolve which connection slot a received segment belongs to: an existing
/// 4-tuple match, or — on a SYN to a listening server port — the listener slot
/// for that port (which then binds to this peer). `None` = no slot (dropped).
fn resolve(frame: &[u8], seg: &TcpSegment) -> Option<usize> {
    let mut src_ip = [0u8; 4];
    src_ip.copy_from_slice(&frame[26..30]);
    // Existing bound connection for this 4-tuple?
    for i in 0..MAX_CONN {
        let c = slot(i);
        if c.in_use
            && c.local_port == seg.dst_port
            && c.peer_port == seg.src_port
            && c.peer_ip == src_ip
        {
            return Some(i);
        }
    }
    // New SYN to a listening port → bind that listener to this peer.
    if seg.syn {
        for i in 0..SERVER_SLOTS {
            let c = slot(i);
            if c.listening && c.local_port == seg.dst_port {
                c.listening = false;
                c.in_use = true;
                c.peer_mac.copy_from_slice(&frame[6..12]);
                c.peer_ip = src_ip;
                c.peer_port = seg.src_port;
                c.snd_nxt = INITIAL_SND;
                return Some(i);
            }
        }
    }
    None
}

/// Drive the right connection's FSM from a received TCP segment.
pub fn on_segment(frame: &[u8]) {
    let seg = parse(frame);
    let Some(i) = resolve(frame, &seg) else {
        return;
    };
    set_current(i);
    unsafe { (&raw mut SAW_TCP).write(true) };
    let c = cur();

    // Advance our receive sequence: SYN occupies 1; in-order payload advances by
    // its length; FIN occupies 1.
    if seg.syn {
        c.rcv_nxt = seg.seq.wrapping_add(1);
    } else {
        if seg.seq == c.rcv_nxt && seg.payload_len > 0 {
            c.rcv_nxt = c.rcv_nxt.wrapping_add(seg.payload_len as u32);
        }
        if seg.fin {
            c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
        }
    }

    // Stash any in-order payload for the echo app.
    let n = seg.payload_len.min(MAX_PAYLOAD);
    if n > 0 && seg.payload_off + n <= frame.len() {
        c.last_payload[..n].copy_from_slice(&frame[seg.payload_off..seg.payload_off + n]);
        c.last_payload_len = n;
    } else {
        c.last_payload_len = 0;
    }

    if seg.rst {
        fsm(i).rst();
    } else {
        fsm(i).segment(seg);
    }
}

// --- setup -----------------------------------------------------------------

/// Passive-open server slot for `port` (slot index = `port - BASE_PORT`):
/// `$Closed → $Listen`, ready to accept a SYN.
pub fn listen(port: u16) {
    let i = (port - BASE_PORT) as usize;
    {
        let c = slot(i);
        c.local_port = port;
        c.listening = true;
        c.in_use = false;
        c.peer_port = 0;
        c.snd_nxt = INITIAL_SND;
    }
    set_current(i);
    fsm(i).open_passive();
    serial::write_str("[tcp] listening on :");
    serial::write_u32_decimal(port as u32);
    serial::writeln("");
}

/// Active-open a connection to `peer_ip:peer_port` from `local_port` (the
/// dedicated client slot): `$Closed → $SynSent` (sends the SYN).
pub fn connect(peer_mac: [u8; 6], peer_ip: [u8; 4], peer_port: u16, local_port: u16) {
    let i = CLIENT_SLOT;
    {
        let c = slot(i);
        c.in_use = true;
        c.listening = false;
        c.local_port = local_port;
        c.peer_mac = peer_mac;
        c.peer_ip = peer_ip;
        c.peer_port = peer_port;
        c.snd_nxt = INITIAL_SND;
    }
    set_current(i);
    fsm(i).rst(); // force $Closed from wherever
    on_reset();
    fsm(i).open_active(); // $Closed → $SynSent
}

/// Whether the active-open client connection is `$Established` / `$Closed`.
pub fn client_established() -> bool {
    is_established(CLIENT_SLOT)
}
pub fn client_closed() -> bool {
    is_closed(CLIENT_SLOT)
}

// --- R2a: per-event allocation measurement at scale ------------------------

/// Create `N` fresh `TcpConnection` FSM instances on the real (spinlocked) kernel
/// heap and drive each through a full passive lifecycle with synthetic segments,
/// counting heap allocations — quantifying Frame's per-event allocation cost.
pub fn scale_stress() {
    const N: u32 = 16;
    set_current(CLIENT_SLOT);
    cur().last_payload_len = 0; // no echo payload → no TX noise from deliver_data

    let syn = TcpSegment {
        syn: true,
        ..TcpSegment::default()
    };
    let ack = TcpSegment {
        ack: true,
        ..TcpSegment::default()
    };
    let data = TcpSegment {
        ack: true,
        payload_len: 8,
        ..TcpSegment::default()
    };
    let finack = TcpSegment {
        fin: true,
        ack: true,
        ..TcpSegment::default()
    };

    let before = crate::allocator::alloc_count();
    let mut dispatches: u64 = 0;
    let mut closed: u32 = 0;
    for _ in 0..N {
        let mut c = TcpConnection::__create();
        c.open_passive();
        c.segment(syn);
        c.segment(ack);
        c.segment(data);
        c.close();
        c.segment(finack);
        c.timeout();
        dispatches += 7;
        if c.state() == "Closed" {
            closed += 1;
        }
    }
    let allocs = crate::allocator::alloc_count() - before;

    serial::write_str("[tcp] scale: ");
    serial::write_u32_decimal(N);
    serial::write_str(" conns, ");
    serial::write_u32_decimal(dispatches as u32);
    serial::write_str(" dispatches, ");
    serial::write_u32_decimal(allocs as u32);
    serial::writeln(" heap allocs");
    serial::write_str("[tcp] scale: ");
    serial::write_u32_decimal((allocs / dispatches.max(1)) as u32);
    serial::write_str(" allocs/dispatch, closed ");
    serial::write_u32_decimal(closed);
    serial::write_str("/");
    serial::write_u32_decimal(N);
    serial::writeln(" connections");
}
