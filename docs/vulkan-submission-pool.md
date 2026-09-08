# Bounded Vulkan submission-resource reuse

## Goal and baseline

Optimize the measured driver-facing resource churn without changing rendering,
damage, synchronization, or KMS ownership semantics. Experimental branch in both
repositories: `nvidia-intel-bridge-pooled`.

Baseline diagnostic niri `7f927e31` / Smithay `6bbc494f`:

- Direct / btop closed: 143.489 KMS presentations/sec.
- Direct / btop hidden/running: 140.464/sec.
- Per-copy command-pool/fence creation: ~0.73–0.79 ms average, up to 16.89 ms
  with btop.
- Semaphore/fence/pool destruction: ~0.65–0.79 ms average, up to 19.79 ms with
  btop. In direct mode, this is largely paid in presentation retirement.

The measurements locate expensive host-side Vulkan lifecycle calls, not an exact
NVIDIA internal lock or firmware mechanism. See `frame-timing-results.md`.

## Required invariants

1. A resource set is recycled only after its submitted GPU work is complete.
2. Old returned SyncPoints remain permanently completed after retirement; they must
   never query a VkFence that has been reset for another submission.
3. KMS keeps owning its scanout slot and native sync-file payload. Reusing a Vulkan
   handle must not recycle or overwrite a still-scanned buffer.
4. Producer acquire, source-reader completion and target-reader release dependencies
   remain unchanged.
5. Source/destination image imports remain alive while GPU commands reference them.
6. Semaphore temporary-import and SYNC_FD export/reset semantics are explicit. A
   failed native export must not cause a signaled binary semaphore to be signaled
   again on reuse.
7. Resource sets, pending work and idle storage are bounded. Old completed fence
   handles must not retain an unbounded chain of heavy GPU resources.
8. Reaping on the render thread must not wait on another thread holding a pending
   fence wait; shutdown/error retirement still preserves memory safety.
9. Device loss is a real transfer error; lost resources are not recycled for new work.

## Planned implementation

Each reusable set contains a command pool/buffer, VkFence and reusable input/output
semaphores. The engine keeps a bounded idle pool. Logical completion is separated
from mutable reusable handles: once a submission is complete, its observable state
is frozen while resources return to the engine pool. Old SyncPoints can remain in
KMS or application state without keeping per-frame teardown on the presentation
callback.

Pool reset/reuse occurs on the engine owner thread. No RELEASE_RESOURCES reset flag
should discard the retained command-pool backing allocations. Native export failure
may require replacing an affected signal semaphore after retirement; this is an
error path, not ordinary per-frame churn.

## Validation gates

- Unit tests for pending/completed/lost transitions, capacity and failure cleanup.
- Existing transfer/framebuffer pixel, damage, capture/shared-context and blit tests.
- Stress more than eight completed retained SyncPoints across reuse and resize,
  including concurrent waiters and engine drop; old completion remains true.
- Timing counters for created/reused/recycled/destroyed resource sets. After warmup,
  steady frames should reuse sets without creating/destroying them.
- Strict lint/feature isolation and niri regression tests.
- Separate optimized binary, with known-good diagnostic and direct-target binaries
  preserved. Coordinate any compositor restart with the user.
- Repeat balanced-profile direct-mode btop closed/open trials using the existing
  low-overhead timing collector. No speculative performance claims before this.

## Implementation and validation progress

The engine now keeps at most eight reusable resource sets. A per-submission mutex
protects pending handles from concurrent queries/waits; owner-thread reaping uses
try_lock. Actual GPU retirement freezes a monotonic logical completion state before
extracting the resource set. Old SyncPoints then retain only completion state and
an independent exported FD, not mutable recycled Vulkan handles. A terminal atomic
cache keeps completed status queries nonblocking and permanently true.

Pre-submit errors return valid storage for reset. Abandoned temporary semaphore
payloads and failed native signal export are handled explicitly on retired checkout.
Actual per-fence status/wait is still required after device loss: a global loss flag
is not proof that every pending batch retired. Queue-submit device loss keeps its
potentially submitted ownership until a real retirement wait (or conservative
ownership retention on an unproven error), never returns it to idle.

Independent ownership/concurrency and Vulkan API reviews found no remaining
confirmed blockers after correcting those device-loss cases and a contended status
query. Thirty-one transfer/timing/lifecycle tests pass. Existing direct-target
8/10-bit pixel, damage, shared-context, capture and blit probes pass.

### Retained-fence stress

For each of ABGR8888 and ABGR2101010, the render-node-only stress ran 256 submissions
across two sizes while retaining all 256 old fences, including one concurrent waiter:

