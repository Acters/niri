//! Opt-in Gate 2 diagnostic: copy into a private scanout-capable BO and ask atomic KMS
//! to validate it with TEST_ONLY. It never requests presentation or a modeset and never
//! changes normal swapchain flags. It cannot establish that scanout actually waits for a copy fence.
//! A successful test proves only atomic request acceptance, not displayed pixels or timing.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::time::Duration;

use anyhow::{anyhow, ensure, Context};
use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::{GbmBuffer, GbmBufferFlags};
use smithay::backend::allocator::{Buffer as AllocatorBuffer, Fourcc, Modifier};
use smithay::backend::drm::gbm::framebuffer_from_bo;
use smithay::backend::drm::{DrmNode, DrmSurface, Framebuffer, PlaneConfig, PlaneState};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::vkbridge::VkBridge;
use smithay::backend::renderer::multigpu::ApiDevice;
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Color32F, Frame, Renderer};
use smithay::backend::session::Session;
use smithay::output::Output;
use smithay::reexports::calloop::channel::{self, Event};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::drm::control::{connector, framebuffer, Device};
use smithay::utils::{Buffer, Physical, Rectangle, Transform};

use super::{Tty, TtyOutputState};
use crate::niri::State;

/// Register no event source and start no thread unless the explicit output opt-in is set.
pub(super) fn register(event_loop: &LoopHandle<'static, State>, node: DrmNode) {
    let Some(output) = std::env::var_os("NIRI_VK_KMS_TEST_ONLY_OUTPUT") else {
        return;
    };
    let Ok(output) = output.into_string() else {
        warn!("KMS_TRANSFER_TEST_ONLY_FAIL stage=opt_in: output name is not UTF-8");
        return;
    };
    if output.is_empty() {
        warn!("KMS_TRANSFER_TEST_ONLY_FAIL stage=opt_in: output name is empty");
        return;
    }
    info!(%output, ?node, "KMS_TRANSFER_TEST_ONLY_ARMED: atomic request validation only; no presentation or scanout-wait proof");
    let (sender, receiver) = channel::channel();
    let selected_output = output.clone();
    let mut handled = false;
    let registration = event_loop.insert_source(receiver, move |event, _, state| {
        if handled {
            return;
        }
        handled = true;
        let result = match event {
            Event::Msg(result) => result,
            Event::Closed => {
                warn!(output = %selected_output, stage = "init_channel_closed", "KMS_TRANSFER_TEST_ONLY_FAIL: initialization ended without a result");
                return;
            }
        };
        let mut bridge = match result {
            Ok(bridge) => bridge,
            Err(err) => {
                warn!(output = %selected_output, stage = "vulkan_init", error = ?err, "KMS_TRANSFER_TEST_ONLY_FAIL");
                return;
            }
        };
        let output = state.niri.output_state.keys()
            .find(|output| output.name() == selected_output)
            .cloned();
        match output {
            Some(output) => state.backend.tty().probe_kms(&output, &mut bridge),
            None => warn!(output = %selected_output, stage = "output_lookup", "KMS_TRANSFER_TEST_ONLY_FAIL: selected output is not connected"),
        }
    });
    let token = match registration {
        Ok(token) => token,
        Err(err) => {
            warn!(%output, stage = "channel_registration", error = ?err, "KMS_TRANSFER_TEST_ONLY_FAIL");
            return;
        }
    };
    // Delay even logical-device construction until the event loop has started processing.
    // Never resurrect early/pre-DRM device creation merely to run a diagnostic. Only node
    // identity goes to the worker; none of the compositor's DRM-master fds leave this thread.
    event_loop.insert_idle(move |state| {
        let selected_output = output.clone();
        let mut sender = Some(sender);
        if let Err(err) = state.niri.event_loop.insert_source(
            Timer::from_duration(Duration::from_secs(2)),
            move |_, _, _| {
                if let Some(sender) = sender.take() {
                    if let Err(err) = std::thread::Builder::new()
                        .name("vk-kms-test-init".into())
                        .spawn(move || { let _ = sender.send(VkBridge::new(node)); })
                    {
                        warn!(output = %selected_output, stage = "thread_spawn", error = ?err, "KMS_TRANSFER_TEST_ONLY_FAIL");
                    }
                }
                TimeoutAction::Drop
            },
        ) {
            state.niri.event_loop.remove(token);
            warn!(%output, stage = "timer_registration", error = ?err, "KMS_TRANSFER_TEST_ONLY_FAIL");
        }
    });
}

// A failed framebuffer/test/export operation must not drop either allocation while GPU
// work still accesses it. These guards are declared after their allocations, so retirement
// runs first on every error path. Interrupted is not completion and must be retried.
struct Retire(SyncPoint);

