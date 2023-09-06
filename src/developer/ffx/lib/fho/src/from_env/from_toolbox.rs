// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_command::{FfxContext, Result};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy};
use rcs::OpenDirType;
use std::marker::PhantomData;
use std::time::Instant;

use super::{helpers::*, FhoEnvironment, TryFromEnvWith, DEFAULT_PROXY_TIMEOUT};

/// The implementation of the decorator returned by [`toolbox`] and
/// [`toolbox_or`].
pub struct WithToolbox<P> {
    backup: Option<String>,
    _p: PhantomData<fn() -> P>,
}

impl<P> WithToolbox<P> {
    /// The well known moniker location for the global toolbox capabilities
    /// on a target device.
    pub const TOOLBOX_MONIKER: &str = "/core/toolbox";
}

#[async_trait(?Send)]
impl<P> TryFromEnvWith for WithToolbox<P>
where
    P: Proxy + 'static,
    P::Protocol: DiscoverableProtocolMarker,
{
    type Output = P;
    async fn try_from_env_with(self, env: &FhoEnvironment) -> Result<Self::Output> {
        // for error messages.
        let protocol_name = P::Protocol::PROTOCOL_NAME;
        // start off by connecting to rcs
        let rcs = connect_to_rcs(env).await?;
        // time this so that we can use an appropriately shorter timeout for the attempt
        // to connect by the backup (if there is one)
        let start_time = Instant::now();
        let (proxy, server_end) = create_proxy()?;
        let toolbox_res = open_moniker(
            proxy,
            server_end,
            &rcs,
            OpenDirType::NamespaceDir,
            Self::TOOLBOX_MONIKER,
            DEFAULT_PROXY_TIMEOUT,
        )
        .await;
        let toolbox_took = Instant::now() - start_time;
        // after doing these somewhat awkward lines, we know that toolbox_res is an
        // error and we have to either try the backup or return a useful error
        // message. This just avoids an indentation or having to break this out
        // into another single-use function. It's kind of a reverse `?`.
        let Some(backup) = self.backup else {
            return toolbox_res.with_user_message(|| toolbox_error_message(protocol_name))
        };
        let Err(toolbox_err) = toolbox_res else { return toolbox_res };

        // try to connect to the moniker given instead, but don't double
        // up the timeout.
        let timeout = DEFAULT_PROXY_TIMEOUT.saturating_sub(toolbox_took);
        let (proxy, server_end) = create_proxy()?;
        let moniker_res =
            open_moniker(proxy, server_end, &rcs, OpenDirType::ExposedDir, &backup, timeout).await;

        // stack the errors together so we can see both of them in the log if
        // we want to and then provide an error message that indicates we tried
        // both and could find it at neither.
        moniker_res
            .bug_context(toolbox_err)
            .with_user_message(|| backup_error_message(protocol_name, &backup))
    }
}

fn toolbox_error_message(protocol_name: &str) -> String {
    format!(
        "\
        Attempted to find protocol marker {protocol_name} at \
        '/core/toolbox', but it wasn't available. \n\n\
        Make sure the target is connected and otherwise functioning, \
        and that it is configured to provide capabilities over the \
        network to host tools.\
    "
    )
}

fn backup_error_message(protocol_name: &str, backup_name: &str) -> String {
    format!(
        "\
        Attempted to find protocol marker {protocol_name} at \
        '/core/toolbox' or '{backup_name}', but it wasn't available \
        at either of those monikers. \n\n\
        Make sure the target is connected and otherwise functioning, \
        and that it is configured to provide capabilities over the \
        network to host tools.\
    "
    )
}

/// Uses the `/core/toolbox` to find the given proxy.
pub fn toolbox<P: Proxy>() -> WithToolbox<P> {
    WithToolbox { backup: None, _p: PhantomData::default() }
}

/// Uses the `/core/toolbox` to find the given proxy, and falls
/// back to the given moniker if not.
pub fn toolbox_or<P: Proxy>(or_moniker: impl AsRef<str>) -> WithToolbox<P> {
    WithToolbox { backup: Some(or_moniker.as_ref().to_owned()), _p: PhantomData::default() }
}
