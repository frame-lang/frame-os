// kernel-tests/tests/usb_enumeration_behavior.rs
//
// Level 3 (behavioral) tests for the UsbEnumeration FSM, on the host (B6 Step 3).
// The FSM tracks the enumeration stage; each state's enter handler issues the
// next xHCI command (non-blocking), and the native driver dispatches the
// milestone event on the command's completion:
//
//   $Powered ─ slot_enabled(slot) ─► $SlotEnabled ─ addressed ─► $AddressAssigned
//      └──────────────── fail (via the $Enumerating parent => $^) ──► $Failed
//
// `$Powered`'s enter handler runs at construction, so creating the FSM issues
// Enable Slot. Here `xhci` is the capturing host double (see lib.rs), which
// records the actions + the slot threaded through the domain. (The real TRBs +
// contexts are validated end-to-end by the `usb_enumerates_b6` QEMU smoke test.)

use frame_os_kernel_tests::{xhci, UsbEnumeration};

#[test]
fn construction_issues_enable_slot_in_powered() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    assert_eq!(e.state(), "Powered");
    assert_eq!(xhci::enable_slots(), 1); // $Powered.$> issued Enable Slot
    assert!(!e.is_addressed());
}

#[test]
fn slot_enabled_addresses_the_device_on_that_slot() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    e.slot_enabled(1);
    assert_eq!(e.state(), "SlotEnabled");
    assert_eq!(xhci::addr_slot(), 1); // $SlotEnabled.$> issued Address Device on slot 1
}

#[test]
fn addressed_reads_the_device_descriptor() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    e.slot_enabled(1);
    e.addressed();
    assert_eq!(e.state(), "AddressAssigned");
    assert!(e.is_addressed());
    assert_eq!(xhci::get_desc_slot(), 1); // $AddressAssigned.$> issued GET_DESCRIPTOR
}

#[test]
fn device_described_reads_descriptor_and_sets_configuration() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    e.slot_enabled(1);
    e.addressed();
    e.device_described();
    assert_eq!(e.state(), "DeviceDescribed");
    assert_eq!(xhci::desc_reads(), 1); // parsed the descriptor
    assert_eq!(xhci::set_config_slot(), 1); // issued SET_CONFIGURATION
}

#[test]
fn configured_reaches_configured() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    e.slot_enabled(1);
    e.addressed();
    e.device_described();
    e.configured();
    assert_eq!(e.state(), "Configured");
    assert!(e.is_configured());
    assert_eq!(xhci::configured_slot(), 1);
}

#[test]
fn fail_from_powered_goes_failed() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    e.fail();
    assert_eq!(e.state(), "Failed");
    assert!(e.is_failed());
}

#[test]
fn fail_from_slot_enabled_funnels_to_failed() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    e.slot_enabled(1);
    e.fail(); // handled by the $Enumerating parent (=> $^)
    assert_eq!(e.state(), "Failed");
    assert!(e.is_failed());
}

#[test]
fn fail_from_address_assigned_funnels_to_failed() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    e.slot_enabled(1);
    e.addressed();
    e.fail();
    assert_eq!(e.state(), "Failed");
}

#[test]
fn fail_from_device_described_funnels_to_failed() {
    xhci::reset();
    let mut e = UsbEnumeration::__create();
    e.slot_enabled(1);
    e.addressed();
    e.device_described();
    e.fail();
    assert_eq!(e.state(), "Failed");
    assert!(e.is_failed());
}
