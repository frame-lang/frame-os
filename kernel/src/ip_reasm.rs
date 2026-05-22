// kernel/src/ip_reasm.rs
//
// IPv4 fragment reassembly — the native half of the `IpReassembly` Frame system
// (B5 Step 6 / B5-5). Frame owns the *lifecycle* ($Idle → $Reassembling →
// $Complete | $Expired); this module owns the *bytes*: the reassembly buffer, a
// per-byte coverage map, the "total length known yet" bookkeeping, and the
// reconstruction of the whole datagram once the holes are filled.
//
// Single-flight, like the rest of the B5 demo: one datagram reassembled at a
// time (a `ping -s 4000` over a 1500-byte link MTU is exactly this — three
// fragments of one datagram, in order). A second datagram with a different IP
// identification resets the in-progress state. This is *not* full RFC-815 hole
// management across many concurrent datagrams; it is correct for a single
// datagram with non-overlapping fragments (which is what a host `ping`
// produces), and the coverage map makes "are all the bytes present" a real
// check rather than a fragment count.

use crate::frame_systems::IpReassembly;
use crate::{interrupts, serial};

/// Max reassembled IP *payload* (bytes after the 20-byte IPv4 header). 8 KiB
/// comfortably covers a 4 KiB ping; a larger datagram is dropped (logged).
const REASM_MAX: usize = 8192;
const ETH_HDR: usize = 14;
const IP_HDR: usize = 20;

/// How long (PIT ticks, 100 Hz) we hold a partial datagram before giving up.
const REASM_TIMEOUT_TICKS: u64 = 100;

/// The parsed summary of one IPv4 fragment, threaded into the `IpReassembly`
/// FSM as an enter parameter. (`Clone + Default` for framec's typed enter-arg
/// context.) The payload bytes themselves stay native (`CUR_PAYLOAD`).
#[derive(Clone, Copy, Default, Debug)]
pub struct Fragment {
    /// Byte offset of this fragment's payload within the datagram.
    pub offset: usize,
    /// This fragment's payload length.
    pub len: usize,
    /// MF (more-fragments) flag — false on the final fragment.
    pub more: bool,
    /// IPv4 identification — the datagram key (distinguishes datagrams).
    pub ident: u16,
}

static mut REASM: Option<IpReassembly> = None;

// In-progress reassembly state (single-flight).
static mut ACTIVE: bool = false;
static mut IDENT: u16 = 0;
static mut TOTAL_KNOWN: bool = false;
static mut TOTAL_LEN: usize = 0; // set when the final (MF=0) fragment arrives
static mut COVERED: usize = 0; // count of distinct payload bytes received
static mut FRAGS_SEEN: u32 = 0;
static mut DEADLINE: u64 = 0;

static mut BUF: [u8; REASM_MAX] = [0; REASM_MAX]; // reassembled payload
static mut COVERAGE: [u8; REASM_MAX / 8] = [0; REASM_MAX / 8]; // 1 bit per payload byte
static mut SAVED_HDR: [u8; ETH_HDR + IP_HDR] = [0; ETH_HDR + IP_HDR]; // first fragment's Eth+IP header
static mut CUR_PAYLOAD: [u8; REASM_MAX] = [0; REASM_MAX]; // current fragment's payload, for store()
// The reconstructed full Ethernet frame (Eth + IP + payload) handed to net.
static mut FULL_FRAME: [u8; ETH_HDR + IP_HDR + REASM_MAX] = [0; ETH_HDR + IP_HDR + REASM_MAX];

fn reasm() -> &'static mut IpReassembly {
    let p = &raw mut REASM;
    unsafe { (*p).get_or_insert_with(IpReassembly::__create) }
}

// --- coverage bitmap helpers ----------------------------------------------

fn cov_get(i: usize) -> bool {
    let p = &raw const COVERAGE;
    let map = unsafe { &*p };
    (map[i / 8] >> (i % 8)) & 1 != 0
}

fn cov_set(i: usize) {
    let p = &raw mut COVERAGE;
    let map = unsafe { &mut *p };
    map[i / 8] |= 1 << (i % 8);
}

