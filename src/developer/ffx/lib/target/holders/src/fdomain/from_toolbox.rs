// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fdomain_client::fidl::{
    DiscoverableProtocolMarker as FDiscoverableProtocolMarker, FDomainResourceDialect,
    Proxy as FProxy,
};
use ffx_command_error::Result;
use fho::{FhoEnvironment, TryFromEnvWith};
use std::marker::PhantomData;

use crate::from_toolbox::WithToolbox;
use crate::DEFAULT_PROXY_TIMEOUT;

use super::connect_to_rcs_fdomain;

#[async_trait(?Send)]
impl<P> TryFromEnvWith for WithToolbox<P, FDomainResourceDialect>
where
    P: FProxy + 'static,
    P::Protocol: FDiscoverableProtocolMarker,
{
    type Output = P;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        // start off by connecting to rcs
        let rcs = connect_to_rcs_fdomain(env).await?;
        let proxy = rcs_fdomain::toolbox::connect_with_timeout::<P::Protocol>(
            &rcs,
            self.backup.as_ref(),
            DEFAULT_PROXY_TIMEOUT,
        )
        .await?;
        Ok(proxy)
    }
}

/// Same as [`toolbox`] but for FDomain
//TODO(sadmac): This will be used soon, as FDomain is implemented.
#[allow(dead_code)]
pub fn toolbox_f<P: FProxy>() -> WithToolbox<P, FDomainResourceDialect> {
    WithToolbox { backup: None, _p: PhantomData::default() }
}

#[allow(dead_code)]
/// Same as [`toolbox_or`] but for FDomain
pub fn toolbox_or_f<P: FProxy>(
    or_moniker: impl AsRef<str>,
) -> WithToolbox<P, FDomainResourceDialect> {
    WithToolbox { backup: Some(or_moniker.as_ref().to_owned()), _p: PhantomData::default() }
}
