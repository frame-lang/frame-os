// kernel-tests/tests/syscall_dispatcher_behavior.rs
//
// Level 3 (behavioral) tests for the SyscallDispatcher HSM, on the host with
// the `usermode` test-double (is_known_syscall: num < 2; perform_syscall:
// echoes a0).
//
// These exercise the two paths the `=> $^` design is about:
//   - a known syscall → $Validating → $Executing → result set, not error;
//   - an unknown syscall → self.reject() forwarded `=> $^` to $Active →
//     error result, is_error true.

use frame_os_kernel_tests::SyscallDispatcher;

#[test]
fn fresh_dispatcher_not_error() {
    let mut d = SyscallDispatcher::__create();
    assert!(!d.is_error());
}

#[test]
fn known_syscall_executes_and_returns_value() {
    let mut d = SyscallDispatcher::__create();
    d.request(0, 99, 0); // known; perform_syscall echoes a0 = 99
    assert!(!d.is_error());
    assert_eq!(d.result(), 99);
}

#[test]
fn unknown_syscall_is_rejected_via_parent() {
    let mut d = SyscallDispatcher::__create();
    d.request(7, 0, 0); // unknown → reject(38) forwarded to $Active
    assert!(
        d.is_error(),
        "an unknown syscall must be flagged as an error"
    );
    assert_eq!(d.result(), 38, "ENOSYS result set by the parent's reject()");
}

#[test]
fn dispatcher_recovers_after_a_reject() {
    let mut d = SyscallDispatcher::__create();
    // First a rejected (unknown) syscall...
    d.request(9, 0, 0);
    assert!(d.is_error());
    // ...then a valid one: back in $Validating, it executes cleanly.
    d.request(1, 7, 0);
    assert!(!d.is_error(), "a valid syscall after a reject must succeed");
    assert_eq!(d.result(), 7);
}

#[test]
fn each_request_classified_independently() {
    let mut d = SyscallDispatcher::__create();
    d.request(0, 1, 0);
    assert!(!d.is_error());
    assert_eq!(d.result(), 1);
    d.request(0, 2, 0);
    assert!(!d.is_error());
    assert_eq!(d.result(), 2);
}
