// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::Result;
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
    pub level_control: fbroker::LevelControlProxy,
    active_dependency_token: fbroker::DependencyToken,
}

impl PowerElementContext {
    pub async fn new(
        topology: &fbroker::TopologyProxy,
        element_name: &str,
        initial_current_level: fbroker::PowerLevel,
        valid_levels: Vec<fbroker::PowerLevel>,
        dependencies: Vec<fbroker::LevelDependency>,
        mut active_dependency_tokens_to_register: Vec<fbroker::DependencyToken>,
    ) -> Result<Self> {
        let active_dependency_token = fbroker::DependencyToken::create();
        active_dependency_tokens_to_register.push(
            active_dependency_token
                .duplicate_handle(Rights::SAME_RIGHTS)
                .expect("failed to duplicate token"),
        );

        let (element_control_client_end, lessor_client_end, level_control_client_end) = topology
            .add_element(
                element_name,
                initial_current_level,
                &valid_levels,
                dependencies,
                active_dependency_tokens_to_register,
                vec![],
            )
            .await?
            .map_err(|d| anyhow::anyhow!("{d:?}"))?;

        Ok(Self {
            element_control: element_control_client_end.into_proxy()?,
            lessor: lessor_client_end.into_proxy()?,
            level_control: level_control_client_end.into_proxy()?,
            active_dependency_token,
        })
    }

    pub fn active_dependency_token(&self) -> fbroker::DependencyToken {
        self.active_dependency_token
            .duplicate_handle(Rights::SAME_RIGHTS)
            .expect("failed to duplicate token")
    }
}
