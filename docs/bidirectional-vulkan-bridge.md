# Bidirectional pooled Vulkan bridge

`bidirectional-vulkan-bridge` is the shared branch in **Acters/niri** and
**Acters/smithay**. It contains both validated single-copy paths in one implementation.
The reverse work already descends from the pooled forward work; the slower
experimental two-stage detile route is deliberately excluded.

Niri pins Smithay and smithay-drm-extras to the published fork revision
`8edc1da00e599358c2c9cafe38db47c6aff2037f`. Both packages use the same revision.
A sibling Smithay checkout is not required for a normal build.

## Configure in config.kdl

These are startup settings. Changing the primary GPU or bridge policy requires a
coordinated compositor restart. Bridge edits during config reload log a restart
notice; they do not silently change active GPU allocation/fence policy.

### NVIDIA renderer → Intel scanout

Merge into the existing config (do not add a second `debug` block):

```kdl
debug {
    render-drm-device "/dev/dri/renderD129"
}
vulkan-bridge {
    enabled true
    direct-target true
    copy-device "render"
}
```

NVIDIA GLES composes the scene. Pooled NVIDIA Vulkan copies exact damage into
Intel-owned LINEAR scanout buffers. NVIDIA-connected outputs remain native.
This is the currently retained configuration on the tested laptop.

### Intel renderer → NVIDIA scanout

```kdl
debug {
    render-drm-device "/dev/dri/renderD128"
}
vulkan-bridge {
    enabled true
    direct-target true
    copy-device "target"
}
```

Intel GLES composes into Intel-owned LINEAR staging. Pooled NVIDIA Vulkan imports
that source and copies exact damage into NVIDIA-owned native scanout buffers.
Intel-connected outputs remain native. Valid shared-buffer/GLES copying remains a
fallback; actual route counters distinguish it from direct Vulkan.

**The node numbers describe the tested laptop, not every machine.** Identify local
render nodes before using either example. This is one binary with two configurations,
not automatic GPU migration.

### Defaults and precedence

When `vulkan-bridge` is present, **the entire bridge policy comes from KDL**; legacy
bridge environment variables are ignored, even for omitted fields. Defaults are:

| Setting | KDL default |
| --- | --- |
| `enabled` | `true` |
| `direct-target` | `true` |
| `copy-device` | `"render"` |
| `timing.enabled` | `false` |
| `timing.output` | unset |

`enabled false` also disables direct writes. Blocks merge by field across includes
using normal positional include semantics. Invalid booleans, copy-device choices
and unknown children are rejected. Timing is off unless explicitly enabled.

If the whole block is absent, the legacy environment contract remains available
for existing installations: NIRI_VKBRIDGE, NIRI_VK_DIRECT_TARGET,
NIRI_VK_COPY_DEVICE, SMITHAY_FRAME_TIMING and NIRI_FRAME_TIMING_FILE. With neither
KDL nor environment settings, legacy defaults remain transfer enabled, direct target
off, render-side copy and timing off. New configurations should use KDL instead.

### Optional timing

```kdl
vulkan-bridge {
    copy-device "target"
    timing {
        enabled true
        output "/run/user/1000/niri-frame-timing.jsonl"
    }
}
```

Use your own absolute path in an existing private directory; `1000` is only an
example UID. No shell-variable or tilde expansion is performed. Without an output
path, requested recording is disabled with a warning rather than collecting data
that cannot be reported. Output is append-only. Normal use should keep timing off;
per-frame debug logging is not required. General application logging is separate
from the bridge settings.

## Build

With normal niri build dependencies installed:

```sh
cargo build --release --locked -p niri --bin niri
```

For local Smithay development, explicitly override both Smithay packages with local
Cargo patches. The committed manifest/lockfile use the reproducible GitHub pin,
not whichever sibling branch happens to be checked out.

## Validation and limitations

Included: bounded resource reuse, stable old SyncPoints, same-frame synchronization,
exact damage, copy-role capability negotiation, native target recovery and aggregate
diagnostics. Both directions passed pixel, fence, capture and blit tests.

Intel-primary was faster than shared-GLES sampling but did not consistently sustain
DP-1's 239.760 Hz under balanced power policy. A matched quiet performance-profile
trial reached 239.007 presentations/sec, not perfect pacing. No automatic power
profile or clock changes are made by this configuration.

The rejected detile experiment and results remain on `intel-nvidia-bridge-tiled`.
Historical measurements are in `frame-timing-results.md`, `vulkan-submission-pool.md`
and `intel-nvidia-bridge.md`.

Creating/building this branch does not replace the installed compositor or edit the
user's config/service. The older installed binary does not understand the new KDL
block; migrate configuration only when adopting the unified build.
