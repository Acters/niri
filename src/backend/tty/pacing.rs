//! Opt-in pacing measurements. Graphics paths only record clocks/counters into bounded TLS
//! histograms. Serialization and file I/O run on a separate thread, once per five-second window.
//! Timings are CPU wall time (including stalls), not GPU execution time.

use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use smithay::backend::drm::DrmNode;
use smithay::backend::renderer::multigpu::timing::{self, Counter, Stage, StreamSnapshot};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::drm::control::crtc;

use crate::niri::State;
use crate::utils::get_monotonic_time;

const WINDOW: Duration = Duration::from_secs(5);
static EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
pub(super) struct QueueStamp {
    at: Duration,
    epoch: u64,
}

pub(super) fn queue_stamp() -> Option<QueueStamp> {
    now().map(|at| QueueStamp {
        at,
        epoch: EPOCH.load(Ordering::Relaxed),
    })
}

pub(super) fn id(node: DrmNode, crtc: crtc::Handle) -> timing::StreamId {
    (node.dev_id(), crtc.into())
}

pub(super) fn now() -> Option<Duration> {
    timing::enabled().then(get_monotonic_time)
}

#[derive(Default)]
pub(super) struct SurfaceTiming {
    last_render: Option<Duration>,
    last_event: Option<(u32, Duration, Duration)>,
    last_counted: Option<(u32, Duration)>,
}

impl SurfaceTiming {
    pub fn render_start(&mut self, target: Duration) {
        let Some(now) = now() else {
            return;
        };
        if let Some(previous) = self.last_render.replace(now) {
            timing::observe(Stage::FrameInterval, now.saturating_sub(previous));
        }
        timing::observe(Stage::RenderLead, target.saturating_sub(now));
    }

    /// Observe the hardware timestamp BEFORE upstream's optional event throttling.
    /// Sequence gaps also occur when a client is idle; they are not automatically dropped frames.
    pub fn event(&mut self, sequence: u32, presentation: Duration, callback: Duration) {
        if !timing::enabled() || presentation.is_zero() {
            return;
        }
        if self
            .last_event
            .is_some_and(|(previous, _, _)| previous == sequence)
        {
            return;
        }
        // Attribute only after frame_submitted supplies this event's queue epoch.
        self.last_event = Some((sequence, presentation, callback));
    }

    /// Attach the queued frame's intended deadline to its actual hardware presentation.
    /// A throttled synthetic callback can reuse the timestamp saved for that SAME sequence.
    pub fn presented(
        &mut self,
        sequence: u32,
        target: Duration,
        queued: Option<QueueStamp>,
        refresh: Option<Duration>,
    ) {
        if !timing::enabled() {
            return;
        }
        let Some(queued) = queued.filter(|queued| queued.epoch == EPOCH.load(Ordering::Relaxed))
        else {
            timing::count(Counter::TransitionPresentationsIgnored, 1);
            return;
        };
        let Some((actual_sequence, actual, callback)) = self.last_event else {
            timing::count(Counter::UnknownClock, 1);
            return;
        };
        if actual_sequence != sequence {
            timing::count(Counter::UnknownClock, 1);
            return;
        }
        if let Some((previous_sequence, previous)) = self.last_counted {
            if previous_sequence == sequence {
                return;
            }
            let distance = sequence.wrapping_sub(previous_sequence);
            if distance < (1 << 31) {
                timing::count(Counter::SequenceGaps, u64::from(distance.saturating_sub(1)));
            }
            timing::observe(Stage::PresentationInterval, actual.saturating_sub(previous));
        }
        self.last_counted = Some((sequence, actual));
        timing::count(Counter::PresentEvents, 1);
        if actual > callback {
            timing::count(Counter::FuturePresentationTimestamp, 1);
        }
        timing::observe(Stage::PresentCallbackDelay, callback.saturating_sub(actual));
        timing::observe(Stage::PresentLateness, actual.saturating_sub(target));
        timing::observe(Stage::PresentEarliness, target.saturating_sub(actual));
        if refresh.is_some_and(|period| actual.saturating_sub(target) >= period / 2) {
            timing::count(Counter::PresentLate, 1);
        }
        timing::observe(Stage::QueueToPresentation, actual.saturating_sub(queued.at));
    }
}

