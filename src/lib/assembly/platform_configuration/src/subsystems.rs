// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context};
use assembly_config_schema::{AssemblyConfig, BoardInformation, BuildType, ExampleConfig};

use crate::common::{CompletedConfiguration, ConfigurationBuilderImpl};

pub(crate) mod prelude {

    #[allow(unused)]
    pub(crate) use crate::common::{
        BoardInformationExt, ComponentConfigBuilderExt, ConfigurationBuilder, ConfigurationContext,
        DefaultByBuildType, DefineSubsystemConfiguration, FeatureSupportLevel,
        OptionDefaultByBuildTypeExt,
    };

    #[allow(unused)]
    pub(crate) use assembly_config_schema::BuildType;
}

use prelude::*;

mod battery;
mod connectivity;
mod development;
mod diagnostics;
mod driver_framework;
mod example;
mod fonts;
mod forensics;
mod graphics;
mod hwinfo;
mod identity;
mod input;
mod intl;
mod media;
mod radar;
mod rcs;
mod session;
mod setui;
mod starnix;
mod storage;
mod swd;
mod thermal;
mod ui;
mod virtualization;

/// ffx config flag for enabling configuring the assembly+structured config example.
const EXAMPLE_ENABLED_FLAG: &str = "assembly_example_enabled";

/// Convert the high-level description of product configuration into a series of configuration
/// value files with concrete package/component tuples.
///
/// Returns a map from package names to configuration updates.
pub fn define_configuration(
    config: &AssemblyConfig,
    board_info: &BoardInformation,
) -> anyhow::Result<CompletedConfiguration> {
    let icu_config = &config.platform.icu;
    let mut builder = ConfigurationBuilderImpl::new(icu_config);

    // The emulator support bundle is always added, even to an empty build.
    builder.platform_bundle("emulator_support");

    let feature_set_level =
        FeatureSupportLevel::from_deserialized(&config.platform.feature_set_level);

    // Only perform configuration if the feature_set_level is not None (ie, Empty).
    if let Some(feature_set_level) = &feature_set_level {
        let build_type = &config.platform.build_type;

        // Set up the context that's used by each subsystem to get the generally-
        // available platform information.
        let context =
            ConfigurationContext { feature_set_level, build_type, board_info: board_info };

        // Call the configuration functions for each subsystem.
        configure_subsystems(&context, config, &mut builder)?;
    }

    Ok(builder.build())
}

struct CommonBundles;
impl DefineSubsystemConfiguration<()> for CommonBundles {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Set up the platform's common AIBs by feature_set_level and build_type.
        for bundle_name in match (context.feature_set_level, context.build_type) {
            (FeatureSupportLevel::Bootstrap, _) => {
                vec!["bootstrap", "bootstrap_userdebug", "bootstrap_eng"]
            }
            (FeatureSupportLevel::Utility, BuildType::Eng) => {
                vec![
                    "bootstrap",
                    "bootstrap_userdebug",
                    "bootstrap_eng",
                    "core_realm",
                    "core_realm_development_access",
                    "core_realm_eng",
                ]
            }
            (FeatureSupportLevel::Utility, BuildType::UserDebug) => {
                vec![
                    "bootstrap",
                    "bootstrap_userdebug",
                    "core_realm",
                    "core_realm_development_access",
                    "core_realm_user_and_userdebug",
                ]
            }
            (FeatureSupportLevel::Utility, BuildType::User) => {
                vec!["bootstrap", "core_realm", "core_realm_user_and_userdebug"]
            }
            (FeatureSupportLevel::Minimal, BuildType::Eng) => {
                vec![
                    "bootstrap",
                    "bootstrap_userdebug",
                    "bootstrap_eng",
                    "core_realm",
                    "core_realm_eng",
                    "core_realm_development_access",
                    "common_minimal",
                    "minimal_eng",
                    "minimal_userdebug_and_eng",
                    "testing_support",
                ]
            }
            (FeatureSupportLevel::Minimal, BuildType::UserDebug) => {
                vec![
                    "bootstrap",
                    "bootstrap_userdebug",
                    "core_realm",
                    "core_realm_development_access",
                    "core_realm_user_and_userdebug",
                    "common_minimal",
                    "minimal_userdebug",
                    "minimal_userdebug_and_eng",
                ]
            }
            (FeatureSupportLevel::Minimal, BuildType::User) => {
                vec![
                    "bootstrap",
                    "core_realm",
                    "core_realm_user_and_userdebug",
                    "common_minimal",
                    "minimal_user",
                ]
            }
        } {
            builder.platform_bundle(bundle_name);
        }

        Ok(())
    }
}

