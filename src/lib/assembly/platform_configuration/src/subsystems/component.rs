// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::Context;
use assembly_config_schema::{product_config::ComponentPolicyConfig, FileEntry};
use component_manager_config::{compile, Args};
use std::path::PathBuf;

pub(crate) struct ComponentSubsystem;
impl DefineSubsystemConfiguration<Option<ComponentPolicyConfig>> for ComponentSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        policy: &Option<ComponentPolicyConfig>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if let Some(policy) = &policy {
            let gendir = context.get_gendir().context("Getting gendir for component subsystem")?;

            // Collect the platform policies based on build-type.
            let mut input = vec![
                context.get_resource("component_manager_policy_base.json5"),
                context.get_resource("component_manager_policy_build_type_base.json5"),
                context.get_resource("bootfs_config.json5"),
            ];
            match context.build_type {
                BuildType::Eng => {
                    input.push(context.get_resource("component_manager_policy.json5"));
                    input.push(context.get_resource("component_manager_policy_eng.json5"));
                }
                BuildType::UserDebug => {
                    input.push(context.get_resource("component_manager_policy_userdebug.json5"));
                }
                BuildType::User => {
                    input.push(context.get_resource("component_manager_policy_user.json5"));
                }
            }
            let input = input.into_iter().map(PathBuf::from).collect();

            // Collect the product policies.
            let product = policy.product_policies.iter().map(PathBuf::from).collect();

            // Compile the final policy config file.
            let config = gendir.join("config.json5");
            let output = config.clone().into();
            let args = Args { input, product, output };
            compile(args).context("Compiling the component_manager config")?;

            // Add the policy to the system.
            builder
                .bootfs()
                .file(FileEntry { source: config, destination: "config/component_manager".into() })
                .context("Adding component_manager config")?;
        }
        Ok(())
    }
}