pub(super) fn queued(target: Duration, started: Option<QueueStamp>) {
    if let Some(QueueStamp { at: started, .. }) = started {
        timing::observe(Stage::QueueLead, target.saturating_sub(started));
        timing::observe(Stage::QueueLate, started.saturating_sub(target));
        if started > target {
            timing::count(Counter::QueuePastDeadline, 1);
        }
        if let Some(returned) = now() {
            timing::observe(Stage::QueueReturnLead, target.saturating_sub(returned));
            timing::observe(Stage::QueueReturnLate, returned.saturating_sub(target));
            if returned > target {
                timing::count(Counter::QueueReturnedPastDeadline, 1);
            }
        }
    }
}

struct Window {
    epoch: u64,
    timestamp_ms: u128,
    elapsed_ns: u128,
    dropped_windows: u64,
    phase: String,
    direct_target: bool,
    recording: bool,
    streams: Vec<StreamSnapshot>,
}

enum Report {
    Window(Window),
    Control(serde_json::Value),
}

struct ReportState {
    writer_ready: Arc<AtomicBool>,
    previous: Instant,
    dropped: u64,
    phase: String,
    direct_target: bool,
}

struct ControlSocket {
    socket: UnixDatagram,
    path: PathBuf,
}
impl AsFd for ControlSocket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
}
impl Drop for ControlSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// No timer, thread or file is created unless SMITHAY_FRAME_TIMING=1. A supplied file path
/// is required to avoid unexpectedly writing diagnostics elsewhere. The file is append-only.
pub(super) fn register_reporter(
    event_loop: &LoopHandle<'static, State>,
    direct_target: bool,
    render_node: DrmNode,
    copy_device: smithay::backend::renderer::multigpu::VulkanCopyDevice,
) {
    if !timing::enabled() {
        return;
    }
    let Some(path) = std::env::var_os("NIRI_FRAME_TIMING_FILE").map(PathBuf::from) else {
        warn!("SMITHAY_FRAME_TIMING enabled without NIRI_FRAME_TIMING_FILE; no reports will be written");
        return;
    };
    let (sender, receiver) = sync_channel::<Report>(2);
    let writer_ready = Arc::new(AtomicBool::new(false));
    let worker_ready = writer_ready.clone();
    let header = serde_json::json!({
        "type": "session", "pid": std::process::id(), "version":crate::utils::version(),
        "started_ms": epoch_ms(), "direct_target": direct_target,
        "render_device": render_node.dev_id(), "copy_device_role": format!("{copy_device:?}"),
        "executable": std::env::current_exe().ok(),
        "window_ms": WINDOW.as_millis(),
        "clock": "CPU wall / CLOCK_MONOTONIC presentation",
        "notes": "nested durations overlap; histogram quantiles are upper bounds; sequence gaps may be idle"
    });
    let worker = std::thread::Builder::new().name("niri-pacing-log".into()).spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            let file = OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?;
            let mut file = BufWriter::new(file);
            serde_json::to_writer(&mut file, &header)?;
            writeln!(file)?;
            file.flush()?;
            worker_ready.store(true, Ordering::Release);
            while let Ok(report) = receiver.recv() {
                let window = match report {
                    Report::Window(window) => window,
                    Report::Control(control) => {
                        serde_json::to_writer(&mut file, &control)?;
                        writeln!(file)?;
                        file.flush()?;
                        continue;
                    }
                };
                for stream in window.streams {
                    let metrics: serde_json::Map<String, serde_json::Value> = stream.metrics.into_iter().map(|(stage, histogram)| {
                        let s = histogram.summary();
                        (stage.as_str().to_owned(), serde_json::json!({
                            "count":s.count, "total_ns":s.total_ns, "min_ns":s.min_ns, "max_ns":s.max_ns,
                            "p01_ns":s.p01_ns, "p05_ns":s.p05_ns,
                            "p50_ns":s.p50_ns, "p95_ns":s.p95_ns, "p99_ns":s.p99_ns
                        }))
                    }).collect();
                    let counters: serde_json::Map<String, serde_json::Value> = stream.counters.into_iter().map(|(counter, count)| {
                        (counter.as_str().to_owned(), count.into())
                    }).collect();
                    let row = serde_json::json!({
                        "type":"window", "pid":std::process::id(), "epoch":window.epoch, "timestamp_ms":window.timestamp_ms,
                        "elapsed_ns":window.elapsed_ns, "dropped_windows":window.dropped_windows,
                        "phase":window.phase, "direct_target":window.direct_target, "recording":window.recording,
                        "output":stream.label, "device":stream.id.0, "crtc":stream.id.1,
                        "metrics":metrics, "counters":counters
                    });
                    serde_json::to_writer(&mut file, &row)?;
                    writeln!(file)?;
                }
                file.flush()?;
            }
            Ok(())
        })();
        worker_ready.store(false, Ordering::Release);
        if let Err(err) = result { warn!("frame timing writer stopped: {err:#}"); }
    });
    if let Err(err) = worker {
        warn!("could not start frame timing writer: {err}");
        return;
    }
    let reporting = Rc::new(RefCell::new(ReportState {
        writer_ready,
        previous: Instant::now(),
        dropped: 0,
        phase: "startup".into(),
        direct_target,
    }));
    register_control(event_loop, sender.clone(), reporting.clone(), direct_target);
    if let Err(err) = event_loop.insert_source(Timer::from_duration(WINDOW), move |_, _, _| {
        let end = Instant::now();
        let mut reporting = reporting.borrow_mut();
        let window = Window {
            epoch: EPOCH.load(Ordering::Relaxed),
            timestamp_ms: epoch_ms(),
            elapsed_ns: end.duration_since(reporting.previous).as_nanos(),
            dropped_windows: reporting.dropped,
            phase: reporting.phase.clone(),
            direct_target: reporting.direct_target,
            recording: timing::enabled(),
            streams: timing::drain(),
        };
        reporting.previous = end;
        match sender.try_send(Report::Window(window)) {
            Ok(()) => reporting.dropped = 0,
            Err(TrySendError::Full(_)) => reporting.dropped = reporting.dropped.saturating_add(1),
            Err(TrySendError::Disconnected(_)) => return TimeoutAction::Drop,
        }
        TimeoutAction::ToDuration(WINDOW)
    }) {
        warn!("could not start frame timing reporter: {err}");
    }
}

