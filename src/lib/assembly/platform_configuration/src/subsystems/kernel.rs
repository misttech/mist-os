// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{ensure, Context};
use assembly_config_schema::platform_config::kernel_config::{MemorySize, PlatformKernelConfig};
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

        // A positive value specifies a fixed number of bytes, while a negative
        // value indicates a relative amount, expressed as a percentage of the
        // system's physical memory.
        // Refer to src/devices/sysmem/drivers/sysmem/device.cc for percentage the value range.
        let encode_to_integer = |mem_size: &MemorySize| -> anyhow::Result<i64> {
            match mem_size {
                MemorySize::Fixed(size) => Ok(i64::try_from(*size)?),
                MemorySize::Relative(percentage) => {
                    ensure!(
                        *percentage >= 1 && *percentage <= 99,
                        "Percentage value ({}) must be larger then 0 and less that 100",
                        *percentage
                    );
                    Ok(-i64::from(*percentage))
                }
            }
        };

        if let Some(contiguous_memory_size) = &kernel_config.sysmem_contiguous_memory_size {
            builder.kernel_arg(format!(
                "driver.sysmem.contiguous_memory_size={}",
                encode_to_integer(contiguous_memory_size)
                    .context("Failed to convert `sysmem_contiguous_memory_size`")?
            ));
        }
        if let Some(protected_memory_size) = &kernel_config.sysmem_protected_memory_size {
            builder.kernel_arg(format!(
                "driver.sysmem.protected_memory_size={}",
                encode_to_integer(protected_memory_size)
                    .context("Failed to convert `sysmem_protected_memory_size`")?
            ));
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
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &Default::default(), &mut builder);
        assert!(result.is_ok());
        assert!(builder.build().kernel_args.is_empty());
    }

    #[test]
    fn test_contiguous_memory_size() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config = PlatformKernelConfig {
            sysmem_contiguous_memory_size: Some(MemorySize::Fixed(123)),
            sysmem_protected_memory_size: Some(MemorySize::Fixed(456)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);
        assert!(result.is_ok());
        let config = builder.build();
        assert!(config.kernel_args.contains("driver.sysmem.contiguous_memory_size=123"));
        assert!(config.kernel_args.contains("driver.sysmem.protected_memory_size=456"));
    }

    #[test]
    fn test_contiguous_memory_size_negative() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config = PlatformKernelConfig {
            sysmem_contiguous_memory_size: Some(MemorySize::Relative(5)),
            sysmem_protected_memory_size: Some(MemorySize::Relative(12)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);
        assert!(result.is_ok());
        let config = builder.build();
        assert!(config.kernel_args.contains("driver.sysmem.contiguous_memory_size=-5"));
        assert!(config.kernel_args.contains("driver.sysmem.protected_memory_size=-12"));
    }

    #[test]
    fn test_sysmem_contiguous_memory_size_percentage_too_high() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config = PlatformKernelConfig {
            sysmem_contiguous_memory_size: Some(MemorySize::Relative(100)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);

        let error_message = format!("{:#}", result.unwrap_err());
        assert!(
            error_message.contains("sysmem_contiguous_memory_size"),
            "Faulty message `{}`",
            error_message
        );
        assert!(error_message.contains("value (100)"));
    }

    #[test]
    fn test_sysmem_protected_memory_size_percentage_too_high() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config = PlatformKernelConfig {
            sysmem_protected_memory_size: Some(MemorySize::Relative(100)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);

        let error_message = format!("{:#}", result.unwrap_err());
        assert!(error_message.contains("sysmem_protected_memory_size"));
        assert!(error_message.contains("value (100)"));
    }
    #[test]
    fn test_sysmem_contiguous_memory_size_percentage_too_low() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config = PlatformKernelConfig {
            sysmem_contiguous_memory_size: Some(MemorySize::Relative(0)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);

        let error_message = format!("{:#}", result.unwrap_err());
        assert!(
            error_message.contains("sysmem_contiguous_memory_size"),
            "Faulty message `{}`",
            error_message
        );
        assert!(error_message.contains("value (0)"));
    }

    #[test]
    fn test_sysmem_protected_memory_size_percentage_too_low() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_kernel_config = PlatformKernelConfig {
            sysmem_protected_memory_size: Some(MemorySize::Relative(0)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);

        let error_message = format!("{:#}", result.unwrap_err());
        assert!(error_message.contains("sysmem_protected_memory_size"));
        assert!(error_message.contains("value (0)"));
    }
}
