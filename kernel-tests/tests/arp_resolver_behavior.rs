// kernel-tests/tests/arp_resolver_behavior.rs
//
// Level 3 (behavioral) tests for the ArpResolver lifecycle, on the host (B5
// Step 2a). ArpResolver owns the resolution lifecycle + retry policy; its
// actions call `crate::net::*`, which the test crate doubles with call
// counters (see lib.rs `mod net`). Each `$Incomplete` entry is one attempt:
// it sends a request and arms the timer; `reply()` resolves; `timeout()`
// re-enters (retransmit) until the attempt cap, then `-> $Failed`.

use frame_os_kernel_tests::{net, ArpResolver};

#[test]
fn construction_sends_first_request_and_arms_timer() {
    net::reset();
    let mut a = ArpResolver::__create();
    // The initial $Incomplete enter handler is the first attempt.
    assert_eq!(net::requests_sent(), 1, "one request on construction");
    assert_eq!(net::timers_armed(), 1, "one timer armed on construction");
    assert!(!a.is_resolved());
    assert!(!a.is_failed());
}

#[test]
fn reply_resolves() {
    net::reset();
    let mut a = ArpResolver::__create();
    a.reply();
    assert!(a.is_resolved());
    assert!(!a.is_failed());
}

#[test]
fn timeout_retransmits_below_the_cap() {
    net::reset();
    let mut a = ArpResolver::__create(); // attempt 1
    a.timeout(); // re-enter $Incomplete → attempt 2
    assert!(!a.is_resolved());
    assert!(!a.is_failed());
    assert_eq!(net::requests_sent(), 2, "retransmit sent a second request");
    assert_eq!(net::timers_armed(), 2, "retransmit re-armed the timer");
}

#[test]
fn reply_after_a_retransmit_still_resolves() {
    net::reset();
    let mut a = ArpResolver::__create();
    a.timeout(); // one retransmit
    a.reply();
    assert!(a.is_resolved());
    assert!(!a.is_failed());
}

#[test]
fn timeouts_to_the_cap_fail() {
    net::reset();
    // max_attempts = 5. Construction is attempt 1; four timeouts re-enter to
    // attempt 5; the fifth timeout hits the cap and routes to $Failed.
    let mut a = ArpResolver::__create();
    for _ in 0..5 {
        a.timeout();
    }
    assert!(a.is_failed(), "gave up after the retry cap");
    assert!(!a.is_resolved());
    assert!(
        net::failed(),
        "arp_on_failed() fired via $Failed's enter handler"
    );
    // Five attempts' worth of requests were sent (1 construct + 4 retransmits).
    assert_eq!(net::requests_sent(), 5);
}

#[test]
fn resolved_is_terminal_to_timeout() {
    net::reset();
    let mut a = ArpResolver::__create();
    a.reply(); // $Resolved
    let before = net::requests_sent();
    a.timeout(); // $Resolved doesn't handle timeout() — ignored
    assert!(a.is_resolved());
    assert_eq!(net::requests_sent(), before, "no retransmit once resolved");
}
