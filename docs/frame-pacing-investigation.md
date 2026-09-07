# eDP-1 frame pacing and monitoring overhead

## Build and configuration

- Running niri code: `9ee3ff77`; Smithay code: `0e2ad6d1`.
- Optimized release build, direct-target transfer enabled.
- Outputs: eDP-1 143.998 Hz, DP-1 239.760 Hz, HDMI-A-1 75.001 Hz; VRR off.
- Initial service logging: `niri=debug,smithay::backend::renderer::multigpu=debug,smithay::backend::renderer::gles=error`.
- With explicit user approval, only `RUST_LOG` was changed to `info` in
  `/home/acters/.config/systemd/user/niri.service.d/override.conf`, and the same
  binary was restarted. No rendering code, refresh rate, or transfer policy changed.

## Observations

The user reports stable 240 FPS on the native NVIDIA output, but 140–142 FPS on
the 144 Hz Intel-connected panel when btop/nvtop are running. Quiet logging helped
somewhat. Closing monitoring tools helped; hiding btop on an inactive workspace
while it kept running did not eliminate the drop. Repeated `nvidia-smi` invocations
and persistent `nvidia-smi dmon` also reproduce a drop according to the user.

Before the logging-only restart, a journal sample contained 6,483 direct-transfer
submissions across 45.509169 seconds: 142.433 submissions/sec. Median spacing was
6.9211 ms, p95 7.5052 ms, p99 11.431 ms, maximum 21.6649 ms. There were 98 intervals
above 10 ms and 44 above 13 ms; no sampled render/queue/device-loss errors. These
are tracing timestamps for CPU-side submissions, NOT KMS presentation measurements.
The gaps did not show strong phase concentration at the configured 700 ms btop
interval in a simple circular-phase check.

A ten-second pidstat sample before restart showed about 43.3% of one CPU core on
the main compositor thread (21.4% user, 21.9% system), about 912 voluntary and 12.4
involuntary context switches/sec. This does not locate a stall or prove saturation.

Btop configuration inspected: update interval 700 ms, GPU info Auto, NVIDIA and
Intel included. PCIe measurement was initially enabled. The user disabled both
`nvml_measure_pcie_speeds` and `rsmi_measure_pcie_speeds`; this did not fix the drop.
Those settings were changed by the user, not overwritten by this investigation.

## Controlled headless polling tests

Other monitoring tools were to be closed, TestUFO visible on eDP-1, and commands
wrote to files rather than a terminal window. Results below are user visual
observations, not instrumented presentation counters.

| Test | Window | Result |
| --- | --- | --- |
| `nvidia-smi --query-gpu=timestamp,index,utilization.gpu,utilization.memory,memory.used,power.draw,temperature.gpu --loop-ms=700` | 20 seconds | No reproduced drop |
| `nvidia-smi dmon -s c -d 1 -c 15` | 15 samples | Inconclusive; user requested longer runs |
| `nvidia-smi dmon -s c -d 1 -c 60` | 60 samples | Stable around 144 FPS |
| `nvidia-smi dmon -s u -d 1 -c 60` | 60 samples | Repeated occasional 1–2 FPS drops/recovery; changing to performance profile removed the effect |
| `nvidia-smi dmon -s p -d 1 -c 60` | 60 samples, balanced verified with powerprofilesctl | Stable around 144 FPS |

The utilization trial changed power profile, so it is not a fixed-profile causal
measurement. The user explicitly characterized it as inconclusive for the whole
monitoring-tool problem. The earlier basic-query test was shorter and did not query
the same full engine-utilization set as dmon. Default dmon on this installation
selects `puc`: power/temperature, utilization, and clocks, NOT PCIe throughput.

## Assessment

Evidence favors monitoring/query-related interference and reduced deadline headroom
on the bridged path. A 144 Hz frame budget is about 6.94 ms; occasional delays can
miss refresh opportunities even without a rendering error. Hiding the monitoring
window and reproducing with CLI monitoring make visible-window composition an
insufficient explanation. Basic telemetry did not reproduce the issue, so it is
incorrect to claim that every NVML query causes it.

Driver/firmware serialization, process/engine queries, and CPU scheduling are still
hypotheses. A genuine bridge scheduling bug has NOT been established. The 240 Hz
native route and hardware video decoding are different pipelines and do not rule
out stalls in the cross-GPU submission/synchronization path.

Historical precedent exists across Sway/KDE, but is not proof of the current driver
issue: https://forums.developer.nvidia.com/t/regression-nvidia-545-23-06-frame-stutters-when-gpu-stats-are-queried/270572
That thread's reporter later marked their issue fixed in 580.65.06; this machine
uses a newer driver and must be diagnosed from its own measurements.

## Next useful measurements; no speculative scheduler rewrite

Keep a fixed power profile, browser/window placement and monitoring interval.
Add opt-in aggregate timing/counters (not per-frame logs) for:

- Source preparation/render and target acquire/publication CPU duration.
- Vulkan submit/object-retirement duration and cache misses.
- CPU fallback waits when native-fence import/export fails.
- Actual KMS sequence gaps and presentation time versus predicted deadline.
- Browser/client commit arrival versus compositor submission, where attributable.

Static review identifies avoidable costs to profile: two no-draw Intel GLES frames
per direct transfer, per-copy Vulkan pool/fence/semaphore creation/destruction,
redundant completed-fence waits, and CPU-copy damage merging performed before a
direct transfer succeeds. Required fence ordering must not be removed as an
optimization. Ordinary last-Arc destruction is NOT proven to wait unfinished GPU
work: the engine retains pending batches until their fences are reached.

For clean FPS comparisons, stop GPU monitoring tools or use a slower sampling
interval. Performance mode is an observed mitigation for the utilization-only
trial, with the expected power/heat tradeoff; it is not proof of a root-cause fix.
No compositor source or driver settings were changed. The user changed power profiles
during the comparisons; no power profile was forced by the assistant. All test
polling processes have exited. Quieter service logging remains active.
