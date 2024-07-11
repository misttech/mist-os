// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use selinux::security_server;
use starnix_core::task::{CurrentTask, KernelFeatures};
use starnix_logging::log_error;
use starnix_sync::{Locked, Unlocked};

/// A collection of parsed features, and their arguments.
#[derive(Default, Debug)]
pub struct Features {
    pub kernel: KernelFeatures,

    /// Configures whether SELinux is fully enabled, faked, or unavailable.
    pub selinux: Option<security_server::Mode>,

    /// Include the /container directory in the root file system.
    pub container: bool,

    /// Include the /test_data directory in the root file system.
    pub test_data: bool,

    /// Include the /custom_artifacts directory in the root file system.
    pub custom_artifacts: bool,

    pub self_profile: bool,

    pub rootfs_rw: bool,

    pub network_manager: bool,
}

/// Parses all the featurse in `entries`.
///
/// Returns an error if parsing fails, or if an unsupported feature is present in `features`.
pub fn parse_features(entries: &Vec<String>) -> Result<Features, Error> {
    let mut features = Features::default();
    for entry in entries {
        let (raw_flag, raw_args) =
            entry.split_once(':').map(|(f, a)| (f, Some(a.to_string()))).unwrap_or((entry, None));
        match (raw_flag, raw_args) {
            ("container", _) => features.container = true,
            ("custom_artifacts", _) => features.custom_artifacts = true,
            ("network_manager", _) => features.network_manager = true,
            ("enable_suid", _) => features.kernel.enable_suid = true,
            ("rootfs_rw", _) => features.rootfs_rw = true,
            ("self_profile", _) => features.self_profile = true,
            ("selinux", mode_arg) => {
                features.selinux = match mode_arg.as_ref() {
                    Some(mode) => {
                        if mode == "fake" {
                            Some(security_server::Mode::Fake)
                        } else {
                            return Err(anyhow!("Invalid SELinux mode"));
                        }
                    }
                    None => Some(security_server::Mode::Enable),
                }
            }
            ("test_data", _) => features.test_data = true,
            (f, _) => {
                return Err(anyhow!("Unsupported feature: {}", f));
            }
        };
    }

    Ok(features)
}

/// Runs all the features that are enabled in `system_task.kernel()`.
pub fn run_container_features(
    _locked: &mut Locked<'_, Unlocked>,
    system_task: &CurrentTask,
    features: &Features,
) -> Result<(), Error> {
    let kernel = system_task.kernel();

    let mut enabled_profiling = false;

    if features.self_profile {
        enabled_profiling = true;
        fuchsia_inspect::component::inspector().root().record_lazy_child(
            "self_profile",
            fuchsia_inspect_contrib::ProfileDuration::lazy_node_callback,
        );
        fuchsia_inspect_contrib::start_self_profiling();
    }
    if !enabled_profiling {
        fuchsia_inspect_contrib::stop_self_profiling();
    }
    if features.network_manager {
        if let Err(e) = kernel.network_manager.init() {
            log_error!("Network manager initialization failed: ({e:?})");
        }
    }

    Ok(())
}
