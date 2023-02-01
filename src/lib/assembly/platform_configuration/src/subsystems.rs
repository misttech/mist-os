// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context};
use assembly_config_schema::{
    AssemblyConfig, BoardInformation, BuildType, ExampleConfig, FeatureSupportLevel,
};

use crate::common::{CompletedConfiguration, ConfigurationBuilderImpl};

pub(crate) mod prelude {

    #[allow(unused)]
    pub(crate) use crate::common::{
        BoardInformationExt, ComponentConfigBuilderExt, ConfigurationBuilder, ConfigurationContext,
        DefaultByBuildType, DefineSubsystemConfiguration, OptionDefaultByBuildTypeExt,
    };
}

use prelude::*;

mod connectivity;
mod console;
mod development;
mod diagnostics;
mod example;
mod identity;
mod input;
mod session;
mod starnix;
mod storage;
mod swd;
mod virtualization;

/// ffx config flag for enabling configuring the assembly+structured config example.
const EXAMPLE_ENABLED_FLAG: &str = "assembly_example_enabled";

struct CommonBundles;
impl DefineSubsystemConfiguration<()> for CommonBundles {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Set up the platform's common AIBs by feature_set_level and build_type.
        for bundle_name in match (context.feature_set_level, context.build_type) {
            (FeatureSupportLevel::Bringup, BuildType::Eng) => {
                vec!["common_bringup"]
            }
            (FeatureSupportLevel::Minimal, BuildType::Eng) => {
                vec![
                    "common_bringup",
                    "common_minimal",
                    "common_minimal_eng",
                    "common_minimal_userdebug",
                ]
            }
            (FeatureSupportLevel::Bringup, BuildType::UserDebug) => {
                vec!["common_bringup"]
            }
            (FeatureSupportLevel::Minimal, BuildType::UserDebug) => {
                vec!["common_bringup", "common_minimal", "common_minimal_userdebug"]
            }
            (FeatureSupportLevel::Bringup, BuildType::User) => {
                vec!["common_bringup"]
            }
            (FeatureSupportLevel::Minimal, BuildType::User) => {
                vec!["common_bringup", "common_minimal"]
            }
            _ => vec![],
        } {
            builder.platform_bundle(bundle_name);
        }
        builder.platform_bundle("emulator_support");

        Ok(())
    }
}

/// Convert the high-level description of product configuration into a series of configuration
/// value files with concrete package/component tuples.
///
/// Returns a map from package names to configuration updates.
pub fn define_configuration(
    config: &AssemblyConfig,
    board_info: Option<&BoardInformation>,
) -> anyhow::Result<CompletedConfiguration> {
    let mut builder = ConfigurationBuilderImpl::default();

    let feature_set_level = &config.platform.feature_set_level;
    let build_type = &config.platform.build_type;

    // Set up the context that's used by each subsystem to get the generally-
    // available platform information.
    let context = ConfigurationContext { feature_set_level, build_type, board_info };

    // Define the common platform bundles for this platform configuration.
    CommonBundles::define_configuration(&context, &(), &mut builder)
        .context("Selecting the common platform assembly input bundles")?;

    // Configure the Product Assembly + Structured Config example, if enabled.
    if should_configure_example() {
        example::ExampleSubsystemConfig::define_configuration(
            &context,
            &config.platform.example_config,
            &mut builder,
        )?;
    } else if config.platform.example_config != ExampleConfig::default() {
        bail!("Config options were set for the example subsystem, but the example is not enabled to be configured.");
    }

    // The real platform subsystems

    connectivity::ConnectivitySubsystemConfig::define_configuration(
        &context,
        &config.platform.connectivity,
        &mut builder,
    )
    .context("Configuring the 'connectivity' subsystem")?;

    console::ConsoleSubsystemConfig::define_configuration(
        &context,
        &config.platform.additional_serial_log_tags,
        &mut builder,
    )
    .context("Configuring the 'console' subsystem")?;

    identity::IdentitySubsystemConfig::define_configuration(
        &context,
        &config.platform.identity,
        &mut builder,
    )
    .context("Configuring the 'identity' subsystem")?;

    input::InputSubsystemConfig::define_configuration(
        &context,
        &config.platform.input,
        &mut builder,
    )
    .context("Configuring the 'input' subsystem")?;

    storage::StorageSubsystemConfig::define_configuration(
        &context,
        &config.platform.storage,
        &mut builder,
    )
    .context("Configuring the 'storage' subsystem")?;

    session::SessionConfig::define_configuration(
        &context,
        &config.product.session_url,
        &mut builder,
    )
    .context("Configuring the 'session' subsystem")?;

    development::DevelopmentConfig::define_configuration(
        &context,
        &config.platform.development_support,
        &mut builder,
    )
    .context("Configuring the 'development' subsystem")?;

    diagnostics::DiagnosticsSubsystem::define_configuration(
        &context,
        &config.platform.diagnostics,
        &mut builder,
    )
    .context("Configuring the 'diagnostics' subsystem")?;

    starnix::StarnixSubsystem::define_configuration(
        &context,
        &config.platform.starnix,
        &mut builder,
    )
    .context("Configuring the starnix subsystem")?;

    virtualization::VirtualizationSubsystem::define_configuration(
        &context,
        &config.platform.virtualization,
        &mut builder,
    )
    .context("Configuring the 'virtualization' subsystem")?;

    Ok(builder.build())
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
        let result = define_configuration(&config, Option::None);

        assert!(result.is_err());
    }
}
