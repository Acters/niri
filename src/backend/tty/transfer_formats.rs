//! Capability-ready target-format negotiation for an explicitly target-side Vulkan copy.
//! Normal/native outputs retain upstream allocation policy. No Vulkan creation runs here:
//! the manager query polls its existing background-initialized, per-pair engine.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use smithay::backend::allocator::gbm::GbmAllocator;
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::drm::{DrmDeviceFd, DrmNode};
use smithay::backend::renderer::multigpu::VulkanCopyDevice;
use smithay::reexports::drm::control::crtc;
use smithay::utils::{Buffer, Size};

use super::{GbmDrmCompositor, Tty};

#[derive(Default)]
pub(super) struct TransferFormats {
    decision: Option<Decision>,
    // No buffer ownership lives here: old/current/pending Slots remain with DrmCompositor.
    fallback: Option<(Fourcc, Vec<Modifier>)>,
    restore_pending: bool,
    retry_scheduled: bool,
}

struct Decision {
    epoch: Arc<()>,
    format: Fourcc,
    size: Size<i32, Buffer>,
}

impl TransferFormats {
    fn matches(&self, epoch: &Arc<()>, format: Fourcc, size: Size<i32, Buffer>) -> bool {
        self.decision
            .as_ref()
            .is_some_and(|d| Arc::ptr_eq(&d.epoch, epoch) && d.format == format && d.size == size)
    }

    pub fn rollback(
        &mut self,
        compositor: &mut GbmDrmCompositor,
        allocator: &GbmAllocator<DrmDeviceFd>,
    ) -> Option<Duration> {
        self.fallback.as_ref()?;
        self.restore_pending = true;
        // Keep the attempted decision to avoid retrying a rejected native transition.
        match self.try_restore(compositor, allocator) {
            Ok(()) => Some(Duration::from_millis(1)),
            Err(err) => {
                warn!("could not restore prior swapchain modifiers: {err:#}");
                if self.retry_scheduled {
                    None
                } else {
                    self.retry_scheduled = true;
                    Some(Duration::from_millis(100))
                }
            }
        }
    }

    fn try_restore(
        &mut self,
        compositor: &mut GbmDrmCompositor,
        allocator: &GbmAllocator<DrmDeviceFd>,
    ) -> anyhow::Result<()> {
        if !self.restore_pending {
            return Ok(());
        }
        let Some((format, modifiers)) = self.fallback.as_ref() else {
            self.restore_pending = false;
            return Ok(());
        };
        if compositor.format() != *format {
            // An independent format/recreation decision superseded this recovery.
            self.fallback = None;
            self.restore_pending = false;
            self.decision = None;
            return Ok(());
        }
        anyhow::ensure!(
            !compositor.surface().is_legacy() && !compositor.surface().commit_pending(),
            "waiting for a stable atomic surface before restoring modifiers"
        );
        compositor
            .set_format(allocator.clone(), *format, modifiers.clone())
            .context("restoring saved swapchain modifiers")?;
        self.fallback = None;
        self.restore_pending = false;
        warn!("restored normal swapchain modifiers after reverse-transfer render/queue failure");
        Ok(())
    }
}

