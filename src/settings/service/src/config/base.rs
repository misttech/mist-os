// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::agent::{AgentRegistrar, BlueprintHandle};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The flags used to control behavior of controllers.
#[derive(PartialEq, Debug, Eq, Hash, Copy, Clone, Serialize, Deserialize)]
pub enum ControllerFlag {
    /// This flag controls whether an external service is in control of the
    /// brightness configuration.
    ExternalBrightnessControl,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A load of a config file with the given information about the load.
    Load(ConfigLoadInfo),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConfigLoadInfo {
    /// The status of the load.
    pub status: ConfigLoadStatus,
    /// The contents of the loaded config file.
    pub contents: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigLoadStatus {
    /// Failed to parse the file.
    ParseFailure(String),
    /// Successfully loaded the file.
    Success,
    /// Falling back to default.
    UsingDefaults(String),
}

impl From<ConfigLoadStatus> for String {
    fn from(status: ConfigLoadStatus) -> String {
        match status {
            ConfigLoadStatus::ParseFailure(_) => "ParseFailure".to_string(),
            ConfigLoadStatus::Success => "Success".to_string(),
            ConfigLoadStatus::UsingDefaults(_) => "UsingDefaults".to_string(),
        }
    }
}

/// Represents each agent that can be run.
#[derive(Eq, PartialEq, Hash, Debug, Copy, Clone, Deserialize)]
pub enum AgentType {
    /// Responsible for watching the camera3 mute status. If other clients
    /// of the camera3 api modify the camera state, the agent should watch and
    /// should coordinate that change with the internal camera state.
    CameraWatcher,
    /// Plays earcons in response to certain events. If MediaButtons is
    /// enabled, then it will also handle some media buttons events.
    Earcons,
    /// Responsible for managing the connection to media buttons. It will
    /// broadcast events to the controllers and agents.
    MediaButtons,
    /// Responsible for initializing all of the controllers.
    Restore,
    /// Responsible for logging external API calls to other components and
    /// their responses to Inspect.
    InspectExternalApis,
    /// Responsible for recording internal state of messages sent on the message
    /// hub to policy proxies handlers.
    InspectPolicyValues,
    /// Responsible for logging all settings values of messages between the
    /// proxy and setting handlers to Inspect.
    InspectSettingProxy,
    /// Responsible for logging the setting values in the setting proxy to Inspect.
    InspectSettingValues,
    /// Responsible for logging API usage counts to Inspect.
    InspectSettingTypeUsage,
}

pub fn get_default_agent_types() -> HashSet<AgentType> {
    [
        AgentType::Restore,
        AgentType::InspectExternalApis,
        AgentType::InspectPolicyValues,
        AgentType::InspectSettingProxy,
        AgentType::InspectSettingValues,
        AgentType::InspectSettingTypeUsage,
    ]
    .into_iter()
    .collect()
}

impl From<AgentType> for BlueprintHandle {
    fn from(agent_type: AgentType) -> BlueprintHandle {
        match agent_type {
            AgentType::CameraWatcher => crate::agent::camera_watcher::blueprint::create(),
            AgentType::Earcons => crate::agent::earcons::agent::blueprint::create(),
            AgentType::MediaButtons => crate::agent::media_buttons::blueprint::create(),
            AgentType::Restore => crate::agent::restore_agent::blueprint::create(),
            AgentType::InspectExternalApis => {
                crate::agent::inspect::external_apis::blueprint::create()
            }
            AgentType::InspectSettingProxy => {
                crate::agent::inspect::setting_proxy::blueprint::create()
            }
            AgentType::InspectSettingTypeUsage => {
                crate::agent::inspect::usage_counts::blueprint::create()
            }
            AgentType::InspectPolicyValues => {
                crate::agent::inspect::policy_values::blueprint::create()
            }
            AgentType::InspectSettingValues => {
                crate::agent::inspect::setting_values::blueprint::create()
            }
        }
    }
}

#[macro_export]
macro_rules! create_agent {
    ($component:ident, $create:expr) => {
        AgentRegistrar::Creator(AgentCreator {
            debug_id: concat!(stringify!($component), "_agent"),
            create: |c| Box::pin($create(c)),
        })
    };
}

impl From<AgentType> for AgentRegistrar {
    fn from(agent_type: AgentType) -> AgentRegistrar {
        use crate::agent::*;
        match agent_type {
            AgentType::CameraWatcher => {
                create_agent!(camera_watcher, camera_watcher::CameraWatcherAgent::create)
            }
            AgentType::Earcons => create_agent!(earcons, earcons::agent::Agent::create),
            AgentType::MediaButtons => {
                create_agent!(media_buttons, media_buttons::MediaButtonsAgent::create)
            }
            AgentType::Restore => create_agent!(restore_agent, restore_agent::RestoreAgent::create),
            AgentType::InspectExternalApis => create_agent!(
                external_apis,
                inspect::external_apis::ExternalApiInspectAgent::create
            ),
            AgentType::InspectSettingProxy => create_agent!(
                setting_proxy,
                inspect::setting_proxy::SettingProxyInspectAgent::create
            ),
            AgentType::InspectSettingTypeUsage => create_agent!(
                usage_counts,
                inspect::usage_counts::SettingTypeUsageInspectAgent::create
            ),
            AgentType::InspectPolicyValues => create_agent!(
                policy_values,
                inspect::policy_values::PolicyValuesInspectAgent::create
            ),
            AgentType::InspectSettingValues => create_agent!(
                setting_values,
                inspect::setting_values::SettingValuesInspectAgent::create
            ),
        }
    }
}
