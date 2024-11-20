// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{anyhow, Context};
use assembly_config_schema::board_config::SerialMode;
use assembly_config_schema::platform_config::kernel_config::{
    MemoryReclamationStrategy, OOMBehavior, OOMRebootTimeout, PagetableEvictionPolicy,
    PlatformKernelConfig, ZeroPageScanCount,
};
use assembly_constants::{BootfsDestination, FileEntry};
use camino::Utf8PathBuf;
pub(crate) struct KernelSubsystem;

impl DefineSubsystemConfiguration<PlatformKernelConfig> for KernelSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        kernel_config: &PlatformKernelConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        match (&context.build_type, &kernel_config.oom_behavior) {
            (_, OOMBehavior::Reboot { timeout: OOMRebootTimeout::Normal }) => {}
            (&BuildType::Eng, OOMBehavior::Reboot { timeout: OOMRebootTimeout::Low }) => {
                builder.platform_bundle("kernel_oom_reboot_timeout_low")
            }
            (&BuildType::Eng, OOMBehavior::JobKill) => {
                builder.platform_bundle("kernel_oom_behavior_jobkill")
            }
            (&BuildType::Eng, OOMBehavior::Disable) => {
                builder.platform_bundle("kernel_oom_behavior_disable")
            }
            (&BuildType::UserDebug | &BuildType::User, _) => {
                anyhow::bail!("'kernel.oom_behavior' can only be set on 'build_type=\"eng\"");
            }
        }
        match (&context.board_info.kernel.serial_mode, &context.build_type) {
            (SerialMode::NoOutput, _) => {}
            (SerialMode::Legacy, &BuildType::UserDebug | &BuildType::User) => {
                println!("Serial cannot be enabled on user or userdebug builds. Not enabling.");
            }
            (SerialMode::Legacy, &BuildType::Eng) => {
                builder.platform_bundle("kernel_serial_legacy")
            }
        }
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
            && context.board_info.provides_feature("fuchsia::pmm_checker_auto")
        {
            anyhow::bail!("Board provides conflicting features of 'fuchsia::pmm_checker' and 'fuchsia::pmm_checker_auto'");
        }
        if context.board_info.provides_feature("fuchsia::pmm_checker")
            && context.build_type == &BuildType::Eng
        {
            builder.platform_bundle("kernel_pmm_checker_enabled");
        } else if context.board_info.provides_feature("fuchsia::pmm_checker_auto")
            && context.build_type == &BuildType::Eng
        {
            builder.platform_bundle("kernel_pmm_checker_enabled_auto");
        }

        if context.board_info.kernel.contiguous_physical_pages {
            builder.platform_bundle("kernel_contiguous_physical_pages");
        }

        if context.board_info.kernel.scheduler_prefer_little_cpus {
            builder.kernel_arg("kernel.scheduler.prefer-little-cpus=true".to_owned());
        }

        if context.board_info.kernel.quiet_early_boot {
            anyhow::ensure!(
                context.build_type == &BuildType::Eng,
                "'quiet_early_boot' can only be enabled in 'eng' builds"
            );
            builder.kernel_arg("kernel.phys.verbose=false".to_owned())
        }

        if let Some(serial) = &context.board_info.kernel.serial {
            anyhow::ensure!(
                context.build_type == &BuildType::Eng,
                "'kernel.serial' can only be enabled in 'eng' builds"
            );
            let arg = format!("kernel.serial={}", serial);
            builder.kernel_arg(arg);
        }

        if let Some(oom) = &context.board_info.kernel.oom {
            if oom.evict_at_warning {
                builder.kernel_arg("kernel.oom.evict-at-warning=true".to_owned());
            }
            if oom.evict_continuous {
                builder.kernel_arg("kernel.oom.evict-continuous=true".to_owned());
            }
            if let Some(outofmemory_mb) = oom.out_of_memory_mb {
                let arg = format!("kernel.oom.outofmemory-mb={}", outofmemory_mb);
                builder.kernel_arg(arg);
            }
            if let Some(critical_mb) = oom.critical_mb {
                let arg = format!("kernel.oom.critical-mb={}", critical_mb);
                builder.kernel_arg(arg);
            }
            if let Some(warning_mb) = oom.warning_mb {
                let arg = format!("kernel.oom.warning-mb={}", warning_mb);
                builder.kernel_arg(arg);
            }
        }

        match kernel_config.memory_reclamation_strategy {
            MemoryReclamationStrategy::Balanced => {
                // Use the kernel defaults.
            }
            MemoryReclamationStrategy::Eager => {
                builder.platform_bundle("kernel_page_scanner_aging_fast");
            }
        }

        if context.board_info.kernel.halt_on_panic {
            anyhow::ensure!(
                context.build_type == &BuildType::Eng,
                "'kernel.halt-on-panic' can only be enabled in 'eng' builds"
            );
            builder.kernel_arg("kernel.halt-on-panic=true".to_owned())
        }

        if let Some(page_scanner) = &kernel_config.page_scanner {
            match page_scanner.page_table_eviction_policy {
                PagetableEvictionPolicy::Never => {
                    builder.platform_bundle("kernel_page_table_eviction_never")
                }
                PagetableEvictionPolicy::OnRequest => {
                    builder.platform_bundle("kernel_page_table_eviction_on_request")
                }
                PagetableEvictionPolicy::Always => {}
            }

            if page_scanner.disable_at_boot {
                builder.kernel_arg("kernel.page-scanner.start-at-boot=false".to_owned());
            }

            if page_scanner.disable_eviction {
                builder.kernel_arg("kernel.page-scanner.enable-eviction=false".to_owned());
            }

            let scan_count: u64 = match page_scanner.zero_page_scans_per_second {
                ZeroPageScanCount::Default => 20000,
                ZeroPageScanCount::NoScans => 0,
                ZeroPageScanCount::PerSecond(x) => x,
            };

            let arg = format!("kernel.page-scanner.zero-page-scans-per-second={}", scan_count);
            builder.kernel_arg(arg);
        }

        if let Some(aslr_entropy_bits) = kernel_config.aslr_entropy_bits {
            let kernel_arg = format!("aslr.entropy_bits={}", aslr_entropy_bits);
            builder.kernel_arg(kernel_arg);
        }

        if let Some(memory_limit_mb) = kernel_config.memory_limit_mb {
            let kernel_arg = format!("kernel.memory-limit-mb={}", memory_limit_mb);
            builder.kernel_arg(kernel_arg);
        }

        for thread_roles_file in &context.board_info.configuration.thread_roles {
            let filename = thread_roles_file
                .as_utf8_pathbuf()
                .file_name()
                .ok_or_else(|| {
                    anyhow!("Thread roles file doesn't have a filename: {}", thread_roles_file)
                })?
                .to_owned();
            builder
                .bootfs()
                .file(FileEntry {
                    source: Utf8PathBuf::from(thread_roles_file.clone()),
                    destination: BootfsDestination::ThreadRoles(filename),
                })
                .with_context(|| format!("Adding thread roles file: {}", thread_roles_file))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::subsystems::ConfigurationBuilderImpl;
    use crate::CompletedConfiguration;

    fn build_with_platform_kernel_config(
        platform_kernel_config: PlatformKernelConfig,
    ) -> CompletedConfiguration {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            gendir: Default::default(),
            resource_dir: Default::default(),
            developer_only_options: Default::default(),
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            KernelSubsystem::define_configuration(&context, &platform_kernel_config, &mut builder);
        assert!(result.is_ok());
        builder.build()
    }

    #[test]
    fn test_define_configuration() {
        let completed_config = build_with_platform_kernel_config(Default::default());
        assert!(completed_config.kernel_args.is_empty());
    }

    #[test]
    fn test_define_configuration_aslr() {
        let completed_config = build_with_platform_kernel_config(PlatformKernelConfig {
            aslr_entropy_bits: Some(12),
            ..Default::default()
        });
        assert!(completed_config.kernel_args.contains("aslr.entropy_bits=12"));
    }

    #[test]
    fn test_define_configuration_no_aslr() {
        let completed_config = build_with_platform_kernel_config(Default::default());
        assert!(completed_config.kernel_args.is_empty());
    }

    #[test]
    fn test_define_memory_limit() {
        let completed_config = build_with_platform_kernel_config(PlatformKernelConfig {
            memory_limit_mb: Some(12),
            ..Default::default()
        });
        assert!(completed_config.kernel_args.contains("kernel.memory-limit-mb=12"));
    }
}
