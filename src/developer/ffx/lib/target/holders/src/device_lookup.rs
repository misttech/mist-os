// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::TargetInfoHolder;
use ffx_command_error::{FfxContext as _, Result};
use ffx_config::EnvironmentContext;
use fho::{return_user_error, DeviceLookup, FhoTargetInfo};
use futures::future::LocalBoxFuture;

pub struct DeviceLookupDefaultImpl;

impl DeviceLookup for DeviceLookupDefaultImpl {
    fn target_spec(&self, env: EnvironmentContext) -> LocalBoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            ffx_target::get_target_specifier(&env).await.bug_context("looking up target specifier")
        })
    }

    fn resolve_target_query_to_info(
        &self,
        query: Option<String>,
        ctx: EnvironmentContext,
    ) -> LocalBoxFuture<'_, Result<Vec<Box<dyn FhoTargetInfo>>>> {
        Box::pin(async move {
            match ffx_target::resolve_target_query_to_info(query, &ctx)
                .await
                .bug_context("resolving target")
            {
                Ok(targets) => Ok(targets
                    .iter()
                    .map(|t| {
                        let info: TargetInfoHolder = t.into();
                        Box::new(info) as Box<dyn FhoTargetInfo>
                    })
                    .collect()),
                Err(e) => return_user_error!(e),
            }
        })
    }
}
