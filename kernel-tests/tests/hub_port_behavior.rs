// kernel-tests/tests/hub_port_behavior.rs
//
// Level 3 (behavioral) tests for the HubPort connect/reset/enable FSM, on the
// host (B6 Step 2). The FSM owns one xHCI root-hub port's lifecycle:
//
//   $Disconnected ─ connect(port) ─► $Connected ─ reset ─► $Resetting ─┬─ reset_complete ─► $Enabled
//        ▲                                                             └─ timeout ─► $Connected
//        └──────── disconnect (funneled through $Attached via => $^) ──────────────┘
//
// `$Resetting`'s enter handler calls `crate::xhci::begin_port_reset(port)` and
// `$Enabled`'s calls `on_port_enabled(port)` — here `xhci` is the capturing host
// double (see lib.rs), which records the port + call counts. These tests pin the
// transitions, the disconnect parent-funnel, and that the port threads through.
// The real PORTSC pokes are validated end-to-end by `usb_port_reset_b6`.

use frame_os_kernel_tests::{xhci, HubPort};

#[test]
fn starts_disconnected() {
    xhci::reset();
    let mut p = HubPort::__create();
    assert_eq!(p.state(), "Disconnected");
    assert!(!p.is_enabled());
}

#[test]
fn connect_enters_connected() {
    xhci::reset();
    let mut p = HubPort::__create();
    p.connect(5);
    assert_eq!(p.state(), "Connected");
}

#[test]
fn reset_arms_port_reset_on_the_connected_port() {
    xhci::reset();
    let mut p = HubPort::__create();
    p.connect(5);
    p.reset();
    assert_eq!(p.state(), "Resetting");
    assert_eq!(xhci::resets(), 1);
    assert_eq!(xhci::reset_port(), 5); // the port threaded through the domain
}

#[test]
fn reset_complete_enables_the_port() {
    xhci::reset();
    let mut p = HubPort::__create();
    p.connect(5);
    p.reset();
    p.reset_complete();
    assert_eq!(p.state(), "Enabled");
    assert!(p.is_enabled());
    assert_eq!(xhci::enabled_port(), 5);
}

#[test]
fn reset_timeout_falls_back_to_connected() {
    xhci::reset();
    let mut p = HubPort::__create();
    p.connect(5);
    p.reset();
    p.timeout(); // the controller never reported enabled
    assert_eq!(p.state(), "Connected");
    assert!(!p.is_enabled());
}

#[test]
fn disconnect_from_enabled_funnels_to_disconnected() {
    xhci::reset();
    let mut p = HubPort::__create();
    p.connect(5);
    p.reset();
    p.reset_complete();
    p.disconnect(); // handled by the $Attached parent (=> $^)
    assert_eq!(p.state(), "Disconnected");
    assert!(!p.is_enabled());
}

#[test]
fn disconnect_from_connected_funnels_to_disconnected() {
    xhci::reset();
    let mut p = HubPort::__create();
    p.connect(5);
    p.disconnect();
    assert_eq!(p.state(), "Disconnected");
}

#[test]
fn disconnect_from_resetting_funnels_to_disconnected() {
    xhci::reset();
    let mut p = HubPort::__create();
    p.connect(5);
    p.reset();
    p.disconnect();
    assert_eq!(p.state(), "Disconnected");
}