impl Retire {
    fn wait(&self) {
        while self.0.wait().is_err() {
            std::thread::yield_now();
        }
    }
}

impl Drop for Retire {
    fn drop(&mut self) {
        self.wait();
    }
}

impl Tty {
    fn probe_kms(&mut self, output: &Output, bridge: &mut VkBridge) {
        let output_name = output.name();
        for format in [Fourcc::Abgr8888, Fourcc::Abgr2101010] {
            let mut stage = "preflight";
            let mut actual_modifier = None;
            let mut native_fence = false;
            let result = (|| -> anyhow::Result<()> {
                ensure!(self.session.is_active(), "session is inactive");
                let state = output
                    .user_data()
                    .get::<TtyOutputState>()
                    .context("selected output has no TTY state")?;
                let device = self
                    .devices
                    .get(&state.node)
                    .context("output device disappeared")?;
                ensure!(device.drm.is_atomic(), "refusing non-atomic DRM device");
                let tty_surface = device
                    .surfaces
                    .get(&state.crtc)
                    .context("output surface disappeared")?;
                let surface = tty_surface.compositor.surface();
                // Legacy test_state(false) is a no-op success, not a hardware test. Never use it.
                ensure!(!surface.is_legacy(), "refusing legacy surface");
                ensure!(
                    !surface.commit_pending(),
                    "refusing surface with pending mode/connector/VRR changes"
                );
                ensure!(
                    surface
                        .current_connectors()
                        .into_iter()
                        .any(|c| c == tty_surface.connector),
                    "selected connector is not active on the surface"
                );
                let connector = device.drm.get_connector(tty_surface.connector, false)?;
                ensure!(
                    connector.state() == connector::State::Connected,
                    "selected connector is disconnected"
                );
                // ADDFB2 must see handles from this SAME primary fd, not a render-node GBM fd.
                ensure!(
                    device.gbm.as_fd().as_raw_fd() == surface.device_fd().as_fd().as_raw_fd(),
                    "GBM and KMS framebuffer fds differ"
                );
                let (width, height) = surface.current_mode().size();
                let (width, height) = (u32::from(width), u32::from(height));
                ensure!(width > 0 && height > 0, "invalid current mode size");
                stage = "source_allocation";
                let mut modifiers = bridge.source_modifiers(format, width, height)?;
                let source_device = self
                    .gpu_manager
                    .devices_mut()?
                    .find(|device| *device.node() == self.primary_render_node)
                    .context("primary source renderer is unavailable")?;
                let render_formats =
                    <GlesRenderer as Bind<Dmabuf>>::supported_formats(source_device.renderer())
                        .context("source renderer has no explicit render formats")?;
                modifiers.retain(|modifier| {
                    render_formats.iter().any(|candidate| {
                        candidate.code == format && candidate.modifier == *modifier
                    })
                });
                ensure!(
                    !modifiers.is_empty(),
                    "no shared EGL/Vulkan source modifiers"
                );
                let mut source = source_device
                    .allocator()
                    .create_buffer(width, height, format, &modifiers)
                    .map_err(|err| anyhow!("source allocation failed: {err:?}"))?;
                ensure!(
                    source.format().code == format
                        && modifiers.contains(&source.format().modifier)
                        && source.num_planes() == 1,
                    "source allocation does not match negotiated descriptor"
                );

                stage = "scanout_allocation";
                // This direct API call is intentionally local to the probe. Do not enable
                // Smithay's create_with_modifiers2 feature or change normal allocator flags.
                let bo = device
                    .gbm
                    .create_buffer_object_with_modifiers2::<()>(
                        width,
                        height,
                        format,
                        [Modifier::Linear].into_iter(),
                        GbmBufferFlags::SCANOUT | GbmBufferFlags::RENDERING,
                    )
                    .context("allocating explicit LINEAR SCANOUT|RENDERING BO")?;
                let bo = GbmBuffer::from_bo(bo, false);
                actual_modifier = Some(u64::from(AllocatorBuffer::format(&bo).modifier));
                let destination = bo.export().context("exporting private target BO")?;
                ensure!(
                    destination.format().modifier == Modifier::Linear
                        && destination.format().code == format
                        && destination.num_planes() == 1,
                    "target allocation is not the requested explicit single-plane LINEAR format"
                );
                ensure!(
                    source.size() == destination.size()
                        && destination.size().w == width as i32
                        && destination.size().h == height as i32,
                    "source/target allocation sizes do not match active mode"
                );
                stage = "framebuffer_creation";
                let framebuffer = framebuffer_from_bo(surface.device_fd(), &bo, true)
                    .context("adding private target framebuffer on owning KMS fd")?;
                info!(output = %output_name, ?format, ?actual_modifier,
                    framebuffer_format = ?framebuffer.format(), width, height,
                    "KMS_TRANSFER_TEST_ONLY_ALLOCATED: private buffers; opaque KMS framebuffer format");
                let full =
                    Rectangle::<i32, Buffer>::from_size((width as i32, height as i32).into());

                stage = "first_source_render";
                let acquire = Retire(render_source(
                    source_device.renderer_mut(),
                    &mut source,
                    false,
                )?);
                stage = "first_copy";
                let copy = Retire(bridge.copy(&source, &destination, &acquire.0, None, &[full])?);
                copy.wait();
                stage = "completed_copy_no_fence";
                // The atomic-only helper is the ONLY display-state operation in this module.
                // TEST_ONLY does not attach this FB or leave a real display reader to release.
                test(surface, *framebuffer.as_ref(), width, height, None)?;
                info!(output = %output_name, ?format, ?actual_modifier, native_fence = false, stage,
                    "KMS_TRANSFER_TEST_ONLY_PASS: request accepted; not scanout/pixel validation");

                stage = "second_source_render";
                let acquire = Retire(render_source(
                    source_device.renderer_mut(),
                    &mut source,
                    true,
                )?);
                stage = "second_copy";
                let copy = Retire(bridge.copy(&source, &destination, &acquire.0, None, &[full])?);
                stage = "native_copy_fence_export";
                let fence = copy
                    .0
                    .export()
                    .context("second copy has no exportable native fence")?;
                native_fence = true;
                stage = "native_copy_fence_test";
                let complete_at_test = copy.0.is_reached();
                // Do NOT pre-wait this copy: submit its native SYNC_FD in IN_FENCE_FD.
                // TEST_ONLY may accept the property without waiting or validating execution.
                let tested = test(
                    surface,
                    *framebuffer.as_ref(),
                    width,
                    height,
                    Some(fence.as_fd()),
                );
                copy.wait(); // retirement is required even if TEST_ONLY rejected the request
                tested?;
                info!(output = %output_name, ?format, ?actual_modifier, native_fence, stage, complete_at_test,
                    "KMS_TRANSFER_TEST_ONLY_PASS: request accepted; no proof of actual scanout fence waiting");
                Ok(())
            })();
            if let Err(err) = result {
                warn!(output = %output_name, ?format, ?actual_modifier, native_fence, stage, error = ?err,
                    "KMS_TRANSFER_TEST_ONLY_FAIL");
            }
        }
    }
}

