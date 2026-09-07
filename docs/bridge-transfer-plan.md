# Bridge transfer-engine integration

## Baseline and safety

- Working niri fork: `de0a09fa`; Smithay fork: `7e2c7be6`.
- Experimental branch in both repositories: `nvidia-intel-bridge-transfer-engine`.
- Companion Smithay checkout: `../smithay-nvidia-intel-bridge`.
- Installed `/home/acters/.local/bin/niri` and the active user service remain unchanged during development.
- Coordinate with the user before restarting the graphical session or running disruptive DRM tests.

## Requested order

1. Replace previous-frame bridge presentation with a well-owned same-frame transfer engine.
2. Investigate the early-instance/lazy-device workaround separately, with controlled initialization tests.
3. Test direct Vulkan-to-KMS scanout feasibility; implement only if those tests support it.

## Phase 1 contract

The current source frame's fence feeds a Vulkan transfer. Its completion fence feeds the current target GLES frame, whose finish fence is returned to the normal Smithay caller. No previous-frame selection, fake signaled success for pending work, manager-global presentation sequence, or niri completion redraw channel.

Ownership:

- Device-matched transfer engine owns Vulkan state, imported-image caches and in-flight submission retirement.
- Device-pair transfer storage owns intermediate destination buffers and their target-reader release dependencies.
- Source storage is not rewritten until previous Vulkan reads complete.
- Destination storage is not rewritten until previous target GLES reads complete.
- Imports use stable DMA-BUF identities and retain allocations while Vulkan references them.
- Invalidation retires affected resources safely and clears transfer storage; old results cannot survive generation changes.
- Direct import stays first choice; optional Vulkan copy is followed by CPU fallback or a real error, never false success.

Start with the existing initialization entry point intact. Correct API/device selection and lifetime issues necessary for the transfer engine may be repaired, but initialization-order experiments are phase 2.

## Validation gates

- Build niri workspace against the companion experimental Smithay source.
- Test feature combinations with Vulkan transfer disabled and enabled.
- Unit-test transport state, region validation and buffer reuse/failure behavior where possible.
- Run existing niri/config/IPC tests and formatting/diff checks.
- Use standalone, non-master render-node probes for GPU-copy validation before any compositor restart.
- Validate current-frame pixels, producer and consumer fence dependencies, invalidation/resize, repeated allocation/FD reuse, and failure paths.
- Runtime compositor validation requires coordinated restart and an available rollback to the working installed binary.

## Phase 1 implementation and findings

- Replaced manager-global previous-frame selection with same-frame Vulkan submission and target GLES fence chaining.
- Added pair-local destination storage, source-copy/target-reader release fences, and source-manager generation identities for cross-manager invalidation.
- Removed niri's completion-redraw channel; `NIRI_VKBRIDGE=0` now disables transfers as well as preinit.
- Added Vulkan RAII batch retirement using an independent VkFence, stable DMA-BUF import identities, explicit modifier imports, and foreign ownership acquire/release.
- Independent review caught and corrected device-loss fence retry and retained-input-fence history chains. Known device loss requires invalidation instead of partial CPU reuse of undefined external memory.
- Existing niri/config/IPC library tests: 216 passed, including a rerun after modifier negotiation. Smithay without Vulkan: build passed. Eight new transfer tests passed after correcting a test fixture that constructed a negative Size before validation.
- A render-node-only pixel probe is available in companion Smithay `examples/vulkan_transfer.rs`.

### Hardware findings (offscreen, no DRM master)

- The probe's initial GBM LINEAR usage flag produced an implicit descriptor on the legacy modifier allocation API. Corrected the probe to use the same explicit `[Modifier::Linear]` allocation path as production.
- Unconstrained NVIDIA GBM selected modifier `0x0300000000e08014`, which Vulkan does not advertise for transfer import on this stack.
- Selecting the EGL/Vulkan-compatible source modifier `216172782120099860` validated ABGR8888 and ABGR2101010: 24 frames each, two allocation sizes, partial updates, all-pixel target-GLES readback. Source and copy fences were native/exportable; copies were still pending at return.
- Source allocation now negotiates the EGL-render/Vulkan-import modifier intersection. No hard-coded modifier or guessed OPTIMAL-layout import is used. The default negotiated ABGR2101010 probe passes without a modifier override.
- `vulkaninfo --summary`: NVIDIA 610.57.04, Intel Mesa 26.2.2. Khronos validation layers are not installed; these are pixel/interoperability checks, not validation-layer runs.
- These small offscreen probes establish pixel/synchronization interoperability, not compositor frame-rate performance or direct scanout behavior.

## Status

Phase 1 implementation passes workspace checks, 216 niri/config/IPC tests, eight transfer tests, no-Vulkan feature isolation and strict transfer Clippy. Direct engine probes pass 8/10-bit midtone pixels and a full-HD ten-bit case. Actual MultiRenderer probes pass 72 measured frames per format (8/10-bit), alternating two outputs with same/mixed sizes and two cache invalidations; logs confirm the Vulkan path is used.

