// shell/src/job_summary.rs
//
// `JobSummary` is the snapshot type `JobControl.jobs()` returns for the
// `jobs` builtin (H3 Step 4) to display. It's plain data — no FSM
// involvement — and lives outside the Frame source so JobControl's `.frs`
// can stay focused on lifecycle dispatch.
//
// Fields are `pub` so JobControl's action body can construct values via
// struct literal `JobSummary { id, state, cmd }` and so tests can read
// them directly.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSummary {
    pub id: u32,
    /// OS process id. Used by the ring-3 shell to echo `[id] pid` on background
    /// launch and to resolve `%job` specs to a pid for `kill`/`bg` (M4). The
    /// hosted display doesn't show it, but carrying it keeps one snapshot type.
    pub pid: u32,
    pub state: String,
    pub cmd: String,
    /// 0 unless the job is in `$Done`. Shell reads this after
    /// wait_foreground to print "[exit code: N]" for non-zero exits
    /// (preserving H2's external-command surface behavior).
    pub exit_code: i32,
}
