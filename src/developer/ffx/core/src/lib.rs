// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use core_macros::ffx_command;

use anyhow::Result;
use async_trait::async_trait;
use fdomain_fuchsia_developer_remotecontrol::RemoteControlProxy as FRemoteControlProxy;
use fidl_fuchsia_developer_ffx::{DaemonProxy, TargetProxy, VersionInfo};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use thiserror::Error;

/// Exports used in macros
#[doc(hidden)]
pub mod macro_deps {
    pub use {anyhow, errors, fidl, fidl_fuchsia_developer_ffx, fuchsia_async, futures, rcs};
}

#[derive(Error, Debug)]
pub enum FfxInjectorError {
    // This error message must stay the same as it's load-bearing to infra.
    // LINT.IfChange
    #[error("FFX Daemon was told not to autostart and no existing Daemon instance was found")]
    DaemonAutostartDisabled,
    // LINT.ThenChange(//tools/testing/tefmocheck/string_in_log_check.go)
    #[error(transparent)]
    UnknownError(#[from] anyhow::Error),
}

/// Downcasts an anyhow::Error to a structured error.
/// Used for compatibility purposes until all of ffx
/// is structured.
pub fn downcast_injector_error<T>(res: Result<T, anyhow::Error>) -> Result<T, FfxInjectorError> {
    res.map_err(|err| match err.downcast() {
        Ok(value) => value,
        Err(value) => value.into(),
    })
}

#[async_trait(?Send)]
pub trait Injector {
    async fn daemon_factory(&self) -> Result<DaemonProxy, FfxInjectorError>;
    /// Creates the daemon, ignoring autostart. This should generally not be used
    /// unless you're running `ffx daemon start`.
    async fn daemon_factory_force_autostart(&self) -> Result<DaemonProxy, FfxInjectorError>;
    /// Attempts to get a handle to the ffx daemon.
    async fn try_daemon(&self) -> Result<Option<DaemonProxy>>;
    async fn remote_factory(&self) -> Result<RemoteControlProxy>;
    async fn remote_factory_fdomain(&self) -> Result<FRemoteControlProxy>;
    async fn target_factory(&self) -> Result<TargetProxy>;
    async fn is_experiment(&self, key: &str) -> bool;
    async fn build_info(&self) -> Result<VersionInfo>;
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use errors::ffx_error;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_downcast_injector_error() {
        assert_matches!(
            downcast_injector_error::<()>(Err(ffx_error!("test error").into())),
            Err(FfxInjectorError::UnknownError(_))
        );
        assert_matches!(
            downcast_injector_error::<()>(Err(FfxInjectorError::DaemonAutostartDisabled.into())),
            Err(FfxInjectorError::DaemonAutostartDisabled)
        );
    }
}
