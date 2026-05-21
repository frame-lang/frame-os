// kernel-tests/tests/mount_behavior.rs
//
// Level 3 (behavioral) tests for the Mount lifecycle, on the host. Mount is
// pure (no native actions). It makes the legal mount sequence structural:
// $Unmounted → $Mounting → $Mounted → $Unmounting → $Unmounted, with a
// mount-failed path back to $Unmounted.

use frame_os_kernel_tests::Mount;

#[test]
fn fresh_mount_is_not_mounted() {
    let mut m = Mount::__create();
    assert!(!m.is_mounted());
}

#[test]
fn successful_mount_reaches_mounted() {
    let mut m = Mount::__create();
    m.begin_mount();
    m.mounted_ok();
    assert!(m.is_mounted());
}

#[test]
fn failed_mount_returns_to_unmounted() {
    let mut m = Mount::__create();
    m.begin_mount();
    m.mount_failed();
    assert!(!m.is_mounted());
    // ...and a retry can still succeed.
    m.begin_mount();
    m.mounted_ok();
    assert!(m.is_mounted());
}

#[test]
fn unmount_round_trips_back_to_unmounted() {
    let mut m = Mount::__create();
    m.begin_mount();
    m.mounted_ok();
    m.begin_unmount();
    m.unmounted();
    assert!(!m.is_mounted());
}

#[test]
fn mounted_ok_before_begin_is_ignored() {
    // Per explicit-only-forwarding, $Unmounted doesn't handle mounted_ok.
    let mut m = Mount::__create();
    m.mounted_ok();
    assert!(!m.is_mounted());
}
