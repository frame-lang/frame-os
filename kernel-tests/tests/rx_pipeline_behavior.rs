// kernel-tests/tests/rx_pipeline_behavior.rs
//
// Level 3 (behavioral) tests for the RxPipeline classify→dispatch FSM, on the
// host (B5 Step 3). A parsed `RxDescriptor` is delivered; the pipeline routes
// on its fields (ethertype, then IPv4 protocol) and fires the matching native
// leaf handler. The `net` double records which leaf fired (see lib.rs).
//
// The descriptor is threaded down the graph as an enter parameter; here we
// assert the *dispatch* it produces and that the machine returns to $Idle ready
// for the next frame.

use frame_os_kernel_tests::{net, RxPipeline};

fn desc(ethertype: u16, ip_proto: u8) -> net::RxDescriptor {
    net::RxDescriptor {
        ethertype,
        ip_proto,
    }
}

#[test]
fn arp_frame_dispatches_to_arp() {
    net::reset();
    let mut p = RxPipeline::__create();
    p.deliver(desc(0x0806, 0));
    assert_eq!(net::last_dispatch(), "arp");
}

#[test]
fn ipv4_icmp_dispatches_to_icmp() {
    net::reset();
    let mut p = RxPipeline::__create();
    p.deliver(desc(0x0800, 1)); // IPv4, proto ICMP
    assert_eq!(net::last_dispatch(), "icmp");
}

#[test]
fn ipv4_udp_dispatches_to_udp() {
    net::reset();
    let mut p = RxPipeline::__create();
    p.deliver(desc(0x0800, 17)); // IPv4, proto UDP
    assert_eq!(net::last_dispatch(), "udp");
}

#[test]
fn unknown_ethertype_dispatches_to_nothing() {
    net::reset();
    let mut p = RxPipeline::__create();
    p.deliver(desc(0x86dd, 0)); // IPv6 — not classified at B5 Step 3
    assert_eq!(net::last_dispatch(), "", "no leaf fires for an unknown ethertype");
}

#[test]
fn ipv4_tcp_dispatches_to_tcp() {
    net::reset();
    let mut p = RxPipeline::__create();
    p.deliver(desc(0x0800, 6)); // IPv4, proto TCP
    assert_eq!(net::last_dispatch(), "tcp");
}

#[test]
fn ipv4_unknown_proto_dispatches_to_nothing() {
    net::reset();
    let mut p = RxPipeline::__create();
    p.deliver(desc(0x0800, 99)); // IPv4, unassigned protocol
    assert_eq!(net::last_dispatch(), "");
}

#[test]
fn pipeline_returns_to_idle_and_reclassifies() {
    // Each leaf returns to $Idle, so a second frame classifies independently.
    net::reset();
    let mut p = RxPipeline::__create();
    p.deliver(desc(0x0806, 0)); // -> arp
    assert_eq!(net::last_dispatch(), "arp");
    p.deliver(desc(0x0800, 17)); // -> udp (proves it came back to $Idle)
    assert_eq!(net::last_dispatch(), "udp");
}