// --- parsing ---------------------------------------------------------------

/// If `frame` is an IPv4 *fragment* (MF set, or a non-zero fragment offset),
/// return its parsed `Fragment`; otherwise `None` (a whole, unfragmented
/// datagram, which the caller handles directly). Bounds-checked.
pub fn parse_fragment(frame: &[u8]) -> Option<Fragment> {
    if frame.len() < ETH_HDR + IP_HDR || frame[12..14] != [0x08, 0x00] || frame[14] >> 4 != 4 {
        return None;
    }
    let flags_frag = u16::from_be_bytes([frame[14 + 6], frame[14 + 7]]);
    let mf = (flags_frag & 0x2000) != 0;
    let frag_off = (flags_frag & 0x1FFF) as usize * 8;
    if !mf && frag_off == 0 {
        return None; // not fragmented
    }
    let ihl = (frame[14] & 0x0F) as usize * 4;
    let total_ip = u16::from_be_bytes([frame[16], frame[17]]) as usize;
    let pstart = ETH_HDR + ihl;
    let pend = (ETH_HDR + total_ip).min(frame.len());
    let len = pend.saturating_sub(pstart);
    let ident = u16::from_be_bytes([frame[14 + 4], frame[14 + 5]]);
    Some(Fragment {
        offset: frag_off,
        len,
        more: mf,
        ident,
    })
}

// --- driving the FSM (called from net::on_icmp) ----------------------------

/// Feed one IPv4 fragment to the reassembly FSM. Stashes the fragment's payload
/// for `store()` to copy, starts a fresh reassembly when the datagram id
/// changes (or none is active), and dispatches `fragment(frag)` — which enters
/// `$Reassembling`, stores, and (if the holes are now filled) → `$Complete`.
pub fn on_fragment(frame: &[u8]) {
    let Some(frag) = parse_fragment(frame) else {
        return;
    };
    let ihl = (frame[14] & 0x0F) as usize * 4;
    let total_ip = u16::from_be_bytes([frame[16], frame[17]]) as usize;
    let pstart = ETH_HDR + ihl;
    let pend = (ETH_HDR + total_ip).min(frame.len());
    let plen = pend.saturating_sub(pstart);

    // Oversized (or a malformed offset) → drop the whole reassembly.
    if frag.offset + plen > REASM_MAX {
        reset_inactive();
        return;
    }

    // Stash this fragment's payload bytes for store() to consume.
    {
        let p = &raw mut CUR_PAYLOAD;
        let dst = unsafe { &mut *p };
        dst[..plen].copy_from_slice(&frame[pstart..pend]);
    }

    let active = unsafe { (&raw const ACTIVE).read() };
    let cur_ident = unsafe { (&raw const IDENT).read() };
    if !active || cur_ident != frag.ident {
        begin(frag.ident, frame);
    }
    reasm().fragment(frag);
}

/// Start a fresh reassembly for datagram `ident`: clear coverage/total, save the
/// first fragment's Eth+IP header (for reconstruction), arm the timeout, and
/// recreate the FSM at `$Idle`.
fn begin(ident: u16, frame: &[u8]) {
    {
        let p = &raw mut COVERAGE;
        unsafe { (*p).fill(0) };
    }
    {
        let p = &raw mut SAVED_HDR;
        let hdr = unsafe { &mut *p };
        hdr.copy_from_slice(&frame[..ETH_HDR + IP_HDR]);
    }
    unsafe {
        (&raw mut ACTIVE).write(true);
        (&raw mut IDENT).write(ident);
        (&raw mut TOTAL_KNOWN).write(false);
        (&raw mut TOTAL_LEN).write(0);
        (&raw mut COVERED).write(0);
        (&raw mut FRAGS_SEEN).write(0);
        (&raw mut DEADLINE).write(interrupts::ticks() + REASM_TIMEOUT_TICKS);
    }
    let p = &raw mut REASM;
    unsafe { (*p).replace(IpReassembly::__create()) };
}

fn reset_inactive() {
    unsafe { (&raw mut ACTIVE).write(false) };
}

