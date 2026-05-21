// kernel-tests/tests/block_request_behavior.rs
//
// Level 3 (behavioral) tests for the BlockRequest lifecycle, on the host.
// BlockRequest is pure (no native actions), so it tests directly. It models
// where one block-I/O request is in its life: submitted ($Queued → $InFlight),
// then driven to $Complete or $Error by the *drained* completion.

use frame_os_kernel_tests::BlockRequest;

#[test]
fn fresh_request_is_not_inflight_complete_or_error() {
    let mut r = BlockRequest::__create();
    assert!(!r.is_inflight());
    assert!(!r.is_complete());
    assert!(!r.is_error());
}

#[test]
fn submit_moves_to_inflight() {
    let mut r = BlockRequest::__create();
    r.submit();
    assert!(r.is_inflight());
    assert!(!r.is_complete());
}

#[test]
fn complete_after_submit_succeeds() {
    let mut r = BlockRequest::__create();
    r.submit();
    r.complete();
    assert!(r.is_complete());
    assert!(!r.is_error());
    assert!(!r.is_inflight());
}

#[test]
fn fail_after_submit_errors() {
    let mut r = BlockRequest::__create();
    r.submit();
    r.fail();
    assert!(r.is_error());
    assert!(!r.is_complete());
    assert!(!r.is_inflight());
}

#[test]
fn complete_before_submit_is_ignored() {
    // Per explicit-only-forwarding, $Queued doesn't handle complete/fail — a
    // completion can't arrive before the request is in flight.
    let mut r = BlockRequest::__create();
    r.complete();
    assert!(
        !r.is_complete(),
        "a not-yet-submitted request can't complete"
    );
}

#[test]
fn complete_is_terminal() {
    let mut r = BlockRequest::__create();
    r.submit();
    r.complete();
    // Further events are ignored in the terminal state.
    r.fail();
    r.submit();
    assert!(r.is_complete());
    assert!(!r.is_error());
}
