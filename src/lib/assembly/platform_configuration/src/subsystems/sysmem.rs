// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{ensure, Context};
use assembly_config_capabilities::{Config, ConfigValueType};
use assembly_config_schema::platform_config::sysmem_config::{MemorySize, PlatformSysmemConfig};

const CONTIGUOUS_MEMORY_SIZE_FIELD: &str = "contiguous_memory_size";
const CONTIGUOUS_MEMORY_SIZE_CAPABILITY_SUFFIX: &str = "ContiguousMemorySize";
const FIXED_CONTIGUOUS_MEMORY_SIZE_CAPABILITY: &str = "fuchsia.sysmem.FixedContiguousMemorySize";
#[allow(unused)]
const PERCENT_CONTIGUOUS_MEMORY_SIZE_CAPABILITY: &str =
    "fuchsia.sysmem.PercentContiguousMemorySize";

const PROTECTED_MEMORY_SIZE_FIELD: &str = "protected_memory_size";
const PROTECTED_MEMORY_SIZE_CAPABILITY_SUFFIX: &str = "ProtectedMemorySize";
const FIXED_PROTECTED_MEMORY_SIZE_CAPABILITY: &str = "fuchsia.sysmem.FixedProtectedMemorySize";
#[allow(unused)]
const PERCENT_PROTECTED_MEMORY_SIZE_CAPABILITY: &str = "fuchsia.sysmem.PercentProtectedMemorySize";

const CONTIGUOUS_GUARD_PAGES_UNUSED_CAPABILITY: &str = "fuchsia.sysmem.ContiguousGuardPagesUnused";

const FIXED_CAPABILITY: &str = "Fixed";
const PERCENT_CAPABILITY: &str = "Percent";
const CAPABILITY_PREFIX: &str = "fuchsia.sysmem.";

pub(crate) struct SysmemConfig;
impl DefineSubsystemConfiguration<PlatformSysmemConfig> for SysmemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        platform_sysmem_config: &PlatformSysmemConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // All the fields are Option<T>. If a field is still None after overrides from
        // board_info and platform config have been applied, the config capability will be
        // absent and the default value of the field will be as specified in
        // src/devices/sysmem/drivers/sysmem/BUILD.gn.
        let settings = vec![&context.board_info.platform.sysmem_defaults, platform_sysmem_config]
            .iter()
            .fold(PlatformSysmemConfig::default(), apply_overrides);

        if *context.feature_set_level == FeatureSupportLevel::Embeddable {
            // At least for now, Embeddable --> zero contiguous or protected memory reservations,
            // ignoring PlatformSysmemConfig::contiguous_memory_size and protected_memory_size.
            builder.set_config_capability(
                FIXED_CONTIGUOUS_MEMORY_SIZE_CAPABILITY,
                Config::new(ConfigValueType::Int64, 0.into()),
            )?;
            builder.set_config_capability(
                PERCENT_CONTIGUOUS_MEMORY_SIZE_CAPABILITY,
                Config::new_void(),
            )?;
            builder.set_config_capability(
                FIXED_PROTECTED_MEMORY_SIZE_CAPABILITY,
                Config::new(ConfigValueType::Int64, 0.into()),
            )?;
            builder.set_config_capability(
                PERCENT_PROTECTED_MEMORY_SIZE_CAPABILITY,
                Config::new_void(),
            )?;
        } else {
            let mut apply_memory_size = |field_reference: &Option<MemorySize>,
                                         field_name: &str,
                                         config_capability_name_suffix: &str|
             -> anyhow::Result<()> {
                let fixed_capability = format!(
                    "{}{}{}",
                    CAPABILITY_PREFIX, FIXED_CAPABILITY, config_capability_name_suffix
                );
                let percent_capability = format!(
                    "{}{}{}",
                    CAPABILITY_PREFIX, PERCENT_CAPABILITY, config_capability_name_suffix
                );

                builder
                    .set_config_capability(
                        fixed_capability.as_str(),
                        if let Some(MemorySize::Fixed(fixed_memory_size)) = &field_reference {
                            Config::new(
                                ConfigValueType::Int64,
                                (*fixed_memory_size)
                                    .try_into()
                                    .with_context(|| fixed_capability.clone())?,
                            )
                        } else {
                            Config::new_void()
                        },
                    )
                    .context(fixed_capability)?;

                builder
                    .set_config_capability(
                        percent_capability.as_str(),
                        if let Some(MemorySize::Percent(percent_memory_size)) = &field_reference {
                            ensure!(
                                *percent_memory_size <= 99,
                                "{} Percent must be <= 99 - got {}",
                                field_name,
                                percent_memory_size
                            );
                            Config::new(ConfigValueType::Int32, (*percent_memory_size).into())
                        } else {
                            Config::new_void()
                        },
                    )
                    .context(percent_capability)?;

                Ok(())
            };

            // fuchsia.sysmem.FixedContiguousMemorySize or fuchsia.sysmem.PercentContiguousMemorySize
            apply_memory_size(
                &settings.contiguous_memory_size,
                CONTIGUOUS_MEMORY_SIZE_FIELD,
                CONTIGUOUS_MEMORY_SIZE_CAPABILITY_SUFFIX,
            )?;
            // fuchsia.sysmem.FixedProtectedMemorySize or fuchsia.sysmem.PercentProtectedMemorySize
            apply_memory_size(
                &settings.protected_memory_size,
                PROTECTED_MEMORY_SIZE_FIELD,
                PROTECTED_MEMORY_SIZE_CAPABILITY_SUFFIX,
            )?;
        }

        builder.set_config_capability(
            CONTIGUOUS_GUARD_PAGES_UNUSED_CAPABILITY,
            if let Some(contiguous_guard_pages_unused) = settings.contiguous_guard_pages_unused {
                Config::new(ConfigValueType::Bool, contiguous_guard_pages_unused.into())
            } else {
                Config::new_void()
            },
        )?;

        Ok(())
    }
}

