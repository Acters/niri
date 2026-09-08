# Measured btop / bridge pacing results

## Setup

Diagnostic niri `7f927e31`, Smithay `6bbc494f` instrumentation lineage, same process
PID 2749454 throughout. TestUFO was on eDP-1; balanced power profile was kept fixed;
nvtop/nvidia-smi were closed. Btop was running on an inactive workspace for the
btop-on conditions. No profiling build jobs ran during the measured trials.

Direct policy was changed between frames using the private diagnostic control.
**Scanout allocation remained LINEAR in both modes.** Thus direct-off here means
an intermediate copy plus Intel GLES blit into the same LINEAR scanout policy, not
a recreation of the earlier tiled-scanout phase-two configuration.

Recording-off baseline was visually stable at 143 FPS. Recording-on direct-mode
baseline was visually unchanged. The CPU-only recorder microbenchmark measured
~0.77 us for 16 timed sections, excluding periodic reporting; this is not a claim
of zero observer cost.

Only stable full windows were analyzed: registration/transition, loss and partial
windows excluded, queue epochs preserved. All route counts matched the intended
DirectCopies/IntermediateCopies path. No CPU fence-wait fallbacks or render/queue
errors appeared in these stable windows.

## Presentation rates

These are actual associated KMS presentation event counts divided by recorded
window duration, not browser FPS estimates or per-copy debug-log rates.

| Path | Btop | Stable duration | Presentations/sec | Sequence gaps | Late presentations |
| --- | --- | ---: | ---: | ---: | ---: |
| Direct | Closed | 95.00 s | 143.489 | 48 | 48 |
| Direct | Open, hidden | 125.02 s | 140.464 | 441 | 215 |
| Intermediate, LINEAR target | Open, hidden | 140.02 s | 121.026 | 3217 | 2925 |
| Intermediate, LINEAR target | Closed | 140.01 s | 134.544 | 1323 | 1323 |

Late means at least half a refresh period after the selected target time. Sequence
gaps can include application/scheduling gaps; in these trials the workload was
continuously animating. Different observation durations are normalized for rates.
These were controlled comparisons, not randomized repeated benchmark runs. As a
consistency check, `(PresentEvents + SequenceGaps) / elapsed` is approximately
143.99–144.00 Hz in every condition, matching the physical 143.998 Hz mode. The
measured loss is therefore in presentation cadence, not a switch to a 140 Hz mode.

## Located CPU stalls

All durations below are host elapsed time, including possible driver blocking or
CPU descheduling. They are NOT GPU copy execution times.

| Path / btop | Vulkan resource creation mean / max | Resource destruction mean / max | KMS queue call mean / max |
| --- | ---: | ---: | ---: |
| Direct / closed | 0.732 / 5.722 ms | 0.650 / 4.430 ms | 0.105 / 0.264 ms |
| Direct / open | 0.792 / 16.886 ms | 0.789 / 19.793 ms | 0.096 / 1.862 ms |
| Intermediate / closed | 0.838 / 4.695 ms | 0.730 / 5.240 ms | 0.117 / 0.241 ms |
| Intermediate / open | 1.012 / 23.443 ms | 0.794 / 16.163 ms | 0.111 / 1.797 ms |

`VulkanResources` covers per-copy batch storage, command pool and VkFence creation.
`ResourcesDestroy` covers semaphore, fence and command-pool destruction in
`vkbridge.rs`. Existing Vulkan fence waits were short (maximum below 0.1 ms), and
native input imports numbered two per copy. The measured CPU fallback counters
were absent. This does not rule out GPU-side dependency waits, which are not timed
by this CPU-only recorder.

A particularly useful lifecycle observation:

- In direct mode, `NiriPresentRetire` closely matches `ResourcesDestroy`: with btop
  open, maxima were 19.817 and 19.793 ms respectively. The current KMS frame retains
  the returned Vulkan SyncPoint/Batch. Replacing that frame can drop the last owner
  and perform driver-resource destruction inside the presentation callback, before
  the next redraw/callback work proceeds.
- In intermediate mode, KMS retains the final GLES fence instead. Presentation
  retirement is cheap (~0.029 ms average); Vulkan resource destruction instead falls
  into rendering-side lifetime handling. The cost still exists, and the added GLES
  work makes the measured LINEAR-target configuration substantially slower.

Almost all hardware timestamps were flagged as future relative to callback entry;
we did not use the clamped callback-delay field as proof of zero dispatch latency.
The host creation/destruction durations do not depend on that timestamp comparison.

## Conclusion and next change

The measurements locate a substantial, monitoring-sensitive cost in **CPU-side
Vulkan resource creation/destruction**, rather than showing a large KMS queue-call
cost or a CPU completion-fence fallback. Direct-to-scanout is not the sole cause:
disabling it did not cure the drops and was worse in the fixed-LINEAR comparison.

This establishes where time is being lost, but not the driver's internal locking
mechanism or that all remaining misses have one cause. GPU execution times were
not measured. The additional GPU/blit path can account for misses where the CPU
queued before the selected deadline.

The next targeted optimization should reuse a bounded set of completed submission
resources and decouple heavyweight retirement from the presentation callback.
Reuse must occur only after completion, and old returned SyncPoints must remain
semantically valid even if an underlying resource slot is recycled. Simply removing
waits, resetting fences still referenced by old SyncPoints, or moving churn to a
thread without reducing driver contention would not be a justified fix.

No rendering optimization was implemented during this measurement task.

## Preserved evidence and final state

Raw JSONL: `/home/acters/.local/state/niri-pacing/frame-timing-20260908-2749454.jsonl`.
Analyze using `tools/analyze-frame-timing.py`, selecting output eDP-1 and the desired
phase. Epochs 4, 7, 12 and 15 identify the four table rows (direct closed/open,
intermediate open/closed).

Direct mode was restored, recording switched off, and the user reported recovery
to approximately 142–144 FPS. Power profile remains balanced. Recording is also
disabled in the service drop-in for future restarts. The diagnostic binary remains
installed separately; the known-good direct-target binary is untouched. Remove
`99-pacing-diagnostics.conf` and restart at a coordinated time to return to it.
