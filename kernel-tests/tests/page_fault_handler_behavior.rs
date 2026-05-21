// kernel-tests/tests/page_fault_handler_behavior.rs
//
// Level 3 (behavioral) tests for the PageFaultHandler HSM, run on the host
// with the `vm` test-double (controllable is_lazy_region / lazy_map).
//
// These exercise the classification the kernel's #PF handler relies on:
//   - a fault outside any lazy region → $Fatal,
//   - a fault in a lazy region that maps → recovered (not fatal),
//   - a fault in a lazy region that can't map (OOM) → $LazyFault → $Fatal.

use frame_os_kernel_tests::{vm, PageFaultHandler};

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
