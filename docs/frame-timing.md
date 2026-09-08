# Low-overhead frame timing diagnostics

This is an opt-in diagnostic build on `nvidia-intel-bridge-pacing` in niri and the
companion Smithay repository. It does not change the normal rendering algorithm.
The installed known-good `niri-direct-target` binary is preserved.

## What recording does

- CPU elapsed-time guards and counters around niri redraw, scene update, renderer
  acquisition, render-element creation, DRM rendering, existing synchronization,
  KMS queueing, frame callbacks and capture.
- Nested Smithay stages: source reuse wait, target acquire/publication, Vulkan
  validation, previous-batch retirement, import/cache misses, input fence setup,
  existing CPU fallbacks, object creation, command recording, submit, export and
  destruction.
- Actual KMS timestamps and sequence numbers are associated with the queued frame's
  target presentation time and queue-start timestamp. Callback dispatch delay,
  presentation lateness and queue-to-presentation duration are recorded separately.
- CPU wall times include time descheduled or blocked in native driver calls. They
  are NOT GPU execution times. No extra GPU timestamps, fence-status queries,
  waits, driver telemetry calls, or NVML polling are introduced by recording.
- Fixed-size histograms in bounded thread-local storage. No normal per-frame heap
  allocation, lock, formatting or file I/O. Inactive worker threads do not record.
- Every five seconds the event loop copies raw snapshots into a bounded two-message
  channel. A separate thread computes summaries and writes JSONL. Slow file I/O
  cannot block frame recording; dropped windows are reported, not silently merged.

There is nonzero observer cost. A release CPU-only microbenchmark with 16 timed
sections and two counters measured approximately 0.77 us per simulated frame with
recording enabled versus 0.044 us disabled. This excludes setup, periodic snapshot
allocation/copy and background writing, and is not a bound on whole-compositor cost.
A live recording-off/on comparison remains necessary.

## Enabling the diagnostic build

The user service must start the instrumented binary with:

```ini
Environment=SMITHAY_FRAME_TIMING=1
Environment=NIRI_FRAME_TIMING_FILE=/tmp/niri-frame-timing.jsonl
Environment=NIRI_VK_DIRECT_TARGET=1
Environment=RUST_LOG=info
```

No reporter thread, timer, file or control socket starts when instrumentation is
not enabled at startup. The file is append-only, created mode 0600. Check its session
header and fresh windows before trusting a run. Keep per-frame debug logging off.

## Controls without additional restarts

An owner-only socket is created under the validated private `XDG_RUNTIME_DIR`:
`niri-pacing-PID.sock`. Commands run on the event loop between frames, never by
polling a control file on the rendering path. The helper waits for a direct status
reply independent of the file-writer queue:

```sh
python3 tools/pacing-control.py status
python3 tools/pacing-control.py record off
python3 tools/pacing-control.py record on
python3 tools/pacing-control.py mark direct_on_btop_closed
python3 tools/pacing-control.py direct off
python3 tools/pacing-control.py mark direct_off_btop_closed
python3 tools/pacing-control.py direct on
```

`record off` stops the per-frame clocks/counters, but leaves the diagnostic reporter,
control socket and timer alive. It measures recording-path overhead, not complete
startup instrumentation-off overhead.

`direct off` uses the existing intermediate-transfer fallback. Crucially, this
control leaves the startup LINEAR swapchain allocation policy unchanged, isolating
the transfer/presentation path rather than also changing the buffer modifier. It
requires starting with direct mode enabled. The existing manager setter retires and
recreates transfer state; ignore transition/warmup, and confirm actual DirectCopies
or IntermediateCopies counters instead of trusting only the requested flag.

Each accepted mutation advances a trial epoch and discards the partial CPU window.
Queued frames carry that epoch: old in-flight and recording-off frames cannot be
relabelled into a newly started trial. Hardware timestamps are saved before any
upstream throttling and matched to the same sequence. `status` never changes epochs,
redraws or policy. If a mutation times out, query status rather than blindly retrying
it. Status exposes writer health; also require fresh report rows.

## Controlled comparison

Use a fixed power profile (initially balanced), fixed browser placement on eDP-1,
fixed TestUFO workload and the same btop settings/update interval. Close nvtop and
other GPU monitoring. Do not run nvidia-smi during collection.

1. Check recording off/on with btop closed.
2. Direct on: warm up, mark and collect 60 seconds with btop closed.
3. Open btop (preferably hidden on an inactive workspace to remove visible redraw
   work), wait for startup, mark and collect 60 seconds.
4. Close btop; collect a recovery window if needed.
5. Direct off: retain the same LINEAR swapchains, warm up until intermediate copies
   are active, then repeat closed/open conditions.
6. Restore direct on and the desired recording state.

Changing rendering mode or restarting the service requires the user's approval.
No diagnostic result should be used to justify removing required synchronization.

## Analysis and interpretation

```sh
python3 tools/analyze-frame-timing.py /tmp/niri-frame-timing.jsonl --output eDP-1
```

The helper groups by PID, epoch, phase and direct flag; excludes startup (unless
explicitly requested), registration/transition, dropped and short partial windows.
It merges counts/totals/extrema. It reports the worst single-window p99, NOT an
incorrect average or merged p99. Raw histogram quantiles are upper-bound buckets:
25 us to 1 ms, 100 us to 5 ms, 250 us to 20 ms, then overflow. Exact min/max and totals
remain available. Lower-tail queue/render headroom is rounded upward, so exact
minimum and explicit late counters are useful anchors.

- NiriRedraw includes nested NiriFrame and other phases; do not sum nested timings.
- QueueLead/QueueLate refer to queue-call START. QueueReturnLead/QueueReturnLate
  refer to its RETURN. A call can return late after a frame presented on time.
- PresentLate counts presentation at least half a refresh period after the chosen
  target, not tiny timestamp noise. PresentCallbackDelay is kernel timestamp to
  first DRM-event handling, not Wayland frame-callback delivery.
- SequenceGaps can be idle/client gaps. Treat them as missed busy opportunities only
  during the controlled continuously animating workload.
- CPU fallback counters require a fence but it may already be signaled. Use duration,
  not count alone, to infer blocking. VulkanFenceWait includes live and retirement
  waits. Explicit destructor-body timings exclude later automatic field destruction.
- Snapshots dropped due backpressure are lost, not accumulated into the next window.
  Do not extend a surviving window's elapsed time over dropped intervals or sum
  global drop counts once per output.
- Stop/restart can lose partial or queued trailing windows; exclude run boundaries.
  Work outside attributed redraw/vblank/callback scopes, including some client
  request handling and early imports, is not fully covered.

## Status

Implementation and unit/measurement review are complete. Smithay checkpoint:
`6bbc494f`. No instrumented session has been installed or measured yet. Any eventual bottleneck conclusion must come
from the controlled measurements, not from the existence of these counters.
