// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::FfxError;
use ffx_config::EnvironmentContext;
use fidl::{endpoints::create_proxy, prelude::*};
use fidl_fuchsia_developer_ffx::{
    DaemonError, DaemonProxy, TargetCollectionMarker, TargetMarker, TargetProxy, TargetQuery,
};
use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlProxy};
use futures::{select, Future, FutureExt};
use std::time::Duration;
use timeout::timeout;

const FASTBOOT_INLINE_TARGET: &str = "ffx.fastboot.inline_target";

#[derive(Debug, Clone)]
pub enum TargetKind {
    Normal(String),
    FastbootInline(String),
}

impl ToString for TargetKind {
    fn to_string(&self) -> String {
        match self {
            Self::Normal(target) => target.to_string(),
            Self::FastbootInline(serial) => serial.to_string(),
        }
    }
}

/// Attempt to connect to RemoteControl on a target device using a connection to a daemon.
///
/// The optional |target| is a string matcher as defined in fuchsia.developer.ffx.TargetQuery
/// fidl table.
#[tracing::instrument]
pub async fn get_remote_proxy(
    target: Option<TargetKind>,
    is_default_target: bool,
    daemon_proxy: DaemonProxy,
    proxy_timeout: Duration,
) -> Result<RemoteControlProxy> {
    let (target_proxy, target_proxy_fut) =
        open_target_with_fut(target.clone(), is_default_target, daemon_proxy, proxy_timeout)?;
    let mut target_proxy_fut = target_proxy_fut.boxed_local().fuse();
    let (remote_proxy, remote_server_end) = create_proxy::<RemoteControlMarker>()?;
    let mut open_remote_control_fut =
        target_proxy.open_remote_control(remote_server_end).boxed_local().fuse();
    let res = loop {
        select! {
            res = open_remote_control_fut => {
                match res {
                    Err(e) => {
                        // Getting here is most likely the result of a PEER_CLOSED error, which
                        // may be because the target_proxy closure has propagated faster than
                        // the error (which can happen occasionally). To counter this, wait for
                        // the target proxy to complete, as it will likely only need to be
                        // polled once more (open_remote_control_fut partially depends on it).
                        target_proxy_fut.await?;
                        return Err(e.into());
                    }
                    Ok(r) => break(r),
                }
            }
            res = target_proxy_fut => {
                res?
            }
        }
    };
    let target = target.as_ref().map(ToString::to_string);
    match res {
        Ok(_) => Ok(remote_proxy),
        Err(err) => Err(anyhow::Error::new(FfxError::TargetConnectionError {
            err,
            target,
            is_default_target,
            logs: Some(target_proxy.get_ssh_logs().await?),
        })),
    }
}

/// Attempt to connect to a target given a connection to a daemon.
///
/// The returned future must be polled to completion. It is returned separately
/// from the TargetProxy to enable immediately pushing requests onto the TargetProxy
/// before connecting to the target completes.
///
/// The optional |target| is a string matcher as defined in fuchsia.developer.ffx.TargetQuery
/// fidl table.
#[tracing::instrument]
pub fn open_target_with_fut<'a>(
    target: Option<TargetKind>,
    is_default_target: bool,
    daemon_proxy: DaemonProxy,
    target_timeout: Duration,
) -> Result<(TargetProxy, impl Future<Output = Result<()>> + 'a)> {
    let (tc_proxy, tc_server_end) = create_proxy::<TargetCollectionMarker>()?;
    let (target_proxy, target_server_end) = create_proxy::<TargetMarker>()?;
    let target_kind = target.clone();
    let target = target.as_ref().map(ToString::to_string);
    let t_clone = target.clone();
    let target_collection_fut = async move {
        daemon_proxy
            .connect_to_protocol(
                TargetCollectionMarker::PROTOCOL_NAME,
                tc_server_end.into_channel(),
            )
            .await?
            .map_err(|err| FfxError::DaemonError { err, target: t_clone, is_default_target })?;
        Result::<()>::Ok(())
    };
    let t_clone = target.clone();
    let target_handle_fut = async move {
        if let Some(TargetKind::FastbootInline(serial_number)) = target_kind {
            tracing::trace!("got serial number: {}", serial_number);
            timeout(target_timeout, tc_proxy.add_inline_fastboot_target(&serial_number)).await??;
        }
        timeout(
            target_timeout,
            tc_proxy.open_target(
                &TargetQuery { string_matcher: t_clone.clone(), ..Default::default() },
                target_server_end,
            ),
        )
        .await
        .map_err(|_| FfxError::DaemonError {
            err: DaemonError::Timeout,
            target: t_clone,
            is_default_target,
        })??
        .map_err(|err| FfxError::OpenTargetError { err, target, is_default_target })?;
        Result::<()>::Ok(())
    };
    let fut = async move {
        let ((), ()) = futures::try_join!(target_collection_fut, target_handle_fut)?;
        Ok(())
    };

    Ok((target_proxy, fut))
}

/// Attempts to resolve the default target. Returning Some(_) if a target has been found, None
/// otherwise.
pub async fn resolve_default_target(
    env_context: &EnvironmentContext,
) -> Result<Option<TargetKind>> {
    Ok(maybe_inline_target(env_context.get("target.default").await?, &env_context).await)
}

/// In the event that a default target is supplied and there needs to be additional Fastboot
/// inlining, this will handle wrapping the additional information for use in the FFX injector.
pub async fn maybe_inline_target(
    target: Option<String>,
    env_context: &EnvironmentContext,
) -> Option<TargetKind> {
    match target {
        Some(t) => {
            if env_context.get(FASTBOOT_INLINE_TARGET).await.unwrap_or(false) {
                Some(TargetKind::FastbootInline(t))
            } else {
                Some(TargetKind::Normal(t))
            }
        }
        None => None,
    }
}
