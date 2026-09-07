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

Ready for a coordinated experimental compositor session. No experimental binary installed. Initialization-order and Vulkan-to-KMS experiments remain separate later phases. Niri's experimental Cargo patches intentionally point to the companion local checkout; no experimental commits have been pushed.
