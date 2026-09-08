# Intel-renderer / NVIDIA-scanout bridge

## Baseline and scope

Working pooled NVIDIA-renderer build: niri `eca35507`, Smithay `e6912c04`.
Experimental reverse-route branch in both checkouts: `intel-nvidia-bridge`.
The installed `niri-pooled` binary and NVIDIA-primary session remain unchanged
until a separately approved test.

Goal: fixed Intel primary GLES rendering and native eDP scanout; use a pooled,
same-frame transfer for NVIDIA-connected external outputs. This is not automatic
GPU migration or separate per-output scene rendering.

## Candidate route

```text
Intel GLES scene -> Intel-owned explicit LINEAR staging
    -> NVIDIA Vulkan transfer -> NVIDIA-owned native output buffer -> NVIDIA KMS
```

The copy engine needs to run on NVIDIA for this candidate: historical observations
say NVIDIA can import Intel LINEAR but not Intel tiled, and Intel cannot import
NVIDIA's native tiled output buffers. These are historical leads, not substitutes
for fresh format/size/modifier/fence tests on the current drivers.

Use the existing pool, immutable completion state, exact damage, source/destination
reuse fences, paired target framebuffer hooks and DRM swapchain ownership. Do not
hard-code a private tiling layout or assume that reversing node arguments suffices.

## Required negotiations

- Explicit copy-device role per source/target pair; reset safely if that role changes.
- Source modifiers = Intel GLES renderable intersect chosen copy GPU TRANSFER_SRC.
- Destination modifiers = target allocator/GLES/KMS candidates intersect chosen copy
  GPU TRANSFER_DST at the exact format/extent.
- NVIDIA native swapchain formats may differ from its preferred GLES-only modifier;
  negotiate capabilities, not a hard-coded modifier number.
- Keep valid shared DMA-BUF + target-GLES copying as fallback/control. It may already
  work in this direction through external textures, so direct-Vulkan preference must
  be explicit rather than relying on shared import to fail.
- Native Intel eDP must retain ordinary Intel formats, not be forced LINEAR.

A tiled Intel source plus Intel detile-to-LINEAR plus NVIDIA copy is a possible
second-stage optimization only if measurements justify two engines/intermediate
ownership. Start with the minimal single-copy candidate and measure it honestly.

## Gates

1. Offscreen exact 8/10-bit source/copy/target-node and modifier/fence/pixel tests.
2. Actual pooled MultiRenderer reverse tests: partial damage, multiple target
   buffers/sizes, shared reads/capture/blits, retained fences and invalidation.
3. Kernel acceptance and an approved Intel-primary session, preserving rollback.
4. Measure external-output pacing and resource reuse; do not infer speed or power
   benefits from architecture alone.

## Offscreen validation and integration

Fresh tests on the current machine passed Intel renderD128 -> NVIDIA copy/target
renderD129. The negotiated source set was LINEAR only; destination candidates were
native NVIDIA modifiers, with actual GBM allocation `216172782120099860`.

- ABGR8888 and ABGR2101010 engine probes: exact pixels, partial damage and two sizes.
- Reverse pool stress: 128 retained completions in each format, one resource set,
  127 reuses, no steady destruction/busy/signal replacement; concurrent waits and
  old native/logical fence validity passed through engine destruction.
- Full-HD 10-bit SCANOUT|RENDERING allocation/copy/readback passed. This requests
  scanout usage but does NOT prove kernel framebuffer/page-flip acceptance.
- Reverse actual MultiRenderer: both formats and full HD, 54 measured writes to
  three original native targets, exact sparse damage, shared-context readers,
  capture, target-GLES fallback, blit_to/from, resize and cache invalidation.
  Per-draw counters proved direct Vulkan rather than CPU/intermediate fallback.
- Ordinary reverse shared-GLES pixel fallback passed, as did the existing forward
  NVIDIA->Intel direct-target regression tests in both formats.

The library exposes VulkanCopyDevice::Render (default) / Target, generation-scoped
queries, role-aware source/destination capabilities and bounded caches/retry policy.
Target+direct explicitly prefers Vulkan ahead of a valid shared-texture route,
retaining that route as fallback and preserving its target-reader release fence.