Smithay phase 1 checkpoint: `b4ee98e8`.

Niri phase 1 checkpoint: `1a794dca`. After explicit user approval, the release binary was installed alongside the baseline as `/home/acters/.local/bin/niri-transfer-engine` and selected by `/home/acters/.config/systemd/user/niri.service.d/95-transfer-engine-test.conf`. The original `/home/acters/.local/bin/niri` (`de0a09fa`) is unchanged.

The approved restart succeeded on 2026-09-07 at 15:56 UTC. All three outputs are active (eDP-1 ~144 Hz, DP-1 ~240 Hz, HDMI-A-1 ~75 Hz). The new process reports `1a794dca`; its journal confirms same-frame Vulkan submissions from renderD129 to renderD128 with native fences. User visual feedback is still needed before treating the session as fully validated.

Rollback: remove only `95-transfer-engine-test.conf`, run `systemctl --user daemon-reload`, then restart `niri.service` at a coordinated time. This restores the existing `90-local-build.conf` and baseline binary.

Niri's experimental Cargo patches intentionally point to the companion local checkout; no experimental commits have been pushed.

## Phase 2: initialization ordering

The user confirmed phase one looks correct and approved up to three controlled initialization-test restarts, with phase one retained for rollback.

Test 1 used niri `22bcd968` / Smithay `d66ad5ed` with transfers enabled and `NIRI_VKBRIDGE_PREINIT=0`. It ran as the real compositor (invocation `71ec47fe3bae45329b9bf4a8e445b25c`). At 16:09:00.443 UTC displays/IPC were ready; at 16:09:00.476 Vulkan initialization began with `preinitialized=false`. Instance creation completed in 318 ms and the logical device was ready after 417 ms total. Same-frame native-fence transfers followed. No late-initialization hang reproduced on NVIDIA 610.57.04.

During instance creation niri logged an X11 abstract-socket connection and spawned xwayland-satellite. This suggests loader/layer display interaction may matter; it does not establish the historical deadlock's root cause. Keep background initialization, and do not infer synchronous compositor-main-thread initialization is safe from this test.

The follow-up replaces custom Preinit/global handoff with Smithay Instance/PhysicalDevice wrappers and removes niri's early startup hook (`src/main.rs` now matches upstream). Smithay checkpoint: `319551f8`. Nine transfer tests, strict transfer Clippy, niri workspace check, direct 8/10-bit pixel probes, and the 72-frame ten-bit MultiRenderer/invalidation probe pass. The wrapper-based real-session test passed as restart 2 of the approved maximum 3.

Test 2 ran niri `8d3bd6c8` / Smithay `319551f8`, invocation `d18041addc314948a6d1aba02cc2d43f`. Display/IPC startup completed at 16:25:42.944 UTC; lazy instance initialization began at 16:25:43.111. Instance creation took 198 ms and the logical device was ready after 275 ms. Native-fence transfers activated at 16:25:45.111. The user confirmed it looks correct and authorized proceeding to non-disruptive KMS feasibility checks. The 216 niri/config/IPC tests were rerun and passed for this version.

Current service uses `/home/acters/.local/bin/niri-lazy-transfer` via `96-initialization-test.conf`; removing that drop-in restores phase one. The intermediate raw-late `niri-init-test` and original binaries are preserved. No further initialization restart is needed; do not treat the unused restart allowance as permission for display takeover/KMS tests.

## Phase 3: direct transfer to scanout feasibility

Gate 1 passed: render-node-only SCANOUT|RENDERING LINEAR allocation/copy/readback tests for ABGR8888 and ABGR2101010, 24 frames each at 1920x1080/1952x1104 with partial damage and midtones. The example's `--scanout-candidate` requires the modifiers2 feature so GBM receives the requested usage flags and explicit modifier together. These tests cannot establish KMS admissibility or fence acceptance. Atomic TEST_ONLY and actual-display tests need separately coordinated KMS authority; no display takeover has been authorized yet. An opt-in niri diagnostic (`NIRI_VK_KMS_TEST_ONLY_OUTPUT`) is prepared: a delayed background engine creates private candidates, and the compositor uses its existing atomic DRM surface to test them with `test_state(..., false)` only. It refuses legacy/inactive/pending-mode cases, never commits or page-flips a candidate, and tests both a completed copy without a fence and a second copy with a native fence. The diagnostic was explicitly approved and ran once as niri `540230c7`, invocation `604979ed4c7b440b8a1f894bc8a906ef`. Gate 2 passed at 17:15:52 UTC for eDP-1 1920x1080: ABGR8888 exported as opaque XBGR8888 and ABGR2101010 as opaque XBGR2101010, both explicit LINEAR. For each format atomic TEST_ONLY accepted a completed copy without a fence and a second native copy fence with `complete_at_test=false`. No candidate was displayed. The opt-in was removed from the service drop-in after completion so subsequent restarts do not repeat the diagnostic. Log: `/tmp/niri-kms-test-only.log`. Implement an opt-in direct-to-bound-target path only after the feasibility gates succeed, preserving DrmCompositor slot ownership and target GLES ordering.
