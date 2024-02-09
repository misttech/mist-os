// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module holds the common data types for emulator engines. These are implementation-agnostic
//! data types, not the engine-specific command types that each engine will define for itself. These
//! types will be directly deserializable from the PBM, and converted into engine-specific types at
//! runtime.

use emulator_instance::{EmulatorConfiguration, FlagData, NetworkingMode, PortMapping};
use sdk_metadata::{display_impl, VirtualDeviceV1};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt::Display, path::PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineConsoleType {
    /// An emulator console for issuing commands to the emulation hypervisor.
    Command,

    /// An emulator console designed for machine-to-machine communication through a structured
    /// language such as JSON.
    Machine,

    /// An emulator console for communicating with the virtual serial port.
    Serial,

    /// A default value indicating none of the above.
    None,
}

impl Default for EngineConsoleType {
    fn default() -> Self {
        EngineConsoleType::None
    }
}

display_impl!(EngineConsoleType);

/// Indicates which details the "show" command should return.
// TODO(https://fxbug.dev/324167674): fix.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum ShowDetail {
    All,
    Cmd {
        program: Option<String>,
        args: Option<Vec<String>>,
        env: Option<HashMap<String, String>>,
    },
    Config {
        flags: Option<FlagData>,
    },
    Device {
        device: Option<VirtualDeviceV1>,
    },
    Net {
        mode: Option<NetworkingMode>,
        mac_address: Option<String>,
        upscript: Option<PathBuf>,
        ports: Option<HashMap<String, PortMapping>>,
    },
    Raw {
        config: Option<EmulatorConfiguration>,
    },
}

impl Default for ShowDetail {
    fn default() -> Self {
        ShowDetail::All
    }
}

impl Display for ShowDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShowDetail::All => write!(f, "ShowDetail::All is a metavalue")?,
            ShowDetail::Cmd { program, args, env } => {
                writeln!(f, "Command:")?;
                writeln!(f, "\tProgram: {}", program.as_ref().unwrap_or(&String::from("")))?;
                writeln!(f, "\tArguments: {}", args.as_ref().unwrap_or(&vec![]).join(" "))?;
                writeln!(f, "\tEnvironment:")?;
                if let Some(env_map) = env {
                    for (k, v) in env_map {
                        writeln!(f, "\t\t{k:32}{v}")?;
                    }
                } else {
                    writeln!(f, "\t\tNone")?;
                }
            }
            ShowDetail::Config { flags } => {
                writeln!(f, "Configuration:")?;
                if let Some(flag_data) = flags {
                    let s = serde_json::to_string_pretty(&flag_data)
                        .expect("serialization error for flagdata");
                    writeln!(f, "\t{s}")?;
                } else {
                    writeln!(f, "\tNone")?;
                }
            }
            ShowDetail::Device { device } => {
                writeln!(f, "Device:")?;
                if let Some(device_data) = device {
                    let s = serde_json::to_string_pretty(&device_data)
                        .expect("serialization error for flagdata");
                    writeln!(f, "\t{s}")?;
                } else {
                    writeln!(f, "\tNone")?;
                }
            }
            ShowDetail::Net { mode, mac_address, upscript, ports } => {
                writeln!(f, "Network:")?;
                if let Some(mode_data) = mode {
                    writeln!(f, "\tMode: {}", mode_data)?;
                } else {
                    writeln!(f, "\tMode: None")?;
                }
                if let Some(mac_data) = mac_address {
                    writeln!(f, "\tMAC: {}", mac_data)?;
                } else {
                    writeln!(f, "\tMAC: None")?;
                }
                if let Some(script_data) = upscript {
                    writeln!(f, "\tUpscript: {}", script_data.to_string_lossy())?;
                } else {
                    writeln!(f, "\tUpscript: None")?;
                }
                if let Some(portdata) = ports {
                    writeln!(f, "\tPort mappings:")?;
                    writeln!(f, "\t\t{:10}{:10}{:10}", "name", "guest", "host")?;
                    for (k, v) in portdata {
                        writeln!(f, "\t\t{:10}{:<10}{:<10}", k, v.guest, v.host.unwrap_or(0))?;
                    }
                }
            }
            ShowDetail::Raw { config } => {
                if let Some(config_data) = config {
                    let s = serde_json::to_string_pretty(&config_data)
                        .expect("serialization error for config_data");
                    writeln!(f, "\t{s}")?;
                } else {
                    writeln!(f, "None")?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_show_detail() {
        // Verify it returns a default.
        let _default = ShowDetail::default();
    }
}
