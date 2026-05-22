// kernel-tests/tests/usb_transfer_behavior.rs
//
// Level 3 (behavioral) tests for the UsbTransfer FSM, on the host (B6 Step 4).
// The FSM models one transfer's lifecycle:
//
//   $Idle ─ start ─► $InFlight ─┬─ complete ─► $Complete
//                               └─ fail ─────► $Failed
//
// `$InFlight`'s enter handler queues the transfer (`crate::xhci::queue_interrupt_in`,
// non-blocking) and `$Complete`'s consumes the result (`on_report`). Here `xhci`
// is the capturing host double (see lib.rs), which counts the calls. These tests
// pin the transitions; the real interrupt-IN transfer (Configure Endpoint + EP1
// ring + a keypress-driven Transfer Event) is validated by `usb_transfer_b6`.

use frame_os_kernel_tests::{xhci, UsbTransfer};

#[test]
fn starts_idle() {
    xhci::reset();
    let mut t = UsbTransfer::__create();
    assert_eq!(t.state(), "Idle");
    assert!(!t.is_complete());
    assert!(!t.is_failed());
}

#[test]
fn start_queues_the_transfer() {
    xhci::reset();
    let mut t = UsbTransfer::__create();
    t.start();
    assert_eq!(t.state(), "InFlight");
    assert_eq!(xhci::queued_transfers(), 1); // $InFlight.$> queued it
}

#[test]
fn complete_reads_the_report() {
    xhci::reset();
    let mut t = UsbTransfer::__create();
    t.start();
    t.complete();
    assert_eq!(t.state(), "Complete");
    assert!(t.is_complete());
    assert_eq!(xhci::reports_read(), 1); // $Complete.$> read the report
}

#[test]
fn fail_in_flight_goes_failed() {
    xhci::reset();
    let mut t = UsbTransfer::__create();
    t.start();
    t.fail();
    assert_eq!(t.state(), "Failed");
    assert!(t.is_failed());
    assert_eq!(xhci::reports_read(), 0); // no report on failure
}
