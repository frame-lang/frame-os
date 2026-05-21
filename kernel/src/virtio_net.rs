// kernel/src/virtio_net.rs
//
// Legacy virtio-net driver (B5 Step 1). Pure native — the NIC half of the
// networking stack. Speaks the legacy virtio PCI interface (I/O BAR, classic
// 0.9.5 register layout), which QEMU exposes with
// `-device virtio-net-pci,disable-modern=on`.
//
// Two virtqueues: queue 0 = receive (RX), queue 1 = transmit (TX). Each packet
// buffer is prefixed by a 10-byte legacy `virtio_net_hdr` (we negotiate no
// MRG_RXBUF, so the header is the small form). The driver pre-posts RX buffers,
// transmits a raw Ethernet frame, and receives via the same post/drain split as
// virtio-blk (B4): the IRQ handler (`on_irq`) only *posts* (acks the device ISR
// + sets a flag, no Frame dispatch); the kernel *drains* the RX used ring from
// normal context (`poll_rx`).
//
// Step 1 is driver bring-up: init, read the MAC, and do a deterministic ARP
// round-trip against QEMU's slirp gateway (10.0.2.2) to prove TX + RX + the
// post/drain path end to end. ARP/IP/ICMP as Frame systems arrive at Step 2.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::{frames, interrupts, io, pci, pic, serial};

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_NET_DEVICE: u16 = 0x1000; // legacy/transitional virtio-net

// Legacy virtio PCI I/O register offsets (from the I/O BAR base).
const R_DEVICE_FEATURES: u16 = 0x00;
const R_DRIVER_FEATURES: u16 = 0x04;
const R_QUEUE_PFN: u16 = 0x08;
const R_QUEUE_SIZE: u16 = 0x0C;
const R_QUEUE_SELECT: u16 = 0x0E;
const R_QUEUE_NOTIFY: u16 = 0x10;
const R_STATUS: u16 = 0x12;
const R_ISR: u16 = 0x13;
const R_CONFIG_MAC: u16 = 0x14; // device-specific config (no MSI-X): mac[6]

// Device status bits.
const S_ACKNOWLEDGE: u8 = 1;
const S_DRIVER: u8 = 2;
const S_DRIVER_OK: u8 = 4;

// The one feature we negotiate: a valid MAC in device config.
const VIRTIO_NET_F_MAC: u32 = 1 << 5;

// Virtqueue descriptor flags. (We use single, unchained descriptors — RX
// buffers are device-writable, the TX buffer device-readable — so only the
// WRITE flag is needed; there's no NEXT chaining.)
const VRING_DESC_F_WRITE: u16 = 2;
const VRING_ALIGN: u64 = 4096;

// Legacy virtio_net_hdr (no MRG_RXBUF): flags, gso_type, hdr_len, gso_size,
// csum_start, csum_offset = 10 bytes. Every RX/TX buffer is prefixed by it.
const NET_HDR_LEN: usize = 10;

const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;
const NUM_RX_BUFS: u16 = 16;
const BUF_SIZE: usize = 2048; // per buffer: hdr + frame
/// Largest L2 frame we hand back (Ethernet incl. header, no FCS).
pub const MAX_FRAME: usize = 1514;

/// One virtqueue's bookkeeping (HHDM virt base + ring offsets + indices).
struct Vq {
    virt: u64,
    avail_off: u64,
    used_off: u64,
    qsize: u16,
    avail_idx: u16,
    last_used: u16,
}

impl Vq {
    const EMPTY: Vq = Vq {
        virt: 0,
        avail_off: 0,
        used_off: 0,
        qsize: 0,
        avail_idx: 0,
        last_used: 0,
    };
}

struct Device {
    io_base: u16,
    net_irq: u8,
    rx: Vq,
    tx: Vq,
    rx_bufs_phys: u64,
    rx_bufs_virt: u64,
    tx_phys: u64,
    tx_virt: u64,
    mac: [u8; 6],
    present: bool,
}

static mut DEV: Device = Device {
    io_base: 0,
    net_irq: 0,
    rx: Vq::EMPTY,
    tx: Vq::EMPTY,
    rx_bufs_phys: 0,
    rx_bufs_virt: 0,
    tx_phys: 0,
    tx_virt: 0,
    mac: [0; 6],
    present: false,
};

// Posted by the IRQ handler, drained by the RX poll loop.
static IRQ_PENDING: AtomicBool = AtomicBool::new(false);

