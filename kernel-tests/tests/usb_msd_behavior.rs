// kernel-tests/tests/usb_msd_behavior.rs
//
// Level 3 (behavioral) tests for the UsbMsd FSM, on the host (R3b). The FSM
// models one USB Mass-Storage Bulk-Only Transport transaction's phases:
//
//   $Idle ─ begin(cmd) ─► $CommandPhase ─ cbw_sent ─► $DataPhase ─ data_received ─►
//   $StatusPhase ─ status_received ─► $Complete
//        │ (any active phase, via the $Active parent)
//        └─ fail ─► $Failed
//
// Each phase's enter handler issues the next bulk transfer (`crate::xhci::msd_*`,
// non-blocking); here `xhci` is the capturing host double (see lib.rs), which
// counts the calls and records the SCSI opcode. These tests pin the phase
// transitions; the real CBW/data/CSW bulk exchange + SCSI parsing is validated by
// `usb_msd_r3b`.

use frame_os_kernel_tests::{xhci, UsbMsd};

const SCSI_INQUIRY: u8 = 0x12;

#[test]
fn starts_idle() {
    xhci::reset();
    let mut m = UsbMsd::__create();
    assert_eq!(m.state(), "Idle");
    assert!(!m.is_complete());
    assert!(!m.is_failed());
}

#[test]
fn begin_sends_the_cbw() {
    xhci::reset();
    let mut m = UsbMsd::__create();
    m.begin(SCSI_INQUIRY);
    assert_eq!(m.state(), "CommandPhase");
    assert_eq!(xhci::cbws_sent(), 1); // $CommandPhase.$> sent the CBW
    assert_eq!(xhci::cbw_cmd(), SCSI_INQUIRY); // for the requested SCSI command
}

#[test]
fn full_phase_sequence_to_complete() {
    xhci::reset();
    let mut m = UsbMsd::__create();
    m.begin(SCSI_INQUIRY);
    m.cbw_sent(); // → $DataPhase ($> reads the data)
    assert_eq!(m.state(), "DataPhase");
    assert_eq!(xhci::data_recvs(), 1);
    m.data_received(); // → $StatusPhase ($> reads the CSW)
    assert_eq!(m.state(), "StatusPhase");
    assert_eq!(xhci::csw_recvs(), 1);
    m.status_received(); // → $Complete
    assert_eq!(m.state(), "Complete");
    assert!(m.is_complete());
}

#[test]
fn fail_in_data_phase_goes_failed() {
    xhci::reset();
    let mut m = UsbMsd::__create();
    m.begin(SCSI_INQUIRY);
    m.cbw_sent();
    m.fail(); // funnels through $Active => $^
    assert_eq!(m.state(), "Failed");
    assert!(m.is_failed());
    assert_eq!(xhci::csw_recvs(), 0); // never reached the status phase
}