fn render_source(
    renderer: &mut GlesRenderer,
    source: &mut Dmabuf,
    alternate: bool,
) -> anyhow::Result<SyncPoint> {
    let size = source.size();
    let mut framebuffer = renderer.bind(source)?;
    let mut frame =
        renderer.render(&mut framebuffer, (size.w, size.h).into(), Transform::Normal)?;
    let colors = [
        Color32F::new(0.125, 0.375, 0.625, 1.),
        Color32F::new(0.75, 0.1875, 0.4375, 1.),
    ];
    let index = usize::from(alternate);
    frame.clear(
        colors[index],
        &[Rectangle::<i32, Physical>::from_size(
            (size.w, size.h).into(),
        )],
    )?;
    frame.clear(
        colors[1 - index],
        &[Rectangle::from_size((size.w / 3, size.h * 2 / 5).into())],
    )?;
    Ok(frame.finish()?)
}

fn test(
    surface: &DrmSurface,
    framebuffer: framebuffer::Handle,
    width: u32,
    height: u32,
    fence: Option<BorrowedFd<'_>>,
) -> anyhow::Result<()> {
    // Defense in depth: it is unsafe to reinterpret legacy's unconditional Ok as TEST_ONLY.
    ensure!(!surface.is_legacy(), "refusing legacy test_state");
    ensure!(
        !surface.commit_pending(),
        "refusing pending modeset/state changes"
    );
    surface
        .test_state(
            [PlaneState {
                handle: surface.plane(),
                config: Some(PlaneConfig {
                    src: Rectangle::<f64, Buffer>::from_size(
                        (f64::from(width), f64::from(height)).into(),
                    ),
                    dst: Rectangle::<i32, Physical>::from_size(
                        (width as i32, height as i32).into(),
                    ),
                    transform: Transform::Normal,
                    alpha: 1.,
                    damage_clips: None,
                    fb: framebuffer,
                    fence,
                }),
            }],
            false,
        )
        .context("atomic TEST_ONLY without ALLOW_MODESET")
}