fn configure_subsystems(
    context: &ConfigurationContext<'_>,
    config: &AssemblyConfig,
    builder: &mut dyn ConfigurationBuilder,
) -> anyhow::Result<()> {
    // Define the common platform bundles for this platform configuration.
    CommonBundles::define_configuration(context, &(), builder)
        .context("Selecting the common platform assembly input bundles")?;

    // Configure the Product Assembly + Structured Config example, if enabled.
    if should_configure_example() {
        example::ExampleSubsystemConfig::define_configuration(
            context,
            &config.platform.example_config,
            builder,
        )?;
    } else if config.platform.example_config != ExampleConfig::default() {
        bail!("Config options were set for the example subsystem, but the example is not enabled to be configured.");
    }

    // The real platform subsystems

    battery::BatterySubsystemConfig::define_configuration(
        context,
        &config.platform.battery,
        builder,
    )
    .context("Configuring the 'battery' subsystem")?;

    connectivity::ConnectivitySubsystemConfig::define_configuration(
        context,
        &config.platform.connectivity,
        builder,
    )
    .context("Configuring the 'connectivity' subsystem")?;

    development::DevelopmentConfig::define_configuration(
        context,
        &config.platform.development_support,
        builder,
    )
    .context("Configuring the 'development' subsystem")?;

    diagnostics::DiagnosticsSubsystem::define_configuration(
        context,
        &config.platform.diagnostics,
        builder,
    )
    .context("Configuring the 'diagnostics' subsystem")?;

    driver_framework::DriverFrameworkSubsystemConfig::define_configuration(
        context,
        &config.platform.driver_framework,
        builder,
    )
    .context("Configuring the 'driver_framework' subsystem")?;

    graphics::GraphicsSubsystemConfig::define_configuration(
        context,
        &config.platform.graphics,
        builder,
    )
    .context("Configuring the 'graphics' subsystem")?;

    hwinfo::HwinfoSubsystem::define_configuration(context, &config.product.info, builder)
        .context("Configuring the 'hwinfo' subsystem")?;

    identity::IdentitySubsystemConfig::define_configuration(
        context,
        &config.platform.identity,
        builder,
    )
    .context("Configuring the 'identity' subsystem")?;

    input::InputSubsystemConfig::define_configuration(context, &config.platform.input, builder)
        .context("Configuring the 'input' subsystem")?;

    media::MediaSubsystem::define_configuration(context, &config.platform.media, builder)
        .context("Configuring the 'media' subsystem")?;

    radar::RadarSubsystemConfig::define_configuration(context, &(), builder)
        .context("Configuring the 'radar' subsystem")?;

    rcs::RcsSubsystemConfig::define_configuration(context, &(), builder)
        .context("Configuring the 'rcs' subsystem")?;

    session::SessionConfig::define_configuration(
        context,
        &(&config.platform.session, &config.product.session_url),
        builder,
    )
    .context("Configuring the 'session' subsystem")?;

    starnix::StarnixSubsystem::define_configuration(context, &config.platform.starnix, builder)
        .context("Configuring the starnix subsystem")?;

    storage::StorageSubsystemConfig::define_configuration(
        context,
        &config.platform.storage,
        builder,
    )
    .context("Configuring the 'storage' subsystem")?;

    swd::SwdSubsystemConfig::define_configuration(
        context,
        &config.platform.software_delivery,
        builder,
    )
    .context("Configuring the 'software_delivery' subsystem")?;

    thermal::ThermalSubsystem::define_configuration(context, &(), builder)
        .context("Configuring the 'thermal' subsystem")?;

    ui::UiSubsystem::define_configuration(context, &config.platform.ui, builder)
        .context("Configuring the 'ui' subsystem")?;

    virtualization::VirtualizationSubsystem::define_configuration(
        context,
        &config.platform.virtualization,
        builder,
    )
    .context("Configuring the 'virtualization' subsystem")?;

    fonts::FontsSubsystem::define_configuration(context, &config.platform.fonts, builder)
        .context("Configuring the 'fonts' subsystem")?;

    intl::IntlSubsystem::define_configuration(context, &config.platform.intl, builder)
        .context("Confguring the 'intl' subsystem")?;

    setui::SetUiSubsystem::define_configuration(context, &config.platform.setui, builder)
        .context("Confguring the 'SetUI' subsystem")?;

    forensics::ForensicsSubsystem::define_configuration(
        context,
        &config.platform.forensics,
        builder,
    )
    .context("Confguring the 'Forensics' subsystem")?;

    Ok(())
}

/// Check ffx config for whether we should execute example code.
fn should_configure_example() -> bool {
    futures::executor::block_on(ffx_config::get::<bool, _>(EXAMPLE_ENABLED_FLAG))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_util as util;

    #[test]
    fn test_example_config_without_configure_example_returns_err() {
        let json5 = r#"
            {
            platform: {
                build_type: "eng",
                example_config: {
                    include_example_aib: true
                }
            },
            product: {},
            }
        "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        let result = define_configuration(&config, &BoardInformation::default());

        assert!(result.is_err());
    }
}
