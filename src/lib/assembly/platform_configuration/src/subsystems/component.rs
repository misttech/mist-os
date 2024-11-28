// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{Context, Result};
use assembly_config_schema::platform_config::development_support_config::DevelopmentSupportConfig;
use assembly_config_schema::platform_config::health_check_config::HealthCheckConfig;
use assembly_config_schema::platform_config::starnix_config::PlatformStarnixConfig;
use assembly_config_schema::product_config::ComponentPolicyConfig;
use assembly_constants::{BootfsDestination, FileEntry};
use camino::Utf8PathBuf;
use component_manager_config::{compile, Args};
use std::fs::File;
use std::path::PathBuf;

pub(crate) struct ComponentConfig<'a> {
    pub policy: &'a ComponentPolicyConfig,
    pub development_support: &'a DevelopmentSupportConfig,
    pub starnix: &'a PlatformStarnixConfig,
    pub health_check: &'a HealthCheckConfig,
}

pub(crate) struct ComponentSubsystem;
impl DefineSubsystemConfiguration<ComponentConfig<'_>> for ComponentSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &ComponentConfig<'_>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let gendir = context.get_gendir().context("Getting gendir for component subsystem")?;

        // Add base policies.
        let mut input = vec![
            context.get_resource("component_manager_policy_base.json5"),
            context.get_resource("component_manager_policy_build_type_base.json5"),
            context.get_resource("bootfs_config.json5"),
        ];

        // Apply platform policies specific to subsystems.
        if config.starnix.enabled {
            input.push(context.get_resource("component_manager_policy_starnix.json5"));
        }

        let monikers: Vec<&str> = config
            .health_check
            .verify_components
            .iter()
            .map(|verify_component| verify_component.source_moniker())
            .collect();

        let write_config = |name: &str, value: serde_json::Value| -> Result<Utf8PathBuf> {
            let path = gendir.join(name);
            let file = File::create(&path).with_context(|| format!("Creating config: {}", name))?;
            serde_json::to_writer_pretty(file, &value)
                .with_context(|| format!("Writing config: {}", name))?;
            Ok(path)
        };
        let health_checks_source = write_config(
            "ota_health_check_config.json",
            serde_json::json!(
            {
                "health_check" : {
                    "monikers": monikers,
                },
            }
            ),
        )?;

        input.push(health_checks_source);

        // Collect the platform policies based on build-type.
        match (context.build_type, config.development_support.include_sl4f) {
            // The eng policies are given to Eng and UserDebug builds that also include sl4f.
            (BuildType::Eng, _) | (BuildType::UserDebug, true) => {
                input.push(context.get_resource("component_manager_policy.json5"));
                input.push(context.get_resource("component_manager_policy_eng.json5"));
            }
            (BuildType::UserDebug, false) => {
                input.push(context.get_resource("component_manager_policy_userdebug.json5"));
            }
            (BuildType::User, _) => {
                input.push(context.get_resource("component_manager_policy_user.json5"));
            }
        }

        let input = input.into_iter().map(PathBuf::from).collect();

        // Collect the product policies.
        let product = config
            .policy
            .product_policies
            .iter()
            .map(|p| PathBuf::from(p.as_utf8_pathbuf()))
            .collect();

        // Compile the final policy config file.
        let config = gendir.join("config.json5");
        let output = config.clone().into();
        let args = Args { input, product, output };
        compile(args).context("Compiling the component_manager config")?;

        // Add the policy to the system.
        builder
            .bootfs()
            .file(FileEntry {
                source: config,
                destination: BootfsDestination::ComponentManagerConfig,
            })
            .context("Adding component_manager config")?;
        Ok(())
    }
}
