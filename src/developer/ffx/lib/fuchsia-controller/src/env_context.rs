// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::LibContext;
use anyhow::Result;
use camino::Utf8PathBuf;
use errors::{ffx_error, map_daemon_error};
use ffx_config::environment::ExecutableKind;
use ffx_config::EnvironmentContext;
use ffx_core::Injector;
use ffx_daemon::DaemonConfig;
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use ffx_target::add_manual_target;
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy};
use fidl::AsHandleRef;
use fidl_fuchsia_developer_ffx::{TargetCollectionMarker, TargetCollectionProxy, TargetProxy};
use fuchsia_zircon_types as zx_types;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

fn fxe<E: std::fmt::Debug>(e: E) -> anyhow::Error {
    ffx_error!("{e:?}").into()
}

#[derive(Debug)]
pub struct FfxConfigEntry {
    pub(crate) key: String,
    pub(crate) value: String,
}

pub struct EnvContext {
    lib_ctx: Weak<LibContext>,
    injector: Box<dyn Injector + Send + Sync>,
    pub(crate) context: EnvironmentContext,
}

impl EnvContext {
    pub(crate) fn write_err<T: std::fmt::Debug>(&self, err: T) {
        let lib = self.lib_ctx.upgrade().expect("library context instance deallocated early");
        lib.write_err(err)
    }

    pub(crate) fn lib_ctx(&self) -> Arc<LibContext> {
        self.lib_ctx.upgrade().expect("library context instance deallocated early")
    }

    pub async fn new(
        lib_ctx: Weak<LibContext>,
        config: Vec<FfxConfigEntry>,
        isolate_dir: Option<PathBuf>,
    ) -> Result<Self> {
        // TODO(https://fxbug.dev/42079638): This is a lot of potentially unnecessary data transformation
        // going through several layers of structured into unstructured and then back to structured
        // again. Likely the solution here is to update the input of the config runtime population
        // to accept structured data.
        let formatted_config = config
            .iter()
            .map(|entry| format!("{}={}", entry.key, entry.value))
            .collect::<Vec<String>>()
            .join(",");
        let runtime_config =
            if formatted_config.is_empty() { None } else { Some(formatted_config) };
        let runtime_args = ffx_config::runtime::populate_runtime(&[], runtime_config)?;
        let env_path = None;
        let current_dir = std::env::current_dir()?;
        let context = match isolate_dir {
            Some(d) => EnvironmentContext::isolated(
                ExecutableKind::Test,
                d,
                std::collections::HashMap::from_iter(std::env::vars()),
                runtime_args,
                env_path,
                Utf8PathBuf::try_from(current_dir).ok().as_deref(),
            )
            .map_err(fxe)?,
            None => EnvironmentContext::detect(
                ExecutableKind::Test,
                runtime_args,
                &current_dir,
                env_path,
            )
            .map_err(fxe)?,
        };
        let cache_path = context.get_cache_path()?;
        std::fs::create_dir_all(&cache_path)?;
        let node = overnet_core::Router::new(None)?;
        let target = ffx_target::resolve_default_target(&context).await?;
        let injector = Box::new(Injection::new(
            context.clone(),
            DaemonVersionCheck::CheckApiLevel(
                version_history::HISTORY.get_misleading_version_for_ffx().api_level,
            ),
            node,
            None,
            target,
        ));
        Ok(Self { context, injector, lib_ctx })
    }

    pub async fn connect_daemon_protocol(&self, protocol: String) -> Result<zx_types::zx_handle_t> {
        let proxy = self.injector.daemon_factory().await?;
        let (client, server) = fidl::Channel::create();
        proxy.connect_to_protocol(protocol.as_str(), server).await?.map_err(fxe)?;
        let res = client.raw_handle();
        std::mem::forget(client);
        Ok(res)
    }

    pub async fn connect_target_proxy(&self) -> Result<zx_types::zx_handle_t> {
        let proxy = self.injector.target_factory().await?;
        let hdl = proxy.into_channel().map_err(fxe)?.into_zx_channel();
        let res = hdl.raw_handle();
        std::mem::forget(hdl);
        Ok(res)
    }

    pub async fn target_proxy_factory(&self) -> Result<TargetProxy> {
        self.injector.target_factory().await
    }

    pub async fn target_collection_proxy_factory(&self) -> Result<TargetCollectionProxy> {
        let daemon = self.injector.daemon_factory().await?;
        let (client, server) = fidl::endpoints::create_proxy::<TargetCollectionMarker>()?;
        let protocol = <TargetCollectionMarker as DiscoverableProtocolMarker>::PROTOCOL_NAME;
        daemon
            .connect_to_protocol(protocol, server.into_channel())
            .await?
            .map_err(|err| map_daemon_error(protocol, err))?;
        Ok(client)
    }

    pub async fn target_add(
        &self,
        addr: IpAddr,
        scope_id: u32,
        port: u16,
        wait: bool,
    ) -> Result<()> {
        let tc_proxy = self.target_collection_proxy_factory().await?;
        add_manual_target(&tc_proxy, addr, scope_id, port, wait).await
    }

    pub async fn connect_remote_control_proxy(&self) -> Result<zx_types::zx_handle_t> {
        let target = ffx_target::resolve_default_target(&self.context).await?;
        let is_default_target = target.is_none();
        let daemon = self.injector.daemon_factory().await?;
        let timeout = self.context.get_proxy_timeout().await?;
        let proxy = ffx_target::get_remote_proxy(
            target,
            is_default_target,
            daemon,
            timeout,
            None,
            &self.context,
        )
        .await?;
        let hdl = proxy.into_channel().map_err(fxe)?.into_zx_channel();
        let res = hdl.raw_handle();
        std::mem::forget(hdl);
        Ok(res)
    }

    pub async fn connect_device_proxy(
        &self,
        moniker: String,
        capability_name: String,
    ) -> Result<zx_types::zx_handle_t> {
        let rcs_proxy = self.injector.remote_factory().await?;
        let (hdl, server) = fidl::Channel::create();
        rcs::connect_with_timeout_at(
            Duration::from_secs(15),
            &moniker,
            &capability_name,
            &rcs_proxy,
            server,
        )
        .await?;
        let res = hdl.raw_handle();
        std::mem::forget(hdl);
        Ok(res)
    }
}