fn apply_overrides(acc: PlatformSysmemConfig, x: &&PlatformSysmemConfig) -> PlatformSysmemConfig {
    // Please don't use ..Default::default() here; we want this to fail to build when new field(s)
    // are added.
    PlatformSysmemConfig {
        contiguous_memory_size: x.contiguous_memory_size.or(acc.contiguous_memory_size),
        protected_memory_size: x.protected_memory_size.or(acc.protected_memory_size),
        contiguous_guard_pages_unused: x
            .contiguous_guard_pages_unused
            .or(acc.contiguous_guard_pages_unused),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::subsystems::ConfigurationBuilderImpl;
    use assembly_config_schema::BoardInformation;
    use serde_json::{Number, Value};

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
        let platform_sysmem_config = PlatformSysmemConfig {
            contiguous_memory_size: Some(MemorySize::Fixed(123)),
            protected_memory_size: Some(MemorySize::Fixed(456)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            SysmemConfig::define_configuration(&context, &platform_sysmem_config, &mut builder);
        assert!(result.is_ok());
        let config = builder.build();
        assert_eq!(
            config.configuration_capabilities[FIXED_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Number(Number::from(123))
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[FIXED_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Number(Number::from(456))
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[CONTIGUOUS_GUARD_PAGES_UNUSED_CAPABILITY].value(),
            Value::Null
        );
    }

    #[test]
    fn test_contiguous_memory_size_percent() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_sysmem_config = PlatformSysmemConfig {
            contiguous_memory_size: Some(MemorySize::Percent(5)),
            protected_memory_size: Some(MemorySize::Percent(12)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            SysmemConfig::define_configuration(&context, &platform_sysmem_config, &mut builder);
        assert!(result.is_ok());
        let config = builder.build();
        assert_eq!(
            config.configuration_capabilities[FIXED_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Number(Number::from(5))
        );
        assert_eq!(
            config.configuration_capabilities[FIXED_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Number(Number::from(12))
        );
        assert_eq!(
            config.configuration_capabilities[CONTIGUOUS_GUARD_PAGES_UNUSED_CAPABILITY].value(),
            Value::Null
        );
    }

    #[test]
    fn test_contiguous_memory_size_percentage_too_high() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_sysmem_config = PlatformSysmemConfig {
            contiguous_memory_size: Some(MemorySize::Percent(100)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            SysmemConfig::define_configuration(&context, &platform_sysmem_config, &mut builder);

        let error_message = format!("{:#}", result.unwrap_err());
        assert!(
            error_message.contains("contiguous_memory_size"),
            "Faulty message `{}`",
            error_message
        );
        assert!(error_message.contains("got 100"));
    }

    #[test]
    fn test_protected_memory_size_percentage_too_high() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            ramdisk_image: false,
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_sysmem_config = PlatformSysmemConfig {
            protected_memory_size: Some(MemorySize::Percent(100)),
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            SysmemConfig::define_configuration(&context, &platform_sysmem_config, &mut builder);

        let error_message = format!("{:#}", result.unwrap_err());
        assert!(
            error_message.contains("protected_memory_size"),
            "Faulty message `{}`",
            error_message
        );
        assert!(error_message.contains("got 100"));
    }

    #[test]
    fn test_board_defaults_no_platform_overrides() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &BoardInformation {
                platform: assembly_config_schema::board_config::PlatformConfig {
                    sysmem_defaults: PlatformSysmemConfig {
                        contiguous_memory_size: Some(MemorySize::Fixed(123)),
                        protected_memory_size: Some(MemorySize::Fixed(456)),
                        contiguous_guard_pages_unused: Some(true),
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            ramdisk_image: Default::default(),
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_sysmem_config = PlatformSysmemConfig {
            contiguous_memory_size: None,
            protected_memory_size: None,
            contiguous_guard_pages_unused: None,
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            SysmemConfig::define_configuration(&context, &platform_sysmem_config, &mut builder);
        assert!(result.is_ok());
        let config = builder.build();
        assert_eq!(
            config.configuration_capabilities[FIXED_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Number(Number::from(123))
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[FIXED_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Number(Number::from(456))
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[CONTIGUOUS_GUARD_PAGES_UNUSED_CAPABILITY].value(),
            Value::Bool(true)
        );
    }

    #[test]
    fn test_board_defaults_with_platform_overrides() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &BoardInformation {
                platform: assembly_config_schema::board_config::PlatformConfig {
                    sysmem_defaults: PlatformSysmemConfig {
                        contiguous_memory_size: Some(MemorySize::Fixed(123)),
                        protected_memory_size: Some(MemorySize::Fixed(456)),
                        contiguous_guard_pages_unused: Some(true),
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            ramdisk_image: Default::default(),
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_sysmem_config = PlatformSysmemConfig {
            contiguous_memory_size: Some(MemorySize::Percent(12)),
            protected_memory_size: Some(MemorySize::Percent(34)),
            contiguous_guard_pages_unused: Some(false),
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            SysmemConfig::define_configuration(&context, &platform_sysmem_config, &mut builder);
        assert!(result.is_ok());
        let config = builder.build();
        assert_eq!(
            config.configuration_capabilities[FIXED_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Number(Number::from(12))
        );
        assert_eq!(
            config.configuration_capabilities[FIXED_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Number(Number::from(34))
        );
        assert_eq!(
            config.configuration_capabilities[CONTIGUOUS_GUARD_PAGES_UNUSED_CAPABILITY].value(),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_no_overrides() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &BoardInformation {
                platform: assembly_config_schema::board_config::PlatformConfig {
                    sysmem_defaults: PlatformSysmemConfig {
                        contiguous_memory_size: None,
                        protected_memory_size: None,
                        contiguous_guard_pages_unused: None,
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
            ramdisk_image: Default::default(),
            gendir: Default::default(),
            resource_dir: Default::default(),
        };
        let platform_sysmem_config = PlatformSysmemConfig {
            contiguous_memory_size: None,
            protected_memory_size: None,
            contiguous_guard_pages_unused: None,
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            SysmemConfig::define_configuration(&context, &platform_sysmem_config, &mut builder);
        assert!(result.is_ok());
        let config = builder.build();
        assert_eq!(
            config.configuration_capabilities[FIXED_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_CONTIGUOUS_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[FIXED_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[PERCENT_PROTECTED_MEMORY_SIZE_CAPABILITY].value(),
            Value::Null
        );
        assert_eq!(
            config.configuration_capabilities[CONTIGUOUS_GUARD_PAGES_UNUSED_CAPABILITY].value(),
            Value::Null
        );
    }
}