- Resource sets created: 1; reused: 255; recycled before teardown: 255.
- Steady resource destruction, pool-busy results, dirty signal replacements: 0.
- All logical and previously signaled native fences remained signaled across reuse
  and after engine destruction; pixel checks passed.

The first version of the stress incorrectly treated a first zero-time native-FD
poll after VkFence completion as an OLD-fence readiness regression. The same first
poll=0 occurred with the untouched unpooled `6bbc494f` engine in a detached reference
worktree, so pooling was not necessary for it. The final test retains ONE exported
FD per record, records initial readiness delay with a 500 ms hard deadline, never
blocks producer reuse for worker publication, and after the first POLLIN requires
permanent immediate readiness. Timeout, error flags or ready-to-not-ready remain
failures. Initial observed worker delays peaked at 115 us (8-bit) / 97 us (10-bit);
this is a driver/interop observation, not a claim that Vulkan permits semaphore
signaling after its submission fence.

Logs: `/tmp/niri-pool-stress-abgr8888-tracked.log`,
`/tmp/niri-pool-stress-abgr2101010-tracked.log`, and
`/tmp/niri-unpooled-native-fence-control.log`.

## Status

Smithay optimization checkpoint: `e6912c04`.

Offscreen validation passed, including full-HD ten-bit direct-target capture/damage
checks. Strict niri/Smithay linting, feature isolation and 220 niri/config/IPC
regression tests pass. The optimized compositor was subsequently installed and measured as described below.

The temporary unpooled reference worktree was removed after preserving its exact
test patch and log under `/home/acters/.local/state/niri-pacing/`.

## Approved live comparison

The user approved one restart into the optimized release, niri `eca35507` linked
against Smithay `e6912c04`, installed separately as
`/home/acters/.local/bin/niri-pooled`. The prior `niri-pacing` and `niri-direct-target`
binaries remain untouched. Direct-target transfer, LINEAR scanout, quiet logging,
balanced power profile, browser placement and btop settings stayed consistent with
the recorded baseline. Btop ran hidden on an inactive workspace; nvtop/nvidia-smi
were closed during measurement.

| Direct-target mode | Before pooling | Pooled |
| --- | ---: | ---: |
| Btop closed | 143.489 presentations/s (95.00s) | 143.991/s (110.01s) |
| Btop hidden/running | 140.464/s (125.02s) | 143.946/s (105.00s) |
| Btop-on refresh gaps | 441 | 5 |
| Btop-on late presentations | 215 | 3 |

The pooled closed window had zero refresh gaps/late presentations. The btop-on
window had five gaps in 105s, versus 441 in 125s before pooling (about 99% fewer
per second). The user reported stable ~144 FPS in both pooled conditions and
explicitly chose to keep the optimized build.

Steady pooled windows showed Reused=Recycled=DirectCopies=FramesSubmitted
(15,840 closed; 15,115 open), zero ResourceSetsCreated/Destroyed, no pool-busy result,
no signal replacement and no CPU fence fallback/render/queue errors.

With btop running:

- Vulkan resource checkout/reset averaged 0.0107 ms, versus 0.7919 ms for the old
  allocation/setup stage; maximum 0.0712 ms versus 16.8859 ms.
- Presentation retirement averaged 0.0240 ms, versus 0.8142 ms; maximum 0.0600 ms
  versus 19.8169 ms. Heavy driver destruction no longer occurs there in steady use.
- Host Vulkan copy setup/submission averaged 0.2330 ms, versus 0.9903 ms.

This is measured improvement on the tested machine, not perfect pacing or proof of
NVIDIA's internal locking mechanism. Rare delays remain (including a longer render
outlier), and GPU execution duration was not measured. Different-duration sequential
trials are not randomized repeated benchmarks. The known future-hardware-timestamp
caveat remains; zero callback-delay values were not used as proof of ideal delivery.

Raw data: `/home/acters/.local/state/niri-pacing/frame-timing-pooled-20260908-3047773.jsonl`.
PID 3047773, epochs 4 (`pooled_direct_btop_closed`) and 7 (`pooled_direct_btop_open`).
Use `tools/analyze-frame-timing.py` to reproduce the stable-window summaries.

Direct mode remains on and recording is off, including for future restarts.
The service's `99-pacing-diagnostics.conf` selects `niri-pooled`; restoring its
ExecStart to the preserved `niri-pacing` binary (then a coordinated restart) restores
the unpooled measurement build. Removing that drop-in restores the earlier
`niri-direct-target` binary selected by `98-direct-target-test.conf`.
No optimization commits have been pushed.
