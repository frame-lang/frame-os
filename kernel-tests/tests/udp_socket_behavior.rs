// kernel-tests/tests/udp_socket_behavior.rs
//
// Level 3 (behavioral) tests for the UdpSocket bind lifecycle, on the host (B5
// Step 3b). The bind state IS the invariant: `recv()` is only handled in
// `$Bound`, so a datagram for an unbound socket is dropped structurally. Pure
// system (no native actions).

use frame_os_kernel_tests::UdpSocket;

#[test]
fn fresh_socket_is_unbound() {
    let mut s = UdpSocket::__create();
    assert!(!s.is_bound());
    assert_eq!(s.received(), 0);
}

#[test]
fn bind_sets_port_and_binds() {
    let mut s = UdpSocket::__create();
    s.bind(68);
    assert!(s.is_bound());
    assert_eq!(s.port(), 68);
}

#[test]
fn recv_on_bound_socket_counts() {
    let mut s = UdpSocket::__create();
    s.bind(68);
    s.recv();
    s.recv();
    assert_eq!(s.received(), 2);
}

#[test]
fn recv_on_unbound_socket_is_dropped() {
    // $Unbound doesn't handle recv() — a stray datagram is ignored, not counted.
    let mut s = UdpSocket::__create();
    s.recv();
    assert_eq!(s.received(), 0, "an unbound socket receives nothing");
    assert!(!s.is_bound());
}

#[test]
fn close_unbinds() {
    let mut s = UdpSocket::__create();
    s.bind(68);
    s.recv();
    s.close();
    assert!(!s.is_bound());
    // And recv() is gated out again once unbound.
    s.recv();
    assert_eq!(s.received(), 1, "no further datagrams counted once closed");
}
