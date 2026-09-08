# Intel tiled-source reverse-bridge experiment

The single-copy Intel-primary route is functional and retained as the control:
Intel GLES LINEAR -> pooled NVIDIA Vulkan -> NVIDIA-native output. On DP-1 it
measured 221.647 presentations/sec, versus 133.954/sec for shared-buffer NVIDIA
GLES sampling. Neither sustained 239.760 Hz. The user requested further tuning.

## Candidate

Intel GLES native noncompressed tiled source -> pooled Intel Vulkan detile into
Intel-owned explicit LINEAR -> pooled NVIDIA Vulkan into the actual NVIDIA-native
target. Native Intel outputs remain unchanged. No assumption that an additional
copy is faster: first compare a controlled full-redraw, final-fence-completed
offscreen proxy, then validate in the real compositor if warranted.

## Lifetime requirements

- Intel source writes wait the previous Intel detile reader, not unnecessarily the
  entire NVIDIA scanout path.
- Intel detile writes to LINEAR wait its previous NVIDIA reader (or fallback GLES
  reader). The current Intel detile fence must be stored immediately after submit,
  even if the subsequent NVIDIA leg fails.
- NVIDIA reads LINEAR only after current detile completion and writes its leased
  target only after target acquire. Its completion becomes the next LINEAR-reader
  release and the current KMS completion.
- A post-detile failure cannot free/reuse the LINEAR buffer or hide outstanding
  source reads. Fallback must sample/copy the correct current image and retain all
  submitted fences. No previous-frame selection is permitted.
- Source/layout/size/device-generation changes retire all three relevant roles.
  Both engines keep the existing bounded resources and immutable old completions.
- Query exact single-plane format/modifier/usage/extent support. Intermediate must
  be source-allocator-owned and importable as Intel TRANSFER_DST and NVIDIA
  TRANSFER_SRC. Do not infer support from memory placement or guessed tiling bits.
- Copy only exact current damage; pair-shared staging may contain another output's
  pixels elsewhere. Intermediate/source state must not become an unbounded chain.

## Measurement caveats

The offline proxy deliberately waits the final completion to measure whole-chain
wall latency and excludes CPU pixel-oracle work. It is not GPU execution timing,
normal asynchronous compositor pacing, or proof about browser rendering. Compare
identical source work, full redraw, format, extent and warmup for both routes.

The first implementation/adoption gate is positive proxy results, followed by
correctness review, full pixel/fence tests, and an approved session restart.
During initial investigation, Intel-primary single-copy remained running with recording off.

## Controlled proxy results

Release probe; balanced profile; user paused GPU-heavy test workloads/monitoring;
96 measured frames after16 warmup iterations per size, full redraw/full transfer.
Intel source pinned to the freshly tested Y-tiled modifier72057594037927938 for
two-leg runs; single-copy source verifiedLINEAR. Target stayednativeNVmodifier
216172782120099860. Previous target-oracle GPU reader was waited OUTSIDE the timer,
and the current target GPU oracle/CPU pixel comparison also stayed outside it.

1080p mean source-render-start→final NV VkFence completion (milliseconds):

| Format | LINEAR single-copy repeats | Intel tiled/two-leg repeats |
| --- | --- | --- |
| ABGR8888 | 7.829 / 7.721 | 6.053 / 6.118 |
| ABGR2101010 | 7.793 / 7.755 | 6.112 / 6.137 |

The second1952×1104 size showed the same direction (~8.5ms single vs~6.5–6.7ms
two-leg). 8-bit order was A/B/B/A; 10-bit order was B/A/A/B. Pixels/fences passed;
one pooled set per engine, no steady creation/destruction/busy/signal replacement.
Logs: `/tmp/niri-detile-ab24-*.log` and `/tmp/niri-detile-ab30-*.log`.

This ~20–22% proxy improvement justifies an opt-in production experiment, not a
claim of240Hz performance. Four scissored clears are not textured desktop rendering;
oracle work can still affect cache/power state even when outside the measured span.

## Integration status

Two-leg production integration is implemented behind default-off
`NIRI_VK_SOURCE_DETILE=1`, active only with target-side copying and direct target
writes enabled. Manager policy changes advance the source epoch and retire both
engines/storage. Diagnostics expose `detile on/off` between frames and record the
requested policy plus DetileCopies and separate host DetileCopy/TargetCopy stages.

