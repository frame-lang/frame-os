// kernel-tests/tests/ip_reassembly_behavior.rs
//
// Level 3 (behavioral) tests for the IpReassembly fragment-reassembly FSM, on
// the host (B5 Step 6 / B5-5). The FSM owns the *lifecycle*:
//
//   $Idle ─ fragment ─► $Reassembling ─┬─ (is_complete) ─► $Complete
//                          ▲  │         └─ timeout ───────► $Expired
//                          └──┘ fragment  [self, re-store]
//
// Each fragment is threaded in as an enter parameter; the `$Reassembling` enter
// handler calls `crate::ip_reasm::store(frag)` and consults the native
// `is_complete()` guard. Here `ip_reasm` is the capturing host double (see
// lib.rs): `store` counts, `is_complete` is a settable flag, and
// `on_complete`/`on_expired` latch. These tests pin the *transitions*; the
// reassembly *algorithm* (coverage map, reconstruction) is validated end-to-end
// by `cargo xtask qemu-tap`'s `ping -s 4000`.

use frame_os_kernel_tests::{ip_reasm, IpReassembly};

fn frag(offset: usize, len: usize, more: bool) -> ip_reasm::Fragment {
    ip_reasm::Fragment {
        offset,
        len,
        more,
        ident: 0x1234,
    }
}

#[test]
fn starts_idle() {
    ip_reasm::reset();
    let mut r = IpReassembly::__create();
    assert_eq!(r.state(), "Idle");
    assert!(!r.is_complete());
    assert!(!r.is_expired());
}

#[test]
fn first_fragment_enters_reassembling_and_stores() {
    ip_reasm::reset();
    let mut r = IpReassembly::__create();
    r.fragment(frag(0, 1480, true)); // more fragments to come
    assert_eq!(r.state(), "Reassembling");
    assert_eq!(ip_reasm::stored(), 1);
    assert!(!ip_reasm::completed());
}

#[test]
fn additional_fragments_re_store_and_stay_reassembling() {
    ip_reasm::reset();
    let mut r = IpReassembly::__create();
    r.fragment(frag(0, 1480, true));
    r.fragment(frag(1480, 1480, true));
    r.fragment(frag(2960, 1048, true));
    // The self-transition re-enters $Reassembling and re-stores each fragment.
    assert_eq!(r.state(), "Reassembling");
    assert_eq!(ip_reasm::stored(), 3);
    assert!(!ip_reasm::completed());
}

#[test]
fn completing_fragment_transitions_to_complete_and_dispatches() {
    ip_reasm::reset();
    let mut r = IpReassembly::__create();
    r.fragment(frag(0, 1480, true));
    r.fragment(frag(1480, 1480, true));
    // The native coverage map is now full once this final fragment lands.
    ip_reasm::set_complete(true);
    r.fragment(frag(2960, 1048, false)); // final fragment (MF=0)
    assert_eq!(r.state(), "Complete");
    assert!(r.is_complete());
    assert_eq!(ip_reasm::stored(), 3);
    assert!(ip_reasm::completed()); // on_complete() fired (datagram dispatched)
    assert!(!ip_reasm::expired());
}

#[test]
fn single_fragment_that_completes_goes_idle_to_complete() {
    ip_reasm::reset();
    let mut r = IpReassembly::__create();
    // A degenerate "fragment" that is already whole on arrival.
    ip_reasm::set_complete(true);
    r.fragment(frag(0, 1000, false));
    assert_eq!(r.state(), "Complete");
    assert!(ip_reasm::completed());
}

#[test]
fn timeout_in_reassembling_transitions_to_expired() {
    ip_reasm::reset();
    let mut r = IpReassembly::__create();
    r.fragment(frag(0, 1480, true));
    assert_eq!(r.state(), "Reassembling");
    r.timeout(); // a fragment was lost / too slow
    assert_eq!(r.state(), "Expired");
    assert!(r.is_expired());
    assert!(ip_reasm::expired()); // on_expired() fired (partial buffer dropped)
    assert!(!ip_reasm::completed());
}

#[test]
fn idle_ignores_timeout() {
    ip_reasm::reset();
    let mut r = IpReassembly::__create();
    r.timeout(); // nothing in flight
    assert_eq!(r.state(), "Idle");
    assert!(!ip_reasm::expired());
}
