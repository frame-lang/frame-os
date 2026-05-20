// kernel-tests/tests/serial_driver_behavior.rs
//
// Level 3 (behavioral) tests for the SerialDriver Frame system, run on
// the host with the capturing `serial` module.
//
// The system models one invariant: you must `init()` (program the UART)
// before you write, or output goes out a misconfigured port. The machine
// makes that structural — writes are only handled in $Ready, and the only
// way to $Ready is through init(). These tests assert exactly that gate,
// plus the happy-path writes.
//
// Capture isolation: libtest runs each test on its own thread and the
// capture buffer is thread-local, so tests don't interfere; each test
// that inspects output still clears first for clarity.

use frame_os_kernel_tests::{serial, SerialDriver};

#[test]
fn fresh_driver_is_uninitialized_not_ready() {
    let mut d = SerialDriver::__create();
    assert!(
        !d.is_ready(),
        "a freshly constructed SerialDriver should be in $Uninitialized (is_ready == false)"
    );
}

#[test]
fn write_before_init_is_dropped() {
    serial::clear();
    let mut d = SerialDriver::__create();

    // Both write events are unhandled in $Uninitialized (explicit-only
    // forwarding drops them), so nothing should reach the UART/capture.
    d.write_line("should not appear".to_string());
    d.write_byte(b'X');

    assert_eq!(
        serial::captured(),
        "",
        "writes in $Uninitialized must be gated (silently dropped), not emitted"
    );
    assert!(
        !d.is_ready(),
        "writes must not have side-effected the state"
    );
}

#[test]
fn init_transitions_to_ready() {
    let mut d = SerialDriver::__create();
    d.init();
    assert!(
        d.is_ready(),
        "after init(), SerialDriver should be in $Ready"
    );
}

#[test]
fn write_line_after_init_emits_text_and_newline() {
    serial::clear();
    let mut d = SerialDriver::__create();
    d.init();
    d.write_line("hello".to_string());
    assert_eq!(serial::captured(), "hello\n");
}

#[test]
fn write_byte_after_init_emits_single_byte() {
    serial::clear();
    let mut d = SerialDriver::__create();
    d.init();
    d.write_byte(b'Z');
    assert_eq!(serial::captured(), "Z");
}

#[test]
fn multiple_writes_after_init_accumulate_in_order() {
    serial::clear();
    let mut d = SerialDriver::__create();
    d.init();
    d.write_line("one".to_string());
    d.write_byte(b'>');
    d.write_line("two".to_string());
    assert_eq!(serial::captured(), "one\n>two\n");
}

#[test]
fn reinit_while_ready_is_ignored_and_stays_ready() {
    let mut d = SerialDriver::__create();
    d.init();
    assert!(d.is_ready());
    // init() is unhandled in $Ready (omitted handler) → silently ignored,
    // no panic, and we remain $Ready.
    d.init();
    assert!(
        d.is_ready(),
        "re-init while $Ready should be a no-op, staying $Ready"
    );
}