pub(super) fn update(tty: &mut Tty, node: DrmNode, crtc: crtc::Handle) -> anyhow::Result<()> {
    if !tty.direct_target_transfer || tty.vulkan_copy_device != VulkanCopyDevice::Target {
        return Ok(());
    }
    let Some(device) = tty.devices.get_mut(&node) else {
        return Ok(());
    };
    let Some(target) = device.render_node else {
        return Ok(());
    };
    if target == tty.primary_render_node || !device.drm.is_active() || !device.drm.is_atomic() {
        return Ok(());
    }
    let Some(surface) = device.surfaces.get_mut(&crtc) else {
        return Ok(());
    };
    if surface.compositor.surface().is_legacy() || surface.compositor.surface().commit_pending() {
        return Ok(());
    }
    surface
        .transfer_formats
        .try_restore(&mut surface.compositor, &device.allocator)?;
    let (width, height) = surface.compositor.pending_mode().size();
    let size: Size<i32, Buffer> = (i32::from(width), i32::from(height)).into();
    let format = surface.compositor.format();
    if surface
        .transfer_formats
        .matches(tty.gpu_manager.vulkan_transfer_generation(), format, size)
    {
        return Ok(());
    }
    let Some(modifiers) = tty
        .gpu_manager
        .vulkan_transfer_target_modifiers(&tty.primary_render_node, &target, format, size)
        .context("querying reverse transfer target modifiers")?
    else {
        // Pending initialization is not a permanent decision. Retry on ordinary redraw.
        return Ok(());
    };
    let epoch = tty.gpu_manager.vulkan_transfer_generation().clone();
    if modifiers.is_empty() {
        surface.transfer_formats.decision = Some(Decision {
            epoch,
            format,
            size,
        });
        return Ok(());
    }
    let candidates: Vec<_> = {
        let renderer = tty.gpu_manager.single_renderer(&target)?;
        let formats = renderer.as_ref().egl_context().dmabuf_render_formats();
        modifiers
            .into_iter()
            .filter(|modifier| {
                !super::is_ccs_modifier(*modifier)
                    && formats
                        .iter()
                        .any(|f| f.code == format && f.modifier == *modifier)
            })
            .collect()
    };
    if !Arc::ptr_eq(&epoch, tty.gpu_manager.vulkan_transfer_generation()) {
        return Ok(()); // Renderer acquisition invalidated the queried engine.
    }
    surface.transfer_formats.decision = Some(Decision {
        epoch,
        format,
        size,
    });
    if candidates.is_empty() {
        return Ok(());
    }
    let current = surface.compositor.modifiers();
    if !current.is_empty() && current.iter().all(|modifier| candidates.contains(modifier)) {
        return Ok(());
    }
    let previous = current.to_vec();
    // set_format validates a new chain before replacing it; old leases remain alive in
    // current/pending/queued frame state. Fresh slot ages force repaint of new buffers.
    // Its TEST_ONLY permits modesetting; actual page-flip/fence acceptance still needs
    // the approved live test. Retain a one-shot fallback for any later render/queue error.
    match surface
        .compositor
        .set_format(device.allocator.clone(), format, candidates)
    {
        Ok(()) => {
            if surface
                .transfer_formats
                .fallback
                .as_ref()
                .is_none_or(|(old_format, _)| *old_format != format)
            {
                surface.transfer_formats.fallback = Some((format, previous));
            }
            surface.transfer_formats.restore_pending = false;
            surface.transfer_formats.retry_scheduled = false;
            info!(output = %surface.name.connector, ?format, modifiers = ?surface.compositor.modifiers(),
                copy_device = ?target, "reverse transfer native swapchain negotiated; live route checked per frame");
        }
        Err(err) => warn!(output = %surface.name.connector, ?err,
            "reverse native modifier negotiation failed; retaining normal swapchain"),
    }
    Ok(())
}

pub(super) fn schedule_retry(
    niri: &mut crate::niri::Niri,
    output: &smithay::output::Output,
    delay: Duration,
) {
    use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
    let output = output.clone();
    // Runs after Niri::redraw has processed Skipped and finalized redraw_state.
    if let Err(err) =
        niri.event_loop
            .insert_source(Timer::from_duration(delay), move |_, _, state| {
                if state.niri.output_state.contains_key(&output) {
                    state.niri.queue_redraw(&output);
                }
                TimeoutAction::Drop
            })
    {
        warn!("could not queue reverse transfer recovery redraw: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_decision_is_scoped_to_epoch_format_and_extent() {
        let epoch = Arc::new(());
        let state = TransferFormats {
            decision: Some(Decision {
                epoch: epoch.clone(),
                format: Fourcc::Abgr8888,
                size: (1920, 1080).into(),
            }),
            ..Default::default()
        };
        assert!(state.matches(&epoch, Fourcc::Abgr8888, (1920, 1080).into()));
        assert!(!state.matches(&Arc::new(()), Fourcc::Abgr8888, (1920, 1080).into()));
        assert!(!state.matches(&epoch, Fourcc::Abgr2101010, (1920, 1080).into()));
        assert!(!state.matches(&epoch, Fourcc::Abgr8888, (1280, 720).into()));
    }
}
