// kernel-tests/tests/kernel_behavior.rs
//
// Level 3 (behavioral) tests for the Kernel HSM, run on the host.
//
// These construct the Kernel directly and assert on (a) the state it
// settles in, observed via `is_done()`, and (b) what its actions printed,
// observed via the capturing `serial` module.
//
// A note on what's testable here, and what isn't:
//
//   `Kernel::__create()` runs the *entire* boot chain synchronously — each
//   init child's `$>` enter handler immediately transitions to the next,
//   so by the time the constructor returns the kernel is already in
//   `$Running`. The five boot children ($InitMemory … $LaunchInit) are
//   therefore transient: they exist only during construction and are never
//   externally observable.
//
//   A consequence: the panic-forwarding path (`=> $^` from each boot child
//   to `$Booting`'s `kernel_panic` handler) is NOT reachable from outside.
//   No external `kernel_panic()` call can land while the kernel is in a
//   boot child, because control never returns to the caller mid-chain. The
//   forwarding is real and would fire if a boot phase's own `$>` handler
//   called `self.kernel_panic(...)` (a phase failing its init work) — but
//   the B0 stubs never self-panic, so there's nothing to observe yet.
//
//   What we CAN test behaviorally at B0:
//     - the boot chain progresses through all phases to $Running
//       (observed via the captured phase banners), and
//     - `$Running`'s OWN `kernel_panic` handler — the runtime-panic
//       variant, distinct from `$Booting`'s — fires and lands in $Halted.
//
//   Testing boot-child forwarding directly needs either a fault-injection
//   hook in a boot phase or restructuring the boot chain to be event-
//   stepped (so children are observable between steps). That's an open
//   design question recorded in docs/systems/kernel.md, not a gap to paper
//   over here.

use frame_os_kernel_tests::{serial, Kernel};

#[test]
fn fresh_kernel_runs_boot_chain_to_running_not_done() {
    serial::clear();
    let mut k = Kernel::__create();
    assert!(
        !k.is_done(),
        "after the boot chain, Kernel should rest in $Running (is_done == false), \
         not $Halted"
    );
}

#[test]
fn boot_chain_prints_all_phases_in_order() {
    serial::clear();
    let _k = Kernel::__create();
    let out = serial::captured();

    // Each banner is one init child's `$>` enter handler; the order is the
    // HSM's transition order. `[run] kernel running` is `$Running`'s enter
    // handler. Assert they appear in sequence (not just present).
    let in_order = [
        "[boot] init memory",
        "[boot] init IDT",
        "[boot] init timer",
        "[boot] init console",
        "[boot] launching init",
        "[run] kernel running",
    ];
    let mut from = 0usize;
    for needle in in_order {
        match out[from..].find(needle) {
            Some(rel) => from += rel + needle.len(),
            None => panic!("missing (or out-of-order) phase {needle:?} in captured output:\n{out}"),
        }
    }
}

#[test]
fn panic_in_running_prints_runtime_message_and_halts() {
    serial::clear();
    let mut k = Kernel::__create();
    assert!(
        !k.is_done(),
        "precondition: kernel is in $Running after boot"
    );

    k.kernel_panic("disk on fire".to_string());

    let out = serial::captured();
    assert!(
        out.contains("KERNEL PANIC during runtime: disk on fire"),
        "expected $Running's runtime-panic message; got:\n{out}"
    );
    assert!(
        k.is_done(),
        "after a runtime panic, Kernel should be in $Halted (is_done == true)"
    );
}

#[test]
fn runtime_panic_uses_running_variant_not_boot_variant() {
    // The Frame argument: the SAME kernel_panic() event is dispatched
    // differently per state. From $Running we must get the "runtime"
    // wording, never the "boot" wording — proving $Running's handler ran,
    // not $Booting's.
    serial::clear();
    let mut k = Kernel::__create();
    k.kernel_panic("nope".to_string());

    let out = serial::captured();
    assert!(
        out.contains("KERNEL PANIC during runtime: "),
        "expected runtime variant; got:\n{out}"
    );
    assert!(
        !out.contains("KERNEL PANIC during boot: "),
        "must NOT use the $Booting variant from $Running; got:\n{out}"
    );
}

#[test]
fn is_done_only_after_panic() {
    serial::clear();
    let mut k = Kernel::__create();
    // is_done is false in $Running...
    assert!(!k.is_done());
    // ...and is idempotent on repeated reads (no transition side effect).
    assert!(!k.is_done());
    k.kernel_panic("halt now".to_string());
    // ...and true once we've reached $Halted.
    assert!(k.is_done());
    assert!(
        k.is_done(),
        "is_done in $Halted should be stable across reads"
    );
}
