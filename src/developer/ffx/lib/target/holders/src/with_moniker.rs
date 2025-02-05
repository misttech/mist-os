// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_command_error::Result;
use fho::{FhoEnvironment, TryFromEnvWith};
use fidl::encoding::DefaultFuchsiaResourceDialect;
use fidl::endpoints::Proxy;
use std::marker::PhantomData;
use std::time::Duration;

use crate::connect_to_rcs;

/// The implementation of the decorator returned by [`moniker`].
pub struct WithMoniker<P, D> {
    moniker: String,
    timeout: Duration,
    _p: PhantomData<(fn() -> P, D)>,
}

#[async_trait(?Send)]
impl<P> TryFromEnvWith for WithMoniker<P, fdomain_client::fidl::FDomainResourceDialect>
where
    P: fdomain_client::fidl::Proxy + 'static,
    P::Protocol: fdomain_client::fidl::DiscoverableProtocolMarker,
{
    type Output = P;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        let rcs_instance = crate::fdomain::connect_to_rcs_fdomain(&env).await?;
        crate::fdomain::open_moniker_fdomain(
            &rcs_instance,
            rcs_fdomain::OpenDirType::ExposedDir,
            &self.moniker,
            self.timeout,
        )
        .await
    }
}

#[async_trait(?Send)]
impl<P> TryFromEnvWith for WithMoniker<P, DefaultFuchsiaResourceDialect>
where
    P: Proxy + 'static,
    P::Protocol: fidl::endpoints::DiscoverableProtocolMarker,
{
    type Output = P;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        let rcs_instance = connect_to_rcs(&env).await?;
        crate::remote_control_proxy::open_moniker(
            &rcs_instance,
            rcs::OpenDirType::ExposedDir,
            &self.moniker,
            self.timeout,
        )
        .await
    }
}
