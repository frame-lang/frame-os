# B5 Step 4 plan — `TcpConnection` (the RFC-793 state machine)

> **Status:** Planning. Decisions locked 2026-05-21. See [`../roadmap.md`](../roadmap.md) Track B → B5 and [`b5.md`](b5.md). The headline milestone — the one the "stress-test Frame" thesis is pointed at. Large; broken into sub-steps, each host- or QEMU-validated. Builds on the B5 NIC + `RxPipeline` (the `$Tcp` leaf delivers segments) and the timer-via-enter-handler pattern (`ArpResolver`).

## Goal
Model a TCP connection as the Frame state machine it canonically is — the full RFC-793 graph — and drive a real handshake + request/response + clean close against an **external client** (the host's own TCP stack, via the smoke harness). If Frame expresses a correct TCP FSM cleanly, that's the headline result.

## Locked decisions
1. **Open direction:** the FSM models **all** RFC-793 states. Live-test **passive open** (kernel as server, via slirp `hostfwd`) first; add **active open** (`$SynSent`, kernel as client via slirp `guestfwd`) as a follow-on. Both paths get host behavioral coverage from the start.
2. **External peer = the xtask harness itself** (Rust `std::net`): no `nc`/`curl` dependency. The harness gains a per-test "TCP probe" hook (connect-with-retry, send, recv, close); for active open it also runs a tiny host listener.
3. **Single connection first:** one `TcpConnection` instance + a one-entry 4-tuple demux, structured so a connection *table* (the manager+instances pattern, like `ProcessTable`) drops in later. Get the FSM correct end-to-end before adding scale.
4. **Short MSL for tests:** `$TimeWait` uses a small MSL (e.g. 250 ms) so the close test doesn't wait 60 s. Configurable; the *structure* (timer armed on entry, fires → `$Closed`) is what matters.
5. **Minimal TCP:** no options we must honor (MSS/window-scale/SACK/timestamps) — we don't offer them and ignore unknown options in the peer's SYN. Fixed receive window, no Nagle, no congestion control beyond a retransmit timer. Enough for a correct handshake/echo/close that interoperates with a real client.

## How RFC-793 maps to Frame (the design)

**States (the full graph):** `$Closed` (initial/terminal) · `$Listen` · `$SynSent` · `$SynReceived` · `$Established` · `$FinWait1` · `$FinWait2` · `$Closing` · `$TimeWait` · `$CloseWait` · `$LastAck`. An **`$Open` parent** (the `Process.$Alive` / `PageFaultHandler.$FaultActive` pattern) holds `rst()` and `abort()`; every active child forwards via `=> $^`, so an RST from any state funnels to one `-> $Closed` disposition written once.

**Segments are the event, processed per-state.** The `RxPipeline` `$Tcp` leaf parses a `TcpSegment` descriptor and fires `segment(seg: TcpSegment)`. Each state's handler inspects `seg` with native `if` guards (Frame guards = native conditionals around transitions) and transitions:
- `$Listen.segment`: `seg.syn` → send SYN-ACK, `-> $SynReceived`.
- `$SynSent.segment`: `seg.syn && seg.ack` → send ACK, `-> $Established`; `seg.syn` only → `-> $SynReceived` (simultaneous open).
- `$SynReceived.segment`: `seg.ack` → `-> $Established`.
- `$Established.segment`: `seg.fin` → ACK it, `-> $CloseWait`; else data → deliver + ACK (stay).
- `$FinWait1.segment`: ACK of our FIN → `-> $FinWait2`; peer FIN → `-> $Closing` (simultaneous close).
- `$FinWait2.segment`: peer FIN → ACK, `-> $TimeWait`.
- `$Closing.segment`: ACK of our FIN → `-> $TimeWait`.
- `$CloseWait`: app `close()` → send FIN, `-> $LastAck`.
- `$LastAck.segment`: ACK of our FIN → `-> $Closed`.
- `$TimeWait`: `timeout` → `-> $Closed`.

`TcpSegment { src_port, dst_port, seq, ack, flags, window, payload_off, payload_len }` is a `Clone + Default + Debug` struct threaded as the **event** param (typed on both framecs); the payload bytes stay in the native `RX_FRAME`. State-scoped values that flow forward (e.g. the peer's ISN into `$SynReceived`) ride **enter params**.

**Timers via enter/exit handlers + the native wheel (post/drain).** `$TimeWait.$>` arms the 2·MSL timer (`<$` cancels); `timeout` → `$Closed`. `$SynSent`/`$SynReceived`/`$LastAck`/`$FinWait1` arm a retransmit timer in `$>` and resend on `timeout`. The `$Established` data-retransmit timer is armed/cancelled by the `send`/`ack` handlers (within-state). State vars hold RTO/retry counts and reset on (re-)entry. A small **native timer wheel** (`tcp.rs`) tracks the per-connection deadlines and `post`s a `timeout(kind)` event that the kernel `drain`s — the same boundary as B4/`ArpResolver`, now with more than one timer.

## Native / Frame split
- **Native (`tcp.rs`):** TCP segment encode/parse + checksum (with the IPv4 pseudo-header); ISN generation; send buffer (unacked) + receive buffer; sequence/window arithmetic helpers (called from guards); the per-connection timer wheel (retransmit + TIME_WAIT) + its drain; the 4-tuple demux (one entry now); the actions the FSM calls (`send_syn`/`send_syn_ack`/`send_ack`/`send_fin`/`send_data`/`deliver_data`/`arm_*`/`cancel_*`).
- **Frame (`TcpConnection`):** the RFC-793 state graph — which state, what each segment/timeout/app-event does in each state, the `=> $^` RST/abort funnel. The deepest Frame system in the project.

## Sub-steps (each validated)
1. **4a — segments + the FSM skeleton (host-validated).** Native TCP encode/parse/checksum + the `RxPipeline` `$Tcp` leaf building a `TcpSegment`. The full 11-state `TcpConnection` + the `$Open` funnel, with the state-graph snapshot (B5-1, reviewed against RFC-793) and per-transition behavioral tests including the hard edges — simultaneous open (`$SynSent`→`$SynReceived`), simultaneous close (`$FinWait1`→`$Closing`), and RST-from-any-state (B5-2). No live traffic yet.
2. **4b — passive handshake (QEMU).** Kernel `open_passive()` → `$Listen` on a port; slirp `hostfwd=tcp::PORT-:N`; the harness connects (new TCP-probe hook, connect-with-retry); 3-way handshake → `$Established`. Smoke: `[tcp] established`.
3. **4c — data echo (QEMU).** `$Established` receives data, echoes it with correct seq/ack/window; the harness sends a request and reads the echo back. Smoke asserts the kernel's log *and* the harness verifies the echoed bytes.
4. **4d — close + TIME_WAIT (QEMU).** Passive close (peer FIN → `$CloseWait` → app `close()` → `$LastAck` → ACK → `$Closed`) and active close (`$FinWait1`→`$FinWait2`→`$TimeWait`→`$Closed`) with the short-MSL `$TimeWait` timer. Smoke: clean close, no RST. **B5-4 met.**
5. **4e — active open (QEMU) + retransmit.** Kernel `open_active()` → `$SynSent` connecting to a harness-hosted host listener via slirp `guestfwd`; handshake from the client side. Plus retransmit-timer behavioral coverage (drop-the-first-segment simulation on the host).
6. **4f — docs + diagrams + CI.** The `TcpConnection`-vs-RFC-793 comparison write-up (B5-6); per-system doc + SVG; full CI + `qemu-test` (B5-7).

## Test approach (the harness as peer)
- **Passive:** `qemu_base_command` adds `hostfwd=tcp::<hostport>-:<guestport>`. `run_smoke_test` gains an optional `tcp_probe` describing {host port, bytes to send, expected echo}; after spawning QEMU it connects (retrying for a few seconds while the kernel boots to `$Listen`), sends, reads the echo, closes — then the usual serial assertions run. Self-contained Rust, CI-friendly.
- **Active:** the harness binds a host listener and adds `guestfwd=tcp:<slirp-addr>:<port>-tcp:127.0.0.1:<hostport>`; the kernel `open_active()`s to `<slirp-addr>:<port>`. (guestfwd is the trickier transport — validate early in 4e; fall back to behavioral-only for active open if it proves flaky in CI.)
- The host's TCP stack is a **correct RFC-793 oracle** — if our handshake/echo/close interoperates with real `std::net`, the FSM is right. Bad checksums / seq errors make the peer drop or RST, surfacing bugs immediately.

## framec gates exercised (the deepest in the roadmap)
- **HSM at scale** — 11 states + a parent `=> $^` funnel; the largest Frame system, stress-testing the framepiler's HSM codegen.
- **Timed behavior without a primitive** — retransmit + TIME_WAIT via enter/exit + state vars + the native wheel + post/drain (the confirmed answer to "Frame has no `after(ms)`").
- **Typed data threading** — a `TcpSegment` struct as the event param + state-scoped values (ISN) via enter params (the new typed-context codegen).
- **Guards** carrying real seq/window predicates.

## Risks
- **TCP correctness** (seq/window/retransmit/the close states + simultaneous open/close) — the deepest risk. Mitigate: per-transition host behavioral tests against RFC-793 *first* (4a), then validate against the real host client.
- **Checksum (pseudo-header)** — mitigate with host tests; a wrong checksum makes the real peer drop the segment, so it surfaces fast.
- **Option interop** — real clients send MSS/window-scale/timestamps in SYN; we ignore unknown options and offer none. Risk the peer dislikes a missing MSS; mitigate by offering MSS only if needed. Validate against both Linux and macOS `std::net` early.
- **`guestfwd` for active open** — trickier transport; flagged for early validation in 4e, behavioral-only fallback.
- **Harness/QEMU timing** — the client must retry-connect until the kernel reaches `$Listen`; bound by the test timeout.
- **Scope** — six sub-steps. Mitigate: commit per sub-step, validate each, same rhythm as B3/B4.

## Test mapping
B5-1 (`TcpConnection` snapshot vs RFC-793) → 4a. B5-2 (per-transition behavioral incl. hard edges) → 4a (+ retransmit in 4e). B5-4 (handshake + request/response + clean close vs external client) → 4b/4c/4d. B5-6 (docs incl. RFC-793 write-up) → 4f. B5-7 (diagrams + CI) → 4f. (B5-3 ICMP responder + B5-5 ARP/reassembly ride the TAP transport upgrade, tracked separately.)