Niri selects the new policy only with `NIRI_VK_COPY_DEVICE=target`. Foreign atomic
outputs start with ordinary renderable formats; after the existing background
engine reports exact destination capabilities, target EGL/KMS intersections select
a native swapchain. Decisions are scoped by epoch, format and extent. set_format is
transactional and old KMS frame leases are never released manually. Saved original
modifiers and one-shot redraw recovery cover ordinary rendering/queue failure;
failed restoration retains its fallback for later active/stable redraw. Delayed
recovery checks output existence to avoid unplug races. Device loss still requires
normal invalidation, not unsafe fallback reuse.

## Isolated test configuration

`/home/acters/.config/niri/config-intel-bridge-test.kdl` is a separate copy selecting
Intel renderD128 while retaining relative includes. Original config.kdl remains
NVIDIA-primary. No running renderer configuration has changed yet.

The intended test combines that config with `NIRI_VK_COPY_DEVICE=target`,
`NIRI_VK_DIRECT_TARGET=1`, `NIRI_VKBRIDGE=1`, quiet info logging and opt-in timing.
Native Intel outputs retain normal allocation policy. Client direct-scanout
feedback remains conservative and unchanged; this is not a claim of importing
NVIDIA-native client surfaces on Intel.

## Status

Smithay reverse-route checkpoint: `61dafe41`.

Offscreen validation and source reviews passed. The user approved an Intel-primary
session and confirmed all three outputs render correctly, including moved windows.
NVIDIA native AB30 modifier negotiation and real direct-copy page flips succeeded
on DP-1 and HDMI-A-1 with no observed render/queue errors. eDP-1 stayed native Intel.

## Single-copy live checkpoint

Graphics checkpoint niri `d94814f5` / Smithay `61dafe41`; counter-only follow-up
niri `ee5b527d` / Smithay `8edc1da0` (same graphics path), running as
`/home/acters/.local/bin/niri-intel-bridge-counted` with the isolated Intel config.
NVIDIA's repeated DRM sequence values exposed a diagnostic dedup assumption; the
follow-up counts distinct (sequence,timestamp) pairs and flags sequence-gap counts
unavailable when sequence does not advance. It does not modify rendering/scheduling.

Balanced-profile TestUFO measurements on DP-1 (239.760 Hz), monitors closed:

| Route | Stable duration | KMS presentations/sec | Late presentations |
| --- | ---: | ---: | ---: |
| Direct NVIDIA Vulkan into native target | 95.02 s | 221.647 | 1721 |
| Shared Intel buffer sampled by NVIDIA GLES | 195.02 s | 133.954 | 20634 |

Direct route counters matched all 21,060 submitted frames, with native input imports
twice per copy and no steady resource-set creation/destruction. Host Vulkan setup
averaged 0.1773 ms, resource checkout 0.0098 ms, and queue return led target time by
2.48 ms on average. Most late presentations were not CPU queue-start lateness;
these CPU clocks cannot separate GPU render, dependency, transfer or client costs.
The shared control had TextureCopies=26,124 and no Vulkan/CPU copies. Native buffer
policy, renderer, placement and power profile stayed fixed between these trials.

Native eDP-1 measured 143.994/s with no gaps or late presentations over 110s and no
bridge work. All 15,840 frames used CLIENT direct scanout: this confirms the native
Intel path but is not a heavy compositor-rendering benchmark.

Raw archive: `/home/acters/.local/state/niri-pacing/frame-timing-reverse-single-copy-3389231.jsonl`.
Relevant PID3389231 epochs4 (DP direct),9 (DP shared),13 (eDP native). Earlier
PID3310086 NVIDIA presentation counts are invalid because of the pre-fix dedup;
its route/error counters and visual test remain separate evidence.

Direct Vulkan substantially improves this reverse comparison but does not yet
sustain stable 240 Hz. The user requested further tuning while initially leaving Intel-primary active.
The subsequent tiled-source/two-leg experiment was correct but slower in live tests;
see [intel-tiled-reverse-plan.md](intel-tiled-reverse-plan.md). Matched quiet
single-copy power-profile tests measured239.007/s on performance versus187.625/s on
balanced, with intel_pstate as the profile driver. This does not isolate a GPU clock
mechanism or establish a battery/idle-power benefit.

The user ultimately chose to restore the NVIDIA-primary pooled build. That default
is active again with direct target enabled and recording off; performance profile
was left as selected. Reverse implementations/configs and all measurements remain
available for explicit future testing, not automatic renderer switching.