fn dev() -> &'static mut Device {
    let p = &raw mut DEV;
    unsafe { &mut *p }
}

/// The IRQ line virtio-net landed on (read from PCI config). Used by the IRQ
/// handler to EOI the correct PIC.
pub fn irq_line() -> u8 {
    dev().net_irq
}

// --- register + ring helpers -----------------------------------------------

fn status_write(base: u16, val: u8) {
    io::outb(base + R_STATUS, val);
}

/// Write descriptor `i` in queue at `vq_virt`.
unsafe fn set_desc(vq_virt: u64, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let d = (vq_virt + (i as u64) * 16) as *mut u8;
    (d as *mut u64).write(addr);
    (d.add(8) as *mut u32).write(len);
    (d.add(12) as *mut u16).write(flags);
    (d.add(14) as *mut u16).write(next);
}

/// Publish descriptor `desc_id` into a queue's available ring and bump its idx.
unsafe fn avail_push(vq: &mut Vq, desc_id: u16) {
    let avail = (vq.virt + vq.avail_off) as *mut u8;
    let ring = avail.add(4) as *mut u16; // ring[] after flags(2)+idx(2)
    ring.add((vq.avail_idx % vq.qsize) as usize).write(desc_id);
    core::sync::atomic::fence(Ordering::SeqCst);
    vq.avail_idx = vq.avail_idx.wrapping_add(1);
    (avail.add(2) as *mut u16).write(vq.avail_idx);
    core::sync::atomic::fence(Ordering::SeqCst);
}

/// Set up virtqueue `idx`: select it, read its size, allocate a contiguous DMA
/// region for the descriptor/avail/used rings, hand the device its PFN.
fn setup_queue(base: u16, idx: u16) -> Option<Vq> {
    io::outw(base + R_QUEUE_SELECT, idx);
    let qsize = io::inw(base + R_QUEUE_SIZE);
    if qsize == 0 {
        return None;
    }
    let q = qsize as u64;
    let desc_size = 16 * q;
    let avail_off = desc_size;
    let avail_size = 6 + 2 * q;
    let used_off = (avail_off + avail_size + VRING_ALIGN - 1) & !(VRING_ALIGN - 1);
    let used_size = 6 + 8 * q;
    let total = used_off + used_size;
    let pages = total.div_ceil(VRING_ALIGN) as usize;

    let queue_phys = frames::alloc_contiguous(pages)?;
    let queue_virt = frames::phys_to_virt(queue_phys) as u64;
    unsafe { core::ptr::write_bytes(queue_virt as *mut u8, 0, pages * 4096) };
    io::outl(base + R_QUEUE_PFN, (queue_phys / VRING_ALIGN) as u32);

    Some(Vq {
        virt: queue_virt,
        avail_off,
        used_off,
        qsize,
        avail_idx: 0,
        last_used: 0,
    })
}

// --- init ------------------------------------------------------------------

