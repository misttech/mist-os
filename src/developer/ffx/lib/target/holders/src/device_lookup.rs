// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_command_error::{FfxContext as _, Result};
use ffx_config::EnvironmentContext;
use ffx_target::TargetInfoQuery;
use fho::DeviceLookup;
use fidl_fuchsia_developer_ffx as ffx_fidl;
use futures::future::LocalBoxFuture;

/// The default implementation of device lookup and resolution. Primarily used for simpler testing.
#[doc(hidden)]
#[derive(Clone)]
pub struct DeviceLookupDefaultImpl;

impl DeviceLookup for DeviceLookupDefaultImpl {
    fn target_spec(&self, env: EnvironmentContext) -> LocalBoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            ffx_target::get_target_specifier(&env).await.bug_context("looking up target specifier")
        })
    }

    fn resolve_target_query_to_info(
        &self,
        query: TargetInfoQuery,
        ctx: EnvironmentContext,
    ) -> LocalBoxFuture<'_, Result<Vec<ffx_fidl::TargetInfo>>> {
        Box::pin(async move {
            ffx_target::resolve_target_query_to_info(query, &ctx)
                .await
                .bug_context("resolving target")
        })
    }
}