/// Private diagnostic controls are event driven, not polled per frame. Mode changes run
/// between frames and retain the startup swapchain formats, isolating the transfer path.
fn register_control(
    event_loop: &LoopHandle<'static, State>,
    sender: SyncSender<Report>,
    reporting: Rc<RefCell<ReportState>>,
    startup_direct: bool,
) {
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        warn!("no XDG_RUNTIME_DIR; frame timing control socket disabled");
        return;
    };
    let runtime = PathBuf::from(runtime);
    let private_runtime = std::fs::symlink_metadata(&runtime).is_ok_and(|meta| {
        meta.is_dir()
            && meta.uid() == smithay::reexports::rustix::process::geteuid().as_raw()
            && meta.permissions().mode() & 0o077 == 0
    });
    if !private_runtime {
        warn!("pacing control requires an owned, private XDG_RUNTIME_DIR");
        return;
    }
    let path = runtime.join(format!("niri-pacing-{}.sock", std::process::id()));
    let socket = match UnixDatagram::bind(&path) {
        Ok(socket) => socket,
        Err(err) => {
            warn!("could not bind pacing control socket: {err}");
            return;
        }
    };
    let control = ControlSocket { socket, path };
    if let Err(err) =
        std::fs::set_permissions(&control.path, std::fs::Permissions::from_mode(0o600))
            .and_then(|_| control.socket.set_nonblocking(true))
    {
        warn!("could not secure pacing control socket: {err}");
        return;
    }
    info!(path = %control.path.display(), "frame timing controls ready (status, mark LABEL, record on/off, direct on/off)");
    let source = Generic::new(control, Interest::READ, Mode::Level);
    if let Err(err) = event_loop.insert_source(source, move |_, control, state| {
        // Bound command handling even if a local sender floods the diagnostics socket.
        for _ in 0..8 {
            let mut buf = [0u8; 128];
            let (len, peer) = match control.socket.recv_from(&mut buf) {
                Ok(result) => result,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => { warn!("pacing control read failed: {err}"); return Ok(PostAction::Remove); }
            };
            let mut report = reporting.borrow_mut();
            let decoded = std::str::from_utf8(&buf[..len]);
            if len == buf.len() || decoded.is_err() {
                reply(&control.socket, &peer, &control_status("invalid", Some("invalid or oversized command"), &report));
                continue;
            }
            let command = decoded.unwrap().trim();
            let mut changed = true;
            let error = if command == "status" {
                changed = false;
                None
            } else if let Some(label) = command.strip_prefix("mark ") {
                if label.is_empty() || label.len() > 80 || !label.bytes().all(|c| c.is_ascii_alphanumeric() || b"_- .".contains(&c)) {
                    Some("invalid phase label")
                } else {
                    report.phase = label.to_owned();
                    None
                }
            } else if command == "record on" || command == "record off" {
                timing::set_enabled(command == "record on");
                None
            } else if command == "direct on" || command == "direct off" {
                if !startup_direct {
                    Some("restart with NIRI_VK_DIRECT_TARGET=1 to enable transfer-policy comparisons")
                } else {
                    let enabled = command == "direct on";
                    state.backend.tty().gpu_manager.set_vulkan_direct_target_enabled(enabled);
                    report.direct_target = enabled;
                    // Allocation policy remains fixed; only the transfer route changes.
                    state.niri.queue_redraw_all();
                    None
                }
            } else { Some("unrecognized command") };
            if error.is_none() && changed {
                EPOCH.fetch_add(1, Ordering::Relaxed);
                // Old CPU windows are discarded. Old in-flight presentations are rejected
                // later by their queue epoch, rather than relabeled as this trial.
                drop(timing::drain());
                for (&node, device) in &mut state.backend.tty().devices {
                    for (&crtc, surface) in &mut device.surfaces {
                        surface.pacing = SurfaceTiming::default();
                        timing::register_stream(id(node, crtc), &surface.name.connector);
                    }
                }
                report.previous = Instant::now();
            }
            let message = control_status(command, error, &report);
            // Confirmation is independent of the bounded disk-writer channel. A client
            // can query status if a datagram reply is lost; it must not retry mutations blindly.
            reply(&control.socket, &peer, &message);
            if changed && error.is_none() && sender.try_send(Report::Control(message)).is_err() {
                warn!("pacing control file record could not be queued; datagram status remains available");
            }
        }
        Ok(PostAction::Continue)
    }) {
        warn!("could not register pacing control socket: {err}");
    }
}