/// Probe + initialize virtio-net: reset, negotiate (MAC only), set up the RX +
/// TX virtqueues, pre-post RX buffers, read the MAC, and enable the IRQ.
/// Returns false if no device is present.
pub fn init() -> bool {
    let Some(pcidev) = pci::find(VIRTIO_VENDOR, VIRTIO_NET_DEVICE) else {
        serial::writeln("[net] virtio-net NOT found");
        return false;
    };
    pcidev.enable_io_and_bus_master();
    let base = pcidev.bar_io(0);
    let irq = pcidev.interrupt_line();

    // Reset, then ACKNOWLEDGE + DRIVER. Negotiate only VIRTIO_NET_F_MAC.
    status_write(base, 0);
    status_write(base, S_ACKNOWLEDGE);
    status_write(base, S_ACKNOWLEDGE | S_DRIVER);
    let features = io::inl(base + R_DEVICE_FEATURES);
    io::outl(base + R_DRIVER_FEATURES, features & VIRTIO_NET_F_MAC);

    // RX = queue 0, TX = queue 1.
    let (Some(rx), Some(tx)) = (setup_queue(base, RX_QUEUE), setup_queue(base, TX_QUEUE)) else {
        serial::writeln("[net] failed to set up virtqueues");
        return false;
    };

    // RX buffer pool: NUM_RX_BUFS × BUF_SIZE, contiguous.
    let rx_pool_pages = (NUM_RX_BUFS as usize * BUF_SIZE).div_ceil(4096);
    let Some(rx_bufs_phys) = frames::alloc_contiguous(rx_pool_pages) else {
        serial::writeln("[net] out of frames for RX pool");
        return false;
    };
    let rx_bufs_virt = frames::phys_to_virt(rx_bufs_phys) as u64;

    // TX scratch (one frame: hdr + outgoing frame).
    let Some(tx_phys) = frames::alloc_frame() else {
        serial::writeln("[net] out of frames for TX scratch");
        return false;
    };
    let tx_virt = frames::phys_to_virt(tx_phys) as u64;

    let d = dev();
    d.io_base = base;
    d.net_irq = irq;
    d.rx = rx;
    d.tx = tx;
    d.rx_bufs_phys = rx_bufs_phys;
    d.rx_bufs_virt = rx_bufs_virt;
    d.tx_phys = tx_phys;
    d.tx_virt = tx_virt;
    d.present = true;

    // Pre-post every RX buffer: one device-writable descriptor each, published
    // into the RX avail ring.
    for i in 0..NUM_RX_BUFS {
        let buf_phys = rx_bufs_phys + (i as u64) * BUF_SIZE as u64;
        unsafe {
            set_desc(d.rx.virt, i, buf_phys, BUF_SIZE as u32, VRING_DESC_F_WRITE, 0);
            avail_push(&mut d.rx, i);
        }
    }

    // Read the MAC from device config.
    for (i, b) in d.mac.iter_mut().enumerate() {
        *b = io::inb(base + R_CONFIG_MAC + i as u16);
    }

    // Route the IRQ (line read from PCI config), then let the device run and
    // notify it that RX buffers are available.
    interrupts::wire_virtio_net(irq);
    pic::unmask_irq(irq);
    status_write(base, S_ACKNOWLEDGE | S_DRIVER | S_DRIVER_OK);
    io::outw(base + R_QUEUE_NOTIFY, RX_QUEUE);

    serial::write_str("[net] virtio-net ready: io 0x");
    serial::write_hex_u64(base as u64);
    serial::write_str(", irq ");
    serial::write_u32_decimal(irq as u32);
    serial::write_str(", MAC ");
    print_mac(&d.mac);
    serial::writeln("");
    true
}

// --- post/drain RX + TX ----------------------------------------------------

/// IRQ post: ack the device ISR and flag a pending event. Native + interrupt-
/// safe — no Frame dispatch (that's the drain's job).
pub fn on_irq() {
    let d = dev();
    if d.present {
        let _ = io::inb(d.io_base + R_ISR); // read-to-ack
        IRQ_PENDING.store(true, Ordering::SeqCst);
    }
}

/// Transmit one raw Ethernet frame. Prefixes the zeroed virtio_net_hdr, copies
/// the frame, publishes a single device-readable descriptor on the TX queue,
/// and notifies. Fire-and-forget (we don't wait on the TX used ring here).
pub fn tx_frame(frame: &[u8]) {
    let d = dev();
    if !d.present {
        return;
    }
    let n = frame.len().min(MAX_FRAME);
    unsafe {
        core::ptr::write_bytes(d.tx_virt as *mut u8, 0, NET_HDR_LEN);
        core::ptr::copy_nonoverlapping(
            frame.as_ptr(),
            (d.tx_virt + NET_HDR_LEN as u64) as *mut u8,
            n,
        );
        // Single descriptor, device-readable (no WRITE flag).
        set_desc(d.tx.virt, 0, d.tx_phys, (NET_HDR_LEN + n) as u32, 0, 0);
        avail_push(&mut d.tx, 0);
    }
    io::outw(d.io_base + R_QUEUE_NOTIFY, TX_QUEUE);
}

