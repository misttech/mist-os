// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use ffx_command_error::Result;
use ffx_config::EnvironmentContext;
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use fho::FhoConnectionBehavior;

mod target_info;
mod target_proxy;

pub use target_info::TargetInfoHolder;
pub use target_proxy::TargetProxyHolder;

pub(crate) async fn init_daemon_behavior(
    context: &EnvironmentContext,
) -> Result<FhoConnectionBehavior> {
    let build_info = context.build_info();
    let overnet_injector = Injection::initialize_overnet(
        context.clone(),
        None,
        DaemonVersionCheck::SameVersionInfo(build_info),
    )
    .await?;

    Ok(FhoConnectionBehavior::DaemonConnector(Arc::new(overnet_injector)))
}
