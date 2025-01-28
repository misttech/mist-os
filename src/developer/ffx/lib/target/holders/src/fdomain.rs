// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::from_toolbox::WithToolbox;
use fdomain_client::fidl::{FDomainResourceDialect, Proxy as FProxy};
use fdomain_fuchsia_developer_remotecontrol::RemoteControlProxy;
use ffx_command_error::Result;
use fho::{FhoEnvironment, TryFromEnv as _};

mod from_toolbox;

use from_toolbox::toolbox_or_f;

#[allow(dead_code)]
/// Same as [`moniker`] but for FDomain
pub fn moniker_f<P: FProxy>(moniker: impl AsRef<str>) -> WithToolbox<P, FDomainResourceDialect> {
    toolbox_or_f(moniker)
}

pub async fn connect_to_rcs_fdomain(env: &FhoEnvironment) -> Result<RemoteControlProxy> {
    let retry_count = 1;
    let mut tries = 0;
    // TODO(b/287693891): Remove explicit retries/timeouts here so they can be
    // configurable instead.
    loop {
        tries += 1;
        let res = RemoteControlProxy::try_from_env(env).await;
        if res.is_ok() || tries > retry_count {
            // Using `TryFromEnv` on `RemoteControlProxy` already contains user error information,
            // which will be propagated after exiting the loop.
            break Ok(res?);
        }
    }
}
