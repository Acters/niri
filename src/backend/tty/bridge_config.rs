//! Startup bridge policy. An explicit KDL block owns the entire policy;
//! legacy environment variables are consulted only when the block is absent.

use std::ffi::OsString;
use std::path::PathBuf;

use niri_config::{BridgeCopyDevice, VulkanBridge};
use smithay::backend::renderer::multigpu::VulkanCopyDevice;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Settings {
    pub enabled: bool,
    pub direct_target: bool,
    pub copy_device: VulkanCopyDevice,
    pub timing_enabled: bool,
    pub timing_output: Option<PathBuf>,
}

pub(super) fn resolve(
    config: Option<&VulkanBridge>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Settings {
    if let Some(config) = config {
        return Settings {
            enabled: config.enabled,
            direct_target: config.enabled && config.direct_target,
            copy_device: match config.copy_device {
                BridgeCopyDevice::Render => VulkanCopyDevice::Render,
                BridgeCopyDevice::Target => VulkanCopyDevice::Target,
            },
            timing_enabled: config.timing.enabled,
            timing_output: config.timing.output.clone(),
        };
    }

    let enabled = env("NIRI_VKBRIDGE").is_none_or(|value| value != "0");
    let copy_device = match env("NIRI_VK_COPY_DEVICE") {
        Some(value) if value == "target" => VulkanCopyDevice::Target,
        Some(value) if value != "render" => {
            warn!(
                ?value,
                "unknown legacy NIRI_VK_COPY_DEVICE; using render GPU"
            );
            VulkanCopyDevice::Render
        }
        _ => VulkanCopyDevice::Render,
    };
    Settings {
        enabled,
        direct_target: enabled && env("NIRI_VK_DIRECT_TARGET").is_some_and(|v| v == "1"),
        copy_device,
        timing_enabled: env("SMITHAY_FRAME_TIMING").is_some_and(|v| v == "1"),
        timing_output: env("NIRI_FRAME_TIMING_FILE").map(PathBuf::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niri_config::BridgeTiming;

    #[test]
    fn kdl_policy_does_not_consult_legacy_environment() {
        let config = VulkanBridge {
            enabled: true,
            direct_target: true,
            copy_device: BridgeCopyDevice::Target,
            timing: BridgeTiming {
                enabled: false,
                output: None,
            },
        };
        let resolved = resolve(Some(&config), |_| {
            panic!("KDL must not read legacy environment")
        });
        assert!(resolved.enabled && resolved.direct_target);
        assert_eq!(resolved.copy_device, VulkanCopyDevice::Target);
        assert!(!resolved.timing_enabled);
        assert_eq!(resolved.timing_output, None);
    }

    #[test]
    fn explicit_disable_also_disables_direct_writes() {
        let config = VulkanBridge {
            enabled: false,
            ..Default::default()
        };
        let resolved = resolve(Some(&config), |_| Some("1".into()));
        assert!(!resolved.enabled && !resolved.direct_target);
        assert!(!resolved.timing_enabled);
    }

    #[test]
    fn absent_block_preserves_existing_environment_contract() {
        let resolved = resolve(None, |key| match key {
            "NIRI_VK_COPY_DEVICE" => Some("target".into()),
            "NIRI_VK_DIRECT_TARGET" | "SMITHAY_FRAME_TIMING" => Some("1".into()),
            "NIRI_FRAME_TIMING_FILE" => Some("/tmp/bridge-timing.jsonl".into()),
            _ => None,
        });
        assert!(resolved.enabled && resolved.direct_target && resolved.timing_enabled);
        assert_eq!(resolved.copy_device, VulkanCopyDevice::Target);
        assert_eq!(
            resolved.timing_output,
            Some(PathBuf::from("/tmp/bridge-timing.jsonl"))
        );
        let defaults = resolve(None, |_| None);
        assert!(defaults.enabled);
        assert!(!defaults.direct_target && !defaults.timing_enabled);
        assert_eq!(defaults.copy_device, VulkanCopyDevice::Render);
    }

    #[test]
    fn both_documented_kdl_profiles_resolve_without_environment() {
        for (node, role, expected) in [
            ("/dev/dri/renderD129", "render", VulkanCopyDevice::Render),
            ("/dev/dri/renderD128", "target", VulkanCopyDevice::Target),
        ] {
            let text = format!(
                r#"
                debug {{ render-drm-device "{node}"; }}
                vulkan-bridge {{ enabled true; direct-target true; copy-device "{role}"; }}
            "#
            );
            let config = niri_config::Config::parse_mem(&text).unwrap();
            assert_eq!(config.debug.render_drm_device, Some(PathBuf::from(node)));
            let resolved = resolve(config.vulkan_bridge.as_ref(), |_| {
                panic!("unexpected env lookup")
            });
            assert!(resolved.enabled && resolved.direct_target);
            assert_eq!(resolved.copy_device, expected);
            assert!(!resolved.timing_enabled);
        }
    }

    #[test]
    fn kdl_timing_output_replaces_the_legacy_path() {
        let config = niri_config::Config::parse_mem(
            r#"
            vulkan-bridge { timing { enabled true; output "/private/kdl.jsonl"; }; }
        "#,
        )
        .unwrap();
        let resolved = resolve(config.vulkan_bridge.as_ref(), |_| {
            panic!("unexpected env lookup")
        });
        assert!(resolved.timing_enabled);
        assert_eq!(
            resolved.timing_output,
            Some(PathBuf::from("/private/kdl.jsonl"))
        );
    }

    #[test]
    fn legacy_disabled_bridge_cannot_enable_direct_writes() {
        let resolved = resolve(None, |key| match key {
            "NIRI_VKBRIDGE" => Some("0".into()),
            "NIRI_VK_DIRECT_TARGET" => Some("1".into()),
            _ => None,
        });
        assert!(!resolved.enabled && !resolved.direct_target);
    }
}
