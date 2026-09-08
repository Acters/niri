use std::path::PathBuf;

use crate::utils::MergeWith;

/// KDL-controlled policy for the cross-GPU Vulkan bridge.
///
/// Presence of this section selects the whole policy instead of legacy environment settings.
/// The primary renderer is still selected by `debug.render_drm_device`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulkanBridge {
    pub enabled: bool,
    pub direct_target: bool,
    pub copy_device: BridgeCopyDevice,
    pub timing: BridgeTiming,
}

impl Default for VulkanBridge {
    fn default() -> Self {
        Self {
            enabled: true,
            direct_target: true,
            copy_device: BridgeCopyDevice::default(),
            timing: BridgeTiming::default(),
        }
    }
}

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum BridgeCopyDevice {
    #[default]
    Render,
    Target,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BridgeTiming {
    pub enabled: bool,
    /// Optional here; the runtime handles enabled timing without an output path.
    pub output: Option<PathBuf>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq, Eq)]
pub struct VulkanBridgePart {
    #[knuffel(child, unwrap(argument))]
    pub enabled: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub direct_target: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub copy_device: Option<BridgeCopyDevice>,
    #[knuffel(child)]
    pub timing: Option<BridgeTimingPart>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq, Eq)]
pub struct BridgeTimingPart {
    #[knuffel(child, unwrap(argument))]
    pub enabled: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub output: Option<PathBuf>,
}

impl MergeWith<VulkanBridgePart> for VulkanBridge {
    fn merge_with(&mut self, part: &VulkanBridgePart) {
        merge_clone!((self, part), enabled, direct_target, copy_device);
        merge!((self, part), timing);
    }
}

impl MergeWith<BridgeTimingPart> for BridgeTiming {
    fn merge_with(&mut self, part: &BridgeTimingPart) {
        merge_clone!((self, part), enabled);
        merge_clone_opt!((self, part), output);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::Config;

    #[test]
    fn absent_and_empty() {
        assert_eq!(Config::default().vulkan_bridge, None);
        assert_eq!(Config::parse_mem("").unwrap().vulkan_bridge, None);
        for text in ["vulkan-bridge {}", "vulkan-bridge { timing {}; }"] {
            assert_eq!(
                Config::parse_mem(text).unwrap().vulkan_bridge,
                Some(VulkanBridge {
                    enabled: true,
                    direct_target: true,
                    copy_device: BridgeCopyDevice::Render,
                    timing: BridgeTiming {
                        enabled: false,
                        output: None,
                    },
                }),
            );
        }
    }

    #[test]
    fn explicit_values() {
        let config = Config::parse_mem(
            r#"
            vulkan-bridge {
                enabled false
                direct-target false
                copy-device "target"
                timing {
                    enabled false
                    output "/path/log.jsonl"
                }
            }
            "#,
        )
        .unwrap();
        assert_eq!(
            config.vulkan_bridge,
            Some(VulkanBridge {
                enabled: false,
                direct_target: false,
                copy_device: BridgeCopyDevice::Target,
                timing: BridgeTiming {
                    enabled: false,
                    output: Some(PathBuf::from("/path/log.jsonl")),
                },
            }),
        );

        let bridge = Config::parse_mem(
            r#"vulkan-bridge { enabled true; direct-target true; copy-device "render"; timing { enabled true; }; }"#,
        )
        .unwrap()
        .vulkan_bridge
        .unwrap();
        assert_eq!(bridge.copy_device, BridgeCopyDevice::Render);
        assert!(bridge.enabled && bridge.direct_target && bridge.timing.enabled);
        assert_eq!(bridge.timing.output, None);
    }

    #[test]
    fn invalid_values_and_duplicate_sections() {
        for text in [
            r#"vulkan-bridge { copy-device "other"; }"#,
            r#"vulkan-bridge { copy-device "Target"; }"#,
            "vulkan-bridge { copy-device true; }",
            "vulkan-bridge { unknown true; }",
            "vulkan-bridge { enabled; }",
            r#"vulkan-bridge { enabled "false"; }"#,
            "vulkan-bridge { direct-target 1; }",
            "vulkan-bridge { timing { enabled; }; }",
            r#"vulkan-bridge { timing { enabled "true"; }; }"#,
            "vulkan-bridge { timing { unknown false; }; }",
            "vulkan-bridge { timing { output false; }; }",
            "vulkan-bridge { enabled true; enabled false; }",
            "vulkan-bridge { timing {}; timing {}; }",
            "vulkan-bridge {}\nvulkan-bridge {}",
        ] {
            assert!(Config::parse_mem(text).is_err(), "accepted {text}");
        }
    }

    /// A dependency-free temporary directory, removed even if an assertion panics.
    struct IncludeDir(PathBuf);

    impl IncludeDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "niri-config-vulkan-bridge-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed),
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("creating include test directory: {err}"),
                }
            }
        }

        fn write(&self, name: &str, text: &str) {
            fs::write(self.0.join(name), text).unwrap();
        }

        fn parse(&self, text: &str) -> VulkanBridge {
            Config::parse(&self.0.join("config.kdl"), text)
                .config
                .unwrap()
                .vulkan_bridge
                .unwrap()
        }
    }

    impl Drop for IncludeDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn positional_include_merge() {
        let dir = IncludeDir::new();
        dir.write(
            "base.kdl",
            r#"vulkan-bridge {
                enabled false
                direct-target false
                copy-device "target"
                timing { enabled true; output "/path/base.jsonl"; }
            }"#,
        );
        dir.write("empty.kdl", "vulkan-bridge { timing {}; }");
        let override_section = r#"vulkan-bridge {
            enabled true
            timing { enabled false; }
        }"#;
        let after = dir.parse(&format!(
            "include \"base.kdl\"\n{override_section}\ninclude \"empty.kdl\"",
        ));
        assert_eq!(
            after,
            VulkanBridge {
                enabled: true,
                direct_target: false,
                copy_device: BridgeCopyDevice::Target,
                timing: BridgeTiming {
                    enabled: false,
                    output: Some(PathBuf::from("/path/base.jsonl")),
                },
            },
        );
        let before = dir.parse(&format!("{override_section}\ninclude \"base.kdl\""));
        assert!(!before.enabled && !before.direct_target);
        assert_eq!(before.copy_device, BridgeCopyDevice::Target);
        assert!(before.timing.enabled);
        assert_eq!(before.timing.output, after.timing.output);

        dir.write(
            "output.kdl",
            r#"vulkan-bridge { timing { output "/path/override.jsonl"; }; }"#,
        );
        let output = dir.parse("include \"base.kdl\"\ninclude \"output.kdl\"");
        assert!(output.timing.enabled);
        assert!(!output.enabled && !output.direct_target);
        assert_eq!(output.copy_device, BridgeCopyDevice::Target);
        assert_eq!(
            output.timing.output,
            Some(PathBuf::from("/path/override.jsonl")),
        );
        assert_eq!(dir.parse("include \"empty.kdl\""), VulkanBridge::default());

        let render = dir.parse(
            r#"include "base.kdl"
            vulkan-bridge { direct-target true; copy-device "render"; }"#,
        );
        assert!(render.direct_target);
        assert_eq!(render.copy_device, BridgeCopyDevice::Render);
        assert!(!render.enabled);
        assert!(render.timing.enabled);
    }
}