// --- actions called by the IpReassembly Frame system -----------------------

/// `$Reassembling.$>`: copy the current fragment's payload into the reassembly
/// buffer at its offset, marking newly-covered bytes (overlaps don't
/// double-count), and latch the total length when the final fragment arrives.
pub fn store(frag: Fragment) {
    let plen = frag.len;
    let src = {
        let p = &raw const CUR_PAYLOAD;
        unsafe { &*p }
    };
    let dst = {
        let p = &raw mut BUF;
        unsafe { &mut *p }
    };
    let mut covered = unsafe { (&raw const COVERED).read() };
    for (i, &byte) in src[..plen].iter().enumerate() {
        let idx = frag.offset + i;
        if idx >= REASM_MAX {
            break;
        }
        dst[idx] = byte;
        if !cov_get(idx) {
            cov_set(idx);
            covered += 1;
        }
    }
    unsafe { (&raw mut COVERED).write(covered) };
    if !frag.more {
        unsafe {
            (&raw mut TOTAL_KNOWN).write(true);
            (&raw mut TOTAL_LEN).write(frag.offset + plen);
        }
    }
    unsafe { (&raw mut FRAGS_SEEN).write((&raw const FRAGS_SEEN).read() + 1) };
}

/// Guard for `$Reassembling → $Complete`: we've seen the final fragment (so the
/// total length is known) and every payload byte `[0, total)` has arrived.
pub fn is_complete() -> bool {
    unsafe {
        (&raw const TOTAL_KNOWN).read() && (&raw const COVERED).read() == (&raw const TOTAL_LEN).read()
    }
}

/// `$Complete.$>`: reconstruct the whole datagram (saved header with the
/// fragment fields cleared + total length fixed) and hand it to the IP layer for
/// protocol dispatch (here: the ICMP echo responder, which replies to the whole
/// request — fragmenting the reply outbound if it exceeds the MTU).
pub fn on_complete() {
    let total = unsafe { (&raw const TOTAL_LEN).read() };
    let frags = unsafe { (&raw const FRAGS_SEEN).read() };
    serial::write_str("[ip] reassembled ");
    serial::write_u32_decimal(total as u32);
    serial::write_str(" bytes from ");
    serial::write_u32_decimal(frags);
    serial::writeln(" fragments");

    // Build the full Ethernet frame: saved Eth+IP header + reassembled payload.
    let full_len = ETH_HDR + IP_HDR + total;
    {
        let hp = &raw const SAVED_HDR;
        let bp = &raw const BUF;
        let fp = &raw mut FULL_FRAME;
        let hdr = unsafe { &*hp };
        let buf = unsafe { &*bp };
        let full = unsafe { &mut *fp };
        full[..ETH_HDR + IP_HDR].copy_from_slice(hdr);
        full[ETH_HDR + IP_HDR..full_len].copy_from_slice(&buf[..total]);
        // Fix the IP header: total length = 20 + payload, clear flags/frag-offset
        // (it's whole now), zero the checksum (net recomputes before replying).
        let ip_total = (IP_HDR + total) as u16;
        full[16..18].copy_from_slice(&ip_total.to_be_bytes());
        full[14 + 6] = 0;
        full[14 + 7] = 0;
        full[24] = 0;
        full[25] = 0;
    }
    let fp = &raw const FULL_FRAME;
    let full = unsafe { &*fp };
    crate::net::on_reassembled_ipv4(&full[..full_len]);

    reset_inactive();
}

/// `$Expired.$>`: a fragment was lost or too slow — drop the partial buffer.
pub fn on_expired() {
    serial::writeln("[ip] reassembly timed out (partial datagram dropped)");
    reset_inactive();
}

/// Fire the reassembly timeout if its deadline has passed (called from the
/// inbound-serve loop, the post/drain timer idiom). Drives the FSM to $Expired.
pub fn drain_timer() {
    let active = unsafe { (&raw const ACTIVE).read() };
    if active && interrupts::ticks() >= unsafe { (&raw const DEADLINE).read() } {
        reasm().timeout();
    }
}
