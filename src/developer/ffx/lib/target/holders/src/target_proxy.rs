// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::init_daemon_behavior;
use async_trait::async_trait;
use errors::FfxError;
use ffx_command_error::{FfxContext as _, Result};
use fho::{FhoEnvironment, TryFromEnv};
use fidl_fuchsia_developer_ffx as ffx_fidl;
use std::ops::Deref;

#[derive(Clone, Debug)]
pub struct TargetProxyHolder(ffx_fidl::TargetProxy);

impl Deref for TargetProxyHolder {
    type Target = ffx_fidl::TargetProxy;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<ffx_fidl::TargetProxy> for TargetProxyHolder {
    fn from(value: ffx_fidl::TargetProxy) -> Self {
        TargetProxyHolder(value)
    }
}

#[async_trait(?Send)]
impl TryFromEnv for TargetProxyHolder {
    async fn try_from_env(env: &FhoEnvironment) -> Result<Self> {
        if env.behavior().await.is_none() {
            let b = init_daemon_behavior(env.environment_context()).await?;
            env.set_behavior(b.clone()).await;
        }
        match env.injector::<Self>().await?.target_factory().await.map_err(|e| {
            // This error case happens when there are multiple targets in target list.
            // So let's print out the ffx error message directly (which comes from OpenTargetError::QueryAmbiguous)
            // rather than just returning "Failed to create target proxy" which is not helpful.
            if let Some(ffx_e) = &e.downcast_ref::<FfxError>() {
                let message = format!("{ffx_e}");
                Err(e).user_message(message)
            } else {
                Err(e).user_message("Failed to create target proxy")
            }
        }) {
            Ok(p) => Ok(p.into()),
            Err(e) => e,
        }
    }
}