/// Drain one received frame, if the RX used ring has advanced. Copies the L2
/// frame (virtio_net_hdr stripped) into `out`, recycles the buffer back onto
/// the RX avail ring, and returns the frame length. None if nothing new.
pub fn poll_rx(out: &mut [u8]) -> Option<usize> {
    let d = dev();
    if !d.present {
        return None;
    }
    let used = (d.rx.virt + d.rx.used_off) as *const u8;
    let used_idx = unsafe { (used.add(2) as *const u16).read() };
    if used_idx == d.rx.last_used {
        return None;
    }
    // used.ring[] is after flags(2)+idx(2); each elem is {u32 id, u32 len}.
    let slot = (d.rx.last_used % d.rx.qsize) as usize;
    let elem = unsafe { used.add(4 + slot * 8) };
    let desc_id = unsafe { (elem as *const u32).read() } as u16;
    let total_len = unsafe { (elem.add(4) as *const u32).read() } as usize;

    let frame_len = total_len.saturating_sub(NET_HDR_LEN);
    let n = frame_len.min(out.len());
    let buf_virt = d.rx_bufs_virt + (desc_id as u64) * BUF_SIZE as u64 + NET_HDR_LEN as u64;
    unsafe {
        core::ptr::copy_nonoverlapping(buf_virt as *const u8, out.as_mut_ptr(), n);
    }

    d.rx.last_used = d.rx.last_used.wrapping_add(1);

    // Recycle the buffer: re-publish its descriptor and notify.
    unsafe { avail_push(&mut d.rx, desc_id) };
    io::outw(d.io_base + R_QUEUE_NOTIFY, RX_QUEUE);

    Some(n)
}

/// Wait (interrupts enabled) for an RX frame, up to `timeout_ticks` PIT ticks.
/// Returns the frame length, or None on timeout. The post/drain wait loop:
/// the IRQ posts, this drains.
fn wait_rx(out: &mut [u8], timeout_ticks: u64) -> Option<usize> {
    let deadline = interrupts::ticks() + timeout_ticks;
    interrupts::enable();
    let result = loop {
        if let Some(n) = poll_rx(out) {
            break Some(n);
        }
        if interrupts::ticks() >= deadline {
            break None;
        }
        interrupts::wait_for_interrupt();
    };
    interrupts::disable();
    result
}

// --- Step 1 demo: ARP round-trip against the slirp gateway -----------------

const GUEST_IP: [u8; 4] = [10, 0, 2, 15]; // QEMU slirp default guest address
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2]; // QEMU slirp gateway (answers ARP)

/// Build a broadcast ARP request for `GATEWAY_IP` into `buf`, returning its
/// length (42 = 14 Ethernet + 28 ARP).
fn build_arp_request(buf: &mut [u8; 42], mac: &[u8; 6]) {
    // Ethernet header.
    buf[0..6].copy_from_slice(&[0xFF; 6]); // dst = broadcast
    buf[6..12].copy_from_slice(mac); // src = our MAC
    buf[12..14].copy_from_slice(&[0x08, 0x06]); // ethertype = ARP
                                                // ARP payload.
    buf[14..16].copy_from_slice(&[0x00, 0x01]); // htype = Ethernet
    buf[16..18].copy_from_slice(&[0x08, 0x00]); // ptype = IPv4
    buf[18] = 6; // hlen
    buf[19] = 4; // plen
    buf[20..22].copy_from_slice(&[0x00, 0x01]); // oper = request
    buf[22..28].copy_from_slice(mac); // sender hardware addr
    buf[28..32].copy_from_slice(&GUEST_IP); // sender protocol addr
    buf[32..38].copy_from_slice(&[0x00; 6]); // target hardware addr (unknown)
    buf[38..42].copy_from_slice(&GATEWAY_IP); // target protocol addr
}

/// B5 Step 1 demo: bring up virtio-net, then ARP the slirp gateway and wait for
/// the reply — proving init + TX + RX + the post/drain path end to end.
pub fn run_demo() {
    if !init() {
        return;
    }

    let mac = dev().mac;
    let mut req = [0u8; 42];
    build_arp_request(&mut req, &mac);
    serial::writeln("[net] ARP who-has 10.0.2.2 (gateway)...");
    tx_frame(&req);

    // Wait for an ARP reply (oper = 2) for our query. slirp answers ARP for the
    // gateway deterministically. ~300 ticks is a generous timeout.
    let mut buf = [0u8; MAX_FRAME];
    let mut got = false;
    for _ in 0..16 {
        let Some(n) = wait_rx(&mut buf, 300) else {
            break;
        };
        // Ethertype ARP (0x0806) + oper reply (0x0002)?
        if n >= 42 && buf[12] == 0x08 && buf[13] == 0x06 && buf[20] == 0x00 && buf[21] == 0x02 {
            serial::write_str("[net] ARP reply: gateway MAC ");
            let mut gw = [0u8; 6];
            gw.copy_from_slice(&buf[22..28]);
            print_mac(&gw);
            serial::writeln("");
            got = true;
            break;
        }
        // Some other frame — keep waiting.
    }
    if got {
        serial::writeln("[net] rx/tx round-trip: ok");
    } else {
        serial::writeln("[net] no ARP reply (timeout)");
    }
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
