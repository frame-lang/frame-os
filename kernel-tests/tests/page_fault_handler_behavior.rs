// kernel-tests/tests/page_fault_handler_behavior.rs
//
// Level 3 (behavioral) tests for the PageFaultHandler HSM, run on the host
// with the `vm` test-double (controllable is_lazy_region / lazy_map).
//
// These exercise the classification the kernel's #PF handler relies on:
//   - a fault outside any lazy region → $Fatal (kernel) / $Killing (user),
//   - a fault in a lazy region that maps → recovered (not fatal),
//   - a fault in a lazy region that can't map (OOM) → unrecoverable,
//   - the user/kernel disposition split (error-code U/S bit) routed through
//     $FaultActive's `=> $^` funnel, and recover() readying for the next fault.
//
// Error code: bit 2 (value 4) is the U/S bit — set ⇒ the fault was in ring 3.

use frame_os_kernel_tests::{vm, PageFaultHandler};

const US_USER: u64 = 4; // error-code U/S bit set ⇒ ring-3 fault

#[test]
fn fresh_handler_is_not_fatal() {
    let mut h = PageFaultHandler::__create();
    assert!(!h.is_fatal());
}

#[test]
fn fault_outside_lazy_region_is_fatal() {
    vm::set_lazy(false);
    let mut h = PageFaultHandler::__create();
    h.fault(0xDEAD_0000, 0);
    assert!(h.is_fatal(), "an unmapped, non-lazy fault must be fatal");
    assert_eq!(h.fault_addr(), 0xDEAD_0000);
}

#[test]
fn lazy_fault_that_maps_recovers() {
    vm::set_lazy(true);
    vm::set_map_ok(true);
    let mut h = PageFaultHandler::__create();
    h.fault(0x5000_0000, 0);
    // $Classifying → $LazyFault → map ok → back to $Classifying. Not fatal.
    assert!(!h.is_fatal(), "a satisfied demand fault must not be fatal");
}

#[test]
fn lazy_fault_oom_is_fatal() {
    vm::set_lazy(true);
    vm::set_map_ok(false); // simulate out-of-frames
    let mut h = PageFaultHandler::__create();
    h.fault(0x5000_0000, 0);
    // $LazyFault's map fails → $Fatal.
    assert!(
        h.is_fatal(),
        "a demand fault that can't be mapped must be fatal"
    );
}

#[test]
fn handler_classifies_each_fault_independently() {
    // A recovered lazy fault leaves the handler ready to classify the next.
    vm::set_lazy(true);
    vm::set_map_ok(true);
    let mut h = PageFaultHandler::__create();
    h.fault(0x5000_0000, 0);
    assert!(!h.is_fatal());
    // Now a non-lazy fault on the same handler → fatal.
    vm::set_lazy(false);
    h.fault(0x9000_0000, 0);
    assert!(h.is_fatal());
    assert_eq!(h.fault_addr(), 0x9000_0000);
}

#[test]
fn user_fault_outside_lazy_region_kills_not_halts() {
    vm::set_lazy(false);
    let mut h = PageFaultHandler::__create();
    h.fault(0xFFFF_FFFF_8000_0000, US_USER);
    // Ring-3 fault → $FaultActive.unrecoverable (via `=> $^`) → $Killing.
    assert!(h.is_killing(), "a ring-3 fault must kill the process");
    assert!(!h.is_fatal(), "a user fault must NOT halt the kernel");
}

#[test]
fn user_lazy_oom_kills_not_halts() {
    vm::set_lazy(true);
    vm::set_map_ok(false); // out of frames during a ring-3 demand fault
    let mut h = PageFaultHandler::__create();
    h.fault(0x5000_0000, US_USER);
    assert!(h.is_killing(), "OOM during a user fault kills the process");
    assert!(!h.is_fatal());
}

#[test]
fn kernel_fault_halts_not_kills() {
    vm::set_lazy(false);
    let mut h = PageFaultHandler::__create();
    h.fault(0x9000_0000, 0); // U/S bit clear ⇒ supervisor (kernel) fault
    assert!(h.is_fatal(), "a kernel fault is a kernel bug → halt");
    assert!(!h.is_killing());
}

#[test]
fn recover_after_kill_readies_for_next_fault() {
    // Kill a user process...
    vm::set_lazy(false);
    let mut h = PageFaultHandler::__create();
    h.fault(0xFFFF_FFFF_8000_0000, US_USER);
    assert!(h.is_killing());
    // ...the kernel survives and resets the handler...
    h.recover();
    assert!(!h.is_killing(), "recover() leaves $Killing");
    // ...and the next fault classifies cleanly (here: a recovered demand fault).
    vm::set_lazy(true);
    vm::set_map_ok(true);
    h.fault(0x5000_0000, US_USER);
    assert!(!h.is_killing(), "a satisfied demand fault is not a kill");
    assert!(!h.is_fatal());
}
