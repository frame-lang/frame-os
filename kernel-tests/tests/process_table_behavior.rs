// kernel-tests/tests/process_table_behavior.rs
//
// Level 3 (behavioral) tests for the ProcessTable manager FSM, on the host.
// ProcessTable holds a Vec<Process> and forwards lifecycle ops by pid; its own
// two states model the capacity invariant ($HasCapacity vs $Full). The
// capacity is a constructor param so tests can fill a small table.
//
// Under test: spawn admits to $Ready and assigns ascending pids; the table
// rejects spawn when $Full; by-pid ops (block/unblock/exit/kill) are forwarded
// once from the $Managing parent; reap frees a slot and recovers capacity.

use frame_os_kernel_tests::ProcessTable;

#[test]
fn fresh_table_has_capacity_and_is_empty() {
    let mut t = ProcessTable::__create(4);
    assert!(!t.is_full());
    assert_eq!(t.count(), 0);
}

#[test]
fn spawn_admits_to_ready_with_ascending_pids() {
    let mut t = ProcessTable::__create(4);
    let p1 = t.spawn();
    let p2 = t.spawn();
    assert_eq!(p1, 1);
    assert_eq!(p2, 2);
    assert_eq!(t.count(), 2);
    assert_eq!(t.pid_state(1), "Ready", "spawn admits straight to Ready");
    assert_eq!(t.pid_state(2), "Ready");
}

#[test]
fn unknown_pid_reports_none() {
    let mut t = ProcessTable::__create(4);
    t.spawn();
    assert_eq!(t.pid_state(999), "None");
}

#[test]
fn table_fills_and_rejects_further_spawns() {
    let mut t = ProcessTable::__create(2);
    assert_eq!(t.spawn(), 1);
    assert_eq!(t.spawn(), 2);
    assert!(t.is_full(), "two spawns into a capacity-2 table → $Full");
    assert_eq!(
        t.spawn(),
        0,
        "spawn on a full table returns pid 0 (rejected)"
    );
    assert_eq!(t.count(), 2, "rejected spawn did not add an entry");
}

#[test]
fn by_pid_lifecycle_is_forwarded() {
    let mut t = ProcessTable::__create(4);
    let pid = t.spawn();
    t.block_pid(pid);
    assert_eq!(t.pid_state(pid), "Blocked");
    t.unblock_pid(pid);
    assert_eq!(t.pid_state(pid), "Ready");
    t.exit_pid(pid, 7);
    assert_eq!(t.pid_state(pid), "Zombie");
}

#[test]
fn kill_pid_zombifies_the_named_process() {
    let mut t = ProcessTable::__create(4);
    let a = t.spawn();
    let b = t.spawn();
    t.kill_pid(a);
    assert_eq!(t.pid_state(a), "Zombie");
    assert_eq!(t.pid_state(b), "Ready", "kill targets only the named pid");
}

#[test]
fn reap_returns_status_frees_slot_and_recovers_capacity() {
    let mut t = ProcessTable::__create(2);
    let a = t.spawn();
    let b = t.spawn();
    assert!(t.is_full());

    t.exit_pid(a, 33);
    let code = t.reap_pid(a);
    assert_eq!(code, 33, "reap returns the process's exit code");
    assert_eq!(t.count(), 1, "reaping frees the slot (entry removed)");
    assert!(
        !t.is_full(),
        "freeing a slot recovers capacity: $Full → $HasCapacity"
    );
    assert_eq!(
        t.pid_state(a),
        "None",
        "the reaped process is gone from the table"
    );
    assert_eq!(t.pid_state(b), "Ready");

    // Capacity recovered → a new spawn succeeds and reuses the freed room.
    let c = t.spawn();
    assert_ne!(c, 0, "spawn succeeds again after a reap freed a slot");
}

#[test]
fn reap_of_non_zombie_returns_error_and_changes_nothing() {
    let mut t = ProcessTable::__create(4);
    let pid = t.spawn(); // $Ready, not a zombie
    assert_eq!(t.reap_pid(pid), -1, "reaping a non-zombie returns -1");
    assert_eq!(t.count(), 1, "nothing was freed");
    assert_eq!(t.pid_state(pid), "Ready");
}