Forty-eight library timing/transfer/lifecycle tests pass, including the f1/f2/reader
ledger, interrupted retirement, bounded caches, failed optional engine startup,
missing-L and cold/mixed-size source activation. Both initial review findings and
a hardware-detected mixed-size activation defect were fixed: valid NVIDIA caps no
longer stay pending forever after optional Intel init failure; ready native S+L
allocation is one transaction even on the first use of a new extent.

Actual MultiRenderer tests pass in both 8/10-bit, small and full-HD, with three
same/mixed-size native targets, exact sparse damage, shared-context reader ordering,
capture, explicit GLES, blit_to/from, source continuation and cache invalidation.
Every measured non-GLES draw required DetileCopies==DirectCopies>0 and zero CPU or
intermediate copies. Existing single-copy reverse and forward tests still pass
with detiling disabled. Two-leg retained-old-fence stress is not claimed: the probe
explicitly rejects that combination, while core per-engine and ledger tests remain.
Strict linting, feature isolation and 222 niri/config/IPC tests pass.

Smithay implementation checkpoint: `38659b6b`.

## Live result: keep detiling disabled

The user approved candidate niri `f0f77513` / Smithay `38659b6b`, installed separately
as `niri-intel-detile`, with Intel primary and the same NVIDIA-native targets. The
same-binary balanced-profile DP-1 comparison rejected the two-leg optimization:

| Phase (PID3593183) | Duration | Presentations/sec | Late presentations |
| --- | ---: | ---: | ---: |
| Single-copy baseline, epoch4 | 415.06s | 198.296 | 17209 |
| Detile on, epoch9 | 315.05s | 160.602 | 24937 |
| Single-copy recovery, epoch13 | 205.03s | 200.710 | 8006 |

Counters verified DetileCopies=DirectCopies=50,597 during the two-leg trial, two
logical submissions per frame, and no steady resource-set creation/destruction or
CPU-copy fallback. Host detile/target copy setup averaged ~0.193/~0.195ms. Most
frames were queued before their deadline; the extra dependency/copy chain was worse
for real pacing despite its win in the four-clear proxy. This is why the proxy was
not used as an adoption claim. Detiling was turned off and remains default-off.

The source and probe are retained as an experimental path, not a recommended
performance optimization. No renderer/presentation safety failure was observed in
these tests; improved pixel correctness alone does not justify a slower default.

## Matched quiet power-profile control

The user selected performance and reported roughly stable238 FPS with only Chromium
TestUFO. After recording that condition, the user approved changing ONLY the power
profile to balanced, keeping the same single-copy path and quiet workload, then
restoring performance:

| Profile / phase | Duration | Presentations/sec | Late | CPU queue-start late |
| --- | ---: | ---: | ---: | ---: |
| Performance, epoch16 | 105.01s | 239.007 | 79 | 0 |
| Balanced, epoch19 | 80.01s | 187.625 | 4165 | 38 |

DP-1 mode is239.760Hz, not exactly240. Performance greatly reduced variability but
was not perfectly full-rate. Whole-frame host time averaged0.697ms in performance
versus1.591ms in the quiet balanced run. `powerprofilesctl list` identified CpuDriver
`intel_pstate` (balanced PlatformDriver placeholder); current EPP read performance.
Thus CPU power/performance policy is a strong contributor, not evidence isolating
NVIDIA GPU downclocking or one particular clock/firmware mechanism. These are host
wall timings and sequential trials, not direct frequency/GPU-execution measurements.
NVIDIA sequence-gap counts remain unavailable because sequence values repeat.

Raw archive:
`/home/acters/.local/state/niri-pacing/frame-timing-detile-and-power-3593183.jsonl`.
Use the recorded PID/epoch/phase and `tools/analyze-frame-timing.py`.

## Final retained configuration

The user chose to restore NVIDIA-primary pooled rendering. `99-reverse-bridge-test.conf`
was archived and removed; `99-pacing-diagnostics.conf` again selects `niri-pooled`
(`eca35507`), original NVIDIA-primary config, direct target enabled, recording off.
Restoration verified renderD129 and all three original output modes. Performance
profile was left unchanged as selected by the user; no silent power-policy reset.

Single-copy reverse and experimental detile branches, binaries, the Intel test
config, and data remain available. The reverse override is saved at
`/home/acters/.local/state/niri-pacing/reverse-bridge-test.conf.saved` for explicit
future experiments. No new commits have been pushed.
