// kernel-tests/tests/tcp_connection_behavior.rs
//
// Level 3 (behavioral) tests for the TcpConnection RFC-793 state machine, on
// the host (B5 Step 4a). Segments are plain structs; the `tcp` double records
// which native actions fired (send_syn_ack, send_ack, ...). These exercise the
// whole graph — both opens, data, both closes, TIME_WAIT, the simultaneous-
// open/close edges, and the RST funnel — against RFC-793.

use frame_os_kernel_tests::{tcp, TcpConnection};

// --- segment builders ------------------------------------------------------

fn seg(syn: bool, ack: bool, fin: bool, payload_len: usize) -> tcp::TcpSegment {
    tcp::TcpSegment {
        syn,
        ack,
        fin,
        rst: false,
        payload_len,
    }
}
fn syn() -> tcp::TcpSegment {
    seg(true, false, false, 0)
}
fn syn_ack() -> tcp::TcpSegment {
    seg(true, true, false, 0)
}
fn ack() -> tcp::TcpSegment {
    seg(false, true, false, 0)
}
fn fin() -> tcp::TcpSegment {
    seg(false, false, true, 0)
}
fn fin_ack() -> tcp::TcpSegment {
    seg(false, true, true, 0)
}
fn data(n: usize) -> tcp::TcpSegment {
    seg(false, true, false, n)
}

/// A connection driven (passively) to $Established, with the action log cleared.
fn established() -> TcpConnection {
    let mut c = TcpConnection::__create();
    c.open_passive();
    c.segment(syn());
    c.segment(ack());
    assert_eq!(c.state(), "Established");
    tcp::reset();
    c
}

// --- open ------------------------------------------------------------------

#[test]
fn fresh_is_closed() {
    let mut c = TcpConnection::__create();
    assert_eq!(c.state(), "Closed");
}

#[test]
fn passive_open_listens() {
    let mut c = TcpConnection::__create();
    c.open_passive();
    assert_eq!(c.state(), "Listen");
}

#[test]
fn active_open_sends_syn_and_goes_syn_sent() {
    tcp::reset();
    let mut c = TcpConnection::__create();
    c.open_active();
    assert_eq!(c.state(), "SynSent");
    assert!(tcp::fired("send_syn"));
    assert!(tcp::fired("arm_retransmit"), "SynSent arms the retransmit timer");
}

#[test]
fn passive_handshake_to_established() {
    tcp::reset();
    let mut c = TcpConnection::__create();
    c.open_passive();
    c.segment(syn());
    assert_eq!(c.state(), "SynReceived");
    assert!(tcp::fired("send_syn_ack"));
    c.segment(ack());
    assert_eq!(c.state(), "Established");
}

#[test]
fn active_handshake_to_established() {
    let mut c = TcpConnection::__create();
    c.open_active(); // -> SynSent
    tcp::reset();
    c.segment(syn_ack());
    assert_eq!(c.state(), "Established");
    assert!(tcp::fired("send_ack"));
}

#[test]
fn simultaneous_open_goes_syn_received() {
    let mut c = TcpConnection::__create();
    c.open_active(); // -> SynSent
    tcp::reset();
    c.segment(syn()); // peer also actively opened (SYN, no ACK)
    assert_eq!(c.state(), "SynReceived");
    assert!(tcp::fired("send_syn_ack"));
}

// --- data ------------------------------------------------------------------

#[test]
fn data_in_established_is_echoed() {
    let mut c = established();
    c.segment(data(10));
    assert_eq!(c.state(), "Established");
    // The echo "app" sends the data back (the data segment piggybacks the ACK,
    // so no separate send_ack fires).
    assert!(tcp::fired("deliver_data"));
    assert!(!tcp::fired("send_ack"), "the echo data carries the ACK");
}

#[test]
fn pure_ack_in_established_is_silent() {
    let mut c = established();
    c.segment(ack()); // no payload, no FIN
    assert_eq!(c.state(), "Established");
    assert!(!tcp::fired("deliver_data"), "a pure ACK delivers nothing");
    assert!(!tcp::fired("send_ack"), "and triggers no reply ACK");
}

// --- passive close (peer closes first) -------------------------------------

#[test]
fn peer_fin_goes_close_wait_then_last_ack_then_closed() {
    let mut c = established();
    c.segment(fin());
    assert_eq!(c.state(), "CloseWait");
    assert!(tcp::fired("send_ack"));
    tcp::reset();
    c.close(); // app closes
    assert_eq!(c.state(), "LastAck");
    assert!(tcp::fired("send_fin"));
    c.segment(ack()); // peer ACKs our FIN
    assert_eq!(c.state(), "Closed");
}

// --- active close (we close first) -----------------------------------------

#[test]
fn local_close_goes_fin_wait1_fin_wait2_time_wait_closed() {
    let mut c = established();
    c.close();
    assert_eq!(c.state(), "FinWait1");
    assert!(tcp::fired("send_fin"));
    c.segment(ack()); // peer ACKs our FIN
    assert_eq!(c.state(), "FinWait2");
    c.segment(fin()); // peer's FIN
    assert_eq!(c.state(), "TimeWait");
    assert!(tcp::fired("arm_timewait"), "TimeWait arms the 2*MSL timer");
    c.timeout(); // 2*MSL elapsed
    assert_eq!(c.state(), "Closed");
}

#[test]
fn fin_then_finack_collapses_fin_wait1_to_time_wait() {
    let mut c = established();
    c.close(); // -> FinWait1
    c.segment(fin_ack()); // peer ACKs our FIN *and* sends its own FIN at once
    assert_eq!(c.state(), "TimeWait");
}

#[test]
fn simultaneous_close_goes_closing_then_time_wait() {
    let mut c = established();
    c.close(); // -> FinWait1
    c.segment(fin()); // peer's FIN before ACKing ours (simultaneous close)
    assert_eq!(c.state(), "Closing");
    c.segment(ack()); // peer ACKs our FIN
    assert_eq!(c.state(), "TimeWait");
}

// --- RST funnel (=> $^) ----------------------------------------------------

#[test]
fn rst_from_established_resets_to_closed() {
    let mut c = established();
    c.rst();
    assert_eq!(c.state(), "Closed");
    assert!(tcp::fired("on_reset"), "the $Open parent's rst handler ran");
}

#[test]
fn rst_from_syn_received_resets_to_closed() {
    let mut c = TcpConnection::__create();
    c.open_passive();
    c.segment(syn()); // -> SynReceived
    tcp::reset();
    c.rst(); // unhandled in $SynReceived -> => $^ -> $Open.rst -> $Closed
    assert_eq!(c.state(), "Closed");
    assert!(tcp::fired("on_reset"));
}

// --- timers ----------------------------------------------------------------

#[test]
fn syn_sent_timeout_retransmits_the_syn() {
    let mut c = TcpConnection::__create();
    c.open_active(); // -> SynSent (send_syn)
    tcp::reset();
    c.timeout(); // retransmit timer fired
    assert_eq!(c.state(), "SynSent", "stays in SynSent on a retransmit");
    assert!(tcp::fired("send_syn"));
}

#[test]
fn syn_received_timeout_retransmits_the_syn_ack() {
    let mut c = TcpConnection::__create();
    c.open_passive();
    c.segment(syn()); // -> SynReceived (send_syn_ack)
    tcp::reset();
    c.timeout(); // retransmit timer fired before the peer's ACK
    assert_eq!(c.state(), "SynReceived", "stays in SynReceived on a retransmit");
    assert!(tcp::fired("send_syn_ack"));
}
