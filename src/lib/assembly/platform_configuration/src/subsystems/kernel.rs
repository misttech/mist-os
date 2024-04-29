// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::kernel_config::PlatformKernelConfig;

pub(crate) struct KernelSubsystem;

impl DefineSubsystemConfiguration<PlatformKernelConfig> for KernelSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        kernel_config: &PlatformKernelConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if kernel_config.lru_memory_compression && !kernel_config.memory_compression {
            anyhow::bail!("'lru_memory_compression' can only be enabled with 'memory_compression'");
        }
        if kernel_config.memory_compression {
            builder.platform_bundle("kernel_anonymous_memory_compression");
        }
        if kernel_config.lru_memory_compression {
            builder.platform_bundle("kernel_anonymous_memory_compression_eager_lru");
        }
        if kernel_config.continuous_eviction {
            builder.platform_bundle("kernel_evict_continuous");
        }

        // If the board supports the PMM checker, and this is an eng build-type
        // build, enable the pmm checker.
        if context.board_info.provides_feature("fuchsia::pmm_checker")
            && context.build_type == &BuildType::Eng
        {
            builder.platform_bundle("kernel_pmm_checker_enabled");
        }

        if context.board_info.kernel.contiguous_physical_pages {
            builder.platform_bundle("kernel_contiguous_physical_pages");
        }

        if let Some(aslr_entropy_bits) = kernel_config.aslr_entropy_bits {
            let kernek_arg = format!("aslr.entropy_bits={}", aslr_entropy_bits);
            builder.kernel_arg(kernek_arg);
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::subsystems::ConfigurationBuilderImpl;

    #[test]
    fn test_define_configuration() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config: PlatformKernelConfig = Default::default();
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);
        assert!(result.is_ok());
        assert!(builder.build().kernel_args.is_empty());
    }

    #[test]
    fn test_define_configuration_aslr() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config =
            PlatformKernelConfig { aslr_entropy_bits: Some(12), ..Default::default() };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);
        assert!(result.is_ok());
        assert!(builder.build().kernel_args.contains("aslr.entropy_bits=12"));
    }

    #[test]
    fn test_define_configuration_no_aslr() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config = Default::default();
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);
        assert!(result.is_ok());
        assert!(!builder.build().kernel_args.iter().any(|s| s.starts_with("aslr.entropy_bits=12")));
    }
}
