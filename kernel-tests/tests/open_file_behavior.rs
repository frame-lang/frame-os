// kernel-tests/tests/open_file_behavior.rs
//
// Level 3 (behavioral) tests for the OpenFile lifecycle, on the host. OpenFile
// is pure (no native actions). The access mode is the state: an fd opened for
// reading is $Reading, for writing is $Writing, and the wrong operation is
// gated out until close → $Closed.

use frame_os_kernel_tests::OpenFile;

#[test]
fn fresh_file_is_not_open_reading_or_writing() {
    let mut f = OpenFile::__create();
    assert!(!f.is_open());
    assert!(!f.is_reading());
    assert!(!f.is_writing());
}

#[test]
fn open_for_reading() {
    let mut f = OpenFile::__create();
    f.open_read();
    assert!(f.is_open());
    assert!(f.is_reading());
    assert!(!f.is_writing());
}

#[test]
fn open_for_writing() {
    let mut f = OpenFile::__create();
    f.open_write();
    assert!(f.is_open());
    assert!(f.is_writing());
    assert!(!f.is_reading());
}

#[test]
fn close_reading_file() {
    let mut f = OpenFile::__create();
    f.open_read();
    f.close();
    assert!(!f.is_open());
    assert!(!f.is_reading());
}

#[test]
fn write_on_a_read_fd_is_gated_out() {
    // $Reading doesn't handle write() — wrong-mode op is ignored, not applied.
    let mut f = OpenFile::__create();
    f.open_read();
    f.write();
    assert!(
        f.is_reading(),
        "a read-fd stays in $Reading on a stray write"
    );
    assert!(!f.is_writing());
}

#[test]
fn closed_is_terminal() {
    let mut f = OpenFile::__create();
    f.open_read();
    f.close();
    // Everything is ignored once closed.
    f.read();
    f.open_write();
    assert!(!f.is_open());
}
