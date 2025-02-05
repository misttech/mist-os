// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::connector::DirectConnector;
use crate::fho_env::FhoConnectionBehavior;
use crate::{FhoEnvironment, TryFromEnv};
use async_trait::async_trait;
use ffx_command_error::{return_user_error, Result};
use std::sync::Arc;

#[async_trait(?Send)]
pub trait CheckEnv {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()>;
}

/// Checks if the experimental config flag is set. This gates the execution of the command.
/// If the flag is set to `true`, this returns `Ok(())`, else returns an error.
pub struct AvailabilityFlag<T>(pub T);

#[async_trait(?Send)]
impl<T: AsRef<str>> CheckEnv for AvailabilityFlag<T> {
    async fn check_env(self, env: &FhoEnvironment) -> Result<()> {
        let flag = self.0.as_ref();
        if env.environment_context().get(flag).unwrap_or(false) {
            Ok(())
        } else {
            return_user_error!(
                "This is an experimental subcommand.  To enable this subcommand run 'ffx config set {} true'",
                flag
            );
        }
    }
}

#[async_trait(?Send)]
impl TryFromEnv for ffx_writer::SimpleWriter {
    async fn try_from_env(_env: &FhoEnvironment) -> Result<Self> {
        Ok(ffx_writer::SimpleWriter::new())
    }
}

#[async_trait(?Send)]
impl<T: serde::Serialize> TryFromEnv for ffx_writer::MachineWriter<T> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(ffx_writer::MachineWriter::new(env.ffx_command().global.machine))
    }
}

#[async_trait(?Send)]
impl<T: serde::Serialize + schemars::JsonSchema> TryFromEnv
    for ffx_writer::VerifiedMachineWriter<T>
{
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        Ok(ffx_writer::VerifiedMachineWriter::new(env.ffx_command().global.machine))
    }
}

// Returns a DirectConnector only if we have a direct connection. Returns None for
// a daemon connection.
#[async_trait(?Send)]
impl TryFromEnv for Option<Arc<dyn DirectConnector>> {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        match env.behavior().await {
            Some(FhoConnectionBehavior::DirectConnector(ref direct)) => Ok(Some(direct.clone())),
            _ => Ok(None),
        }
    }
}