fn control_status(command: &str, error: Option<&str>, report: &ReportState) -> serde_json::Value {
    serde_json::json!({
        "type":"control", "pid":std::process::id(), "epoch":EPOCH.load(Ordering::Relaxed),
        "timestamp_ms":epoch_ms(), "command":command, "accepted":error.is_none(), "error":error,
        "phase":report.phase, "recording":timing::enabled(), "direct_target":report.direct_target,
        "writer_ready":report.writer_ready.load(Ordering::Acquire),
        "allocation_policy":"preserved; capabilities may renegotiate buffers"
    })
}

fn reply(
    socket: &UnixDatagram,
    peer: &std::os::unix::net::SocketAddr,
    message: &serde_json::Value,
) {
    if let (Some(path), Ok(bytes)) = (peer.as_pathname(), serde_json::to_vec(message)) {
        if let Err(err) = socket.send_to(&bytes, path) {
            warn!("pacing control reply failed: {err}; query status before retrying a mutation");
        }
    }
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_distance_wraps_without_negative_gaps() {
        let previous = u32::MAX - 1;
        let current = 1u32;
        assert_eq!(current.wrapping_sub(previous).saturating_sub(1), 2);
    }

    #[test]
    fn hardware_events_and_throttled_delivery_keep_frame_identity() {
        // Also run this test in a fresh process with SMITHAY_FRAME_TIMING=1.
        if !timing::enabled() {
            return;
        }
        let stream = (u64::MAX, 55);
        timing::register_stream(stream, "test-output");
        let _scope = timing::enter(stream);
        let mut surface = SurfaceTiming::default();
        surface.event(10, Duration::from_millis(100), Duration::from_millis(101));
        surface.presented(
            10,
            Duration::from_millis(100),
            Some(QueueStamp {
                at: Duration::from_millis(98),
                epoch: EPOCH.load(Ordering::Relaxed),
            }),
            Some(Duration::from_millis(7)),
        );
        surface.event(10, Duration::from_millis(100), Duration::from_millis(102));
        surface.event(12, Duration::from_millis(114), Duration::from_millis(116));
        // Upstream may subsequently deliver a throttled callback with time zero.
        surface.event(12, Duration::ZERO, Duration::from_millis(117));
        surface.presented(
            12,
            Duration::from_millis(107),
            Some(QueueStamp {
                at: Duration::from_millis(110),
                epoch: EPOCH.load(Ordering::Relaxed),
            }),
            Some(Duration::from_millis(7)),
        );
        let snapshot = timing::drain()
            .into_iter()
            .find(|s| s.id == stream)
            .unwrap();
        let count = |counter| {
            snapshot
                .counters
                .iter()
                .find(|(c, _)| *c == counter)
                .map_or(0, |(_, v)| *v)
        };
        assert_eq!(count(Counter::PresentEvents), 2);
        assert_eq!(count(Counter::SequenceGaps), 1);
        assert_eq!(count(Counter::PresentLate), 1);
        let queued = snapshot
            .metrics
            .iter()
            .find(|(s, _)| *s == Stage::QueueToPresentation)
            .unwrap()
            .1
            .summary();
        assert_eq!(queued.count, 2);
        assert_eq!(queued.max_ns, 4_000_000);
    }

    #[test]
    fn in_flight_and_recording_off_frames_do_not_cross_trial_epochs() {
        if !timing::enabled() {
            return;
        }
        let stream = (u64::MAX, 57);
        timing::register_stream(stream, "epoch-test");
        let _scope = timing::enter(stream);
        let mut surface = SurfaceTiming::default();
        let epoch = EPOCH.load(Ordering::Relaxed);
        surface.event(1, Duration::from_millis(100), Duration::from_millis(101));
        surface.presented(
            1,
            Duration::from_millis(90),
            Some(QueueStamp {
                at: Duration::from_millis(90),
                epoch: epoch.wrapping_sub(1),
            }),
            None,
        );
        surface.event(2, Duration::from_millis(107), Duration::from_millis(108));
        surface.presented(2, Duration::from_millis(100), None, None);
        surface.event(3, Duration::from_millis(114), Duration::from_millis(115));
        surface.presented(
            3,
            Duration::from_millis(114),
            Some(QueueStamp {
                at: Duration::from_millis(110),
                epoch,
            }),
            None,
        );
        let snapshot = timing::drain()
            .into_iter()
            .find(|s| s.id == stream)
            .unwrap();
        let count = |counter| {
            snapshot
                .counters
                .iter()
                .find(|(c, _)| *c == counter)
                .map_or(0, |(_, v)| *v)
        };
        assert_eq!(count(Counter::PresentEvents), 1);
        assert_eq!(count(Counter::TransitionPresentationsIgnored), 2);
        assert_eq!(count(Counter::SequenceGaps), 0);
    }

    #[test]
    fn wrong_sequence_is_not_assigned_an_old_presentation_time() {
        if !timing::enabled() {
            return;
        }
        let stream = (u64::MAX, 56);
        timing::register_stream(stream, "unknown-time");
        let _scope = timing::enter(stream);
        let mut surface = SurfaceTiming::default();
        surface.event(1, Duration::from_millis(100), Duration::from_millis(101));
        surface.presented(
            2,
            Duration::from_millis(107),
            Some(QueueStamp {
                at: Duration::from_millis(100),
                epoch: EPOCH.load(Ordering::Relaxed),
            }),
            None,
        );
        let snapshot = timing::drain()
            .into_iter()
            .find(|s| s.id == stream)
            .unwrap();
        assert_eq!(
            snapshot
                .counters
                .iter()
                .find(|(c, _)| *c == Counter::UnknownClock)
                .unwrap()
                .1,
            1
        );
        assert!(!snapshot
            .metrics
            .iter()
            .any(|(s, _)| *s == Stage::PresentLateness));
    }
}
