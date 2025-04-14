// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use std::time::{Duration, Instant};

#[cfg(feature = "fdomain")]
use {
    fdomain_client::fidl::{DiscoverableProtocolMarker, Proxy},
    fdomain_fuchsia_developer_remotecontrol::RemoteControlProxy,
    fdomain_fuchsia_io as fio,
    fdomain_fuchsia_sys2::OpenDirType,
};

#[cfg(not(feature = "fdomain"))]
use {
    fidl::endpoints::DiscoverableProtocolMarker,
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy, fidl_fuchsia_io as fio,
    fidl_fuchsia_sys2 as sys2, fidl_fuchsia_sys2::OpenDirType,
};

pub const MONIKER: &str = "toolbox";
const LEGACY_MONIKER: &str = "core/toolbox";

#[cfg(not(feature = "fdomain"))]
async fn connect_realm_query(
    rcs: &RemoteControlProxy,
    moniker: &str,
) -> Result<sys2::RealmQueryProxy> {
    // Try to connect via fuchsia.developer.remotecontrol/RemoteControl.ConnectCapability.
    let (query, server) = fidl::endpoints::create_proxy::<sys2::RealmQueryMarker>();
    let result = rcs
        .connect_capability(
            moniker,
            sys2::OpenDirType::NamespaceDir,
            &format!("svc/{}.root", sys2::RealmQueryMarker::PROTOCOL_NAME),
            server.into_channel(),
        )
        .await?
        .map_err(|e| anyhow::anyhow!("{e:?}"));
    if result.is_ok() {
        return Ok(query);
    }
    // Fallback to fuchsia.developer.remotecontrol/RemoteControl.DeprecatedOpenCapability.
    // This can be removed once we drop support for API level 27.
    let (query, server) = fidl::endpoints::create_proxy::<sys2::RealmQueryMarker>();
    rcs.deprecated_open_capability(
        moniker,
        sys2::OpenDirType::NamespaceDir,
        &format!("svc/{}.root", sys2::RealmQueryMarker::PROTOCOL_NAME),
        server.into_channel(),
        Default::default(),
    )
    .await?
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(query)
}

/// Open the service directory of the toolbox.
#[cfg(not(feature = "fdomain"))]
pub async fn open_toolbox(rcs: &RemoteControlProxy) -> Result<fio::DirectoryProxy> {
    let (query, moniker) = {
        if let Ok(query) = connect_realm_query(rcs, MONIKER).await {
            (query, MONIKER)
        } else {
            let query = connect_realm_query(rcs, LEGACY_MONIKER).await?;
            (query, LEGACY_MONIKER)
        }
    };
    let moniker = moniker::Moniker::try_from(moniker)?;
    let namespace_dir = component_debug::dirs::open_instance_directory(
        &moniker,
        sys2::OpenDirType::NamespaceDir.into(),
        &query,
    )
    .await?;
    let (ret, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    namespace_dir.open(
        "svc",
        fio::Flags::PROTOCOL_DIRECTORY | fio::PERM_READABLE,
        &fio::Options::default(),
        server.into(),
    )?;

    Ok(ret)
}

/// Open the service directory of the toolbox.
#[cfg(feature = "fdomain")]
pub async fn open_toolbox(rcs: &RemoteControlProxy) -> Result<fio::DirectoryProxy> {
    rcs.domain().namespace().await.map_err(Into::into).map(fio::DirectoryProxy::from_channel)
}

/// Connects to a protocol available in the namespace of the `toolbox` component.
/// If we fail to connect to the protocol in the namespace of the `toolbox` component, then we'll
/// attempt to connect to the protocol in the exposed directory of the component located at the
/// given `backup_moniker`.
pub async fn connect_with_timeout<P>(
    rcs_proxy: &RemoteControlProxy,
    backup_moniker: Option<impl AsRef<str>>,
    dur: Duration,
) -> Result<P::Proxy>
where
    P: DiscoverableProtocolMarker,
{
    let protocol_name = P::PROTOCOL_NAME;
    // time this so that we can use an appropriately shorter timeout for the attempt
    // to connect by the backup (if there is one)
    let start_time = Instant::now();
    let toolbox_res = crate::open_with_timeout_at::<P>(
        dur,
        MONIKER,
        OpenDirType::NamespaceDir,
        &format!("svc/{protocol_name}"),
        rcs_proxy,
    )
    .await;

    // Fallback to legacy toolbox moniker if toolbox is not available.
    let toolbox_res = match toolbox_res {
        Ok(toolbox) => Ok(toolbox),
        Err(_) => {
            let toolbox_took = Instant::now() - start_time;
            let timeout = dur.saturating_sub(toolbox_took);
            crate::open_with_timeout_at::<P>(
                timeout,
                LEGACY_MONIKER,
                OpenDirType::NamespaceDir,
                &format!("svc/{protocol_name}"),
                rcs_proxy,
            )
            .await
        }
    };

    let toolbox_took = Instant::now() - start_time;

    // after doing these somewhat awkward lines, we know that toolbox_res is an
    // error and we have to either try the backup or return a useful error
    // message. This just avoids an indentation or having to break this out
    // into another single-use function. It's kind of a reverse `?`.
    let Some(backup) = backup_moniker.as_ref().map(|s| s.as_ref()) else {
        return toolbox_res.context(toolbox_error_message(protocol_name));
    };
    if let Ok(toolbox) = toolbox_res {
        return Ok(toolbox);
    }

    // try to connect to the moniker given instead, but don't double
    // up the timeout.
    let timeout = dur.saturating_sub(toolbox_took);
    let moniker_res =
        crate::open_with_timeout::<P>(timeout, &backup, OpenDirType::ExposedDir, &rcs_proxy).await;

    // stack the errors together so we can see both of them in the log if
    // we want to and then provide an error message that indicates we tried
    // both and could find it at neither.
    moniker_res.context(backup_error_message(protocol_name, &backup))
}

fn toolbox_error_message(protocol_name: &str) -> String {
    format!(
        "\
        Attempted to find protocol marker {protocol_name} at \
        '/toolbox', but it wasn't available. \n\n\
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
        '/toolbox' or '{backup_name}', but it wasn't available \
        at either of those monikers. \n\n\
        Make sure the target is connected and otherwise functioning, \
        and that it is configured to provide capabilities over the \
        network to host tools.\
    "
    )
}
