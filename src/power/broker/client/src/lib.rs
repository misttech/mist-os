// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::Result;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_power_broker as fbroker;
use fuchsia_zircon::{HandleBased, Rights};

/// A well-known set of PowerLevels to be specified as the valid_levels for a
/// power element. This is the set of levels in fbroker::BinaryPowerLevel.
pub const BINARY_POWER_LEVELS: [fbroker::PowerLevel; 2] = [
    fbroker::BinaryPowerLevel::Off.into_primitive(),
    fbroker::BinaryPowerLevel::On.into_primitive(),
];

pub struct PowerElementContext {
    pub element_control: fbroker::ElementControlProxy,
    pub lessor: fbroker::LessorProxy,
    pub required_level: fbroker::RequiredLevelProxy,
    pub current_level: fbroker::CurrentLevelProxy,
    active_dependency_token: fbroker::DependencyToken,
    passive_dependency_token: fbroker::DependencyToken,
    name: String,
}

impl PowerElementContext {
    pub fn builder<'a>(
        topology: &'a fbroker::TopologyProxy,
        element_name: &'a str,
        valid_levels: &'a [fbroker::PowerLevel],
    ) -> PowerElementContextBuilder<'a> {
        PowerElementContextBuilder::new(topology, element_name, valid_levels)
    }

    pub fn active_dependency_token(&self) -> fbroker::DependencyToken {
        self.active_dependency_token
            .duplicate_handle(Rights::SAME_RIGHTS)
            .expect("failed to duplicate token")
    }

    pub fn passive_dependency_token(&self) -> fbroker::DependencyToken {
        self.passive_dependency_token
            .duplicate_handle(Rights::SAME_RIGHTS)
            .expect("failed to duplicate token")
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct PowerElementContextBuilder<'a> {
    topology: &'a fbroker::TopologyProxy,
    element_name: &'a str,
    initial_current_level: fbroker::PowerLevel,
    valid_levels: &'a [fbroker::PowerLevel],
    dependencies: Vec<fbroker::LevelDependency>,
    active_dependency_tokens_to_register: Vec<fbroker::DependencyToken>,
    passive_dependency_tokens_to_register: Vec<fbroker::DependencyToken>,
}

impl<'a> PowerElementContextBuilder<'a> {
    pub fn new(
        topology: &'a fbroker::TopologyProxy,
        element_name: &'a str,
        valid_levels: &'a [fbroker::PowerLevel],
    ) -> Self {
        Self {
            topology,
            element_name,
            valid_levels,
            initial_current_level: Default::default(),
            dependencies: Default::default(),
            active_dependency_tokens_to_register: Default::default(),
            passive_dependency_tokens_to_register: Default::default(),
        }
    }

    pub fn initial_current_level(mut self, value: fbroker::PowerLevel) -> Self {
        self.initial_current_level = value;
        self
    }

    pub fn dependencies(mut self, value: Vec<fbroker::LevelDependency>) -> Self {
        self.dependencies = value;
        self
    }

    pub fn active_dependency_tokens_to_register(
        mut self,
        value: Vec<fbroker::DependencyToken>,
    ) -> Self {
        self.active_dependency_tokens_to_register = value;
        self
    }

    pub fn passive_dependency_tokens_to_register(
        mut self,
        value: Vec<fbroker::DependencyToken>,
    ) -> Self {
        self.passive_dependency_tokens_to_register = value;
        self
    }

    pub async fn build(mut self) -> Result<PowerElementContext> {
        let active_dependency_token = fbroker::DependencyToken::create();
        self.active_dependency_tokens_to_register.push(
            active_dependency_token
                .duplicate_handle(Rights::SAME_RIGHTS)
                .expect("failed to duplicate token"),
        );

        let passive_dependency_token = fbroker::DependencyToken::create();
        self.passive_dependency_tokens_to_register.push(
            passive_dependency_token
                .duplicate_handle(Rights::SAME_RIGHTS)
                .expect("failed to duplicate token"),
        );

        let (current_level, current_level_server_end) =
            create_proxy::<fbroker::CurrentLevelMarker>()?;
        let (required_level, required_level_server_end) =
            create_proxy::<fbroker::RequiredLevelMarker>()?;
        let (element_control_client_end, lessor_client_end) = self
            .topology
            .add_element(fbroker::ElementSchema {
                element_name: Some(self.element_name.into()),
                initial_current_level: Some(self.initial_current_level),
                valid_levels: Some(self.valid_levels.to_vec()),
                dependencies: Some(self.dependencies),
                active_dependency_tokens_to_register: Some(
                    self.active_dependency_tokens_to_register,
                ),
                passive_dependency_tokens_to_register: Some(
                    self.passive_dependency_tokens_to_register,
                ),
                level_control_channels: Some(fbroker::LevelControlChannels {
                    current: current_level_server_end,
                    required: required_level_server_end,
                }),
                ..Default::default()
            })
            .await?
            .map_err(|d| anyhow::anyhow!("{d:?}"))?;
        let element_control = element_control_client_end.into_proxy()?;
        Ok(PowerElementContext {
            element_control,
            lessor: lessor_client_end.into_proxy()?,
            required_level,
            current_level,
            active_dependency_token,
            passive_dependency_token,
            name: self.element_name.to_string(),
        })
    }
}
