// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::LibContext;
use anyhow::Result;
use errors::ffx_error;
use ffx_config::environment::ExecutableKind;
use ffx_config::EnvironmentContext;
use ffx_core::Injector;
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use fidl::endpoints::Proxy;
use fidl::AsHandleRef;
use fuchsia_zircon_types as zx_types;
use hoist::Hoist;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tempfile::TempDir;

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
    _hoist_cache_dir: TempDir,
    #[allow(unused)]
    context: EnvironmentContext,
}

impl EnvContext {
    pub(crate) fn write_err<T: std::fmt::Debug>(&self, err: T) {
        let lib = self.lib_ctx.upgrade().expect("library context instance deallocated early");
        lib.write_err(err)
    }

    pub(crate) fn lib_ctx(&self) -> Arc<LibContext> {
        self.lib_ctx.upgrade().expect("library context instance deallocated early")
    }

    pub async fn new(lib_ctx: Weak<LibContext>, config: Vec<FfxConfigEntry>) -> Result<Self> {
        // TODO(fxbug.dev/129230): This is a lot of potentially unnecessary data transformation
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
        let context = EnvironmentContext::detect(
            ExecutableKind::Test,
            runtime_args,
            &std::env::current_dir()?,
            env_path,
        )
        .map_err(fxe)?;
        let cache_path = context.get_cache_path()?;
        std::fs::create_dir_all(&cache_path)?;
        let _hoist_cache_dir = tempfile::tempdir_in(&cache_path)?;
        let hoist = Hoist::with_cache_dir_maybe_router(_hoist_cache_dir.path(), None)?;
        let target = ffx_target::resolve_default_target(&context).await?;
        let injector = Box::new(Injection::new(
            context.clone(),
            DaemonVersionCheck::CheckApiLevel(version_history::LATEST_VERSION.api_level),
            hoist.clone(),
            None,
            target,
        ));
        Ok(Self { context, injector, _hoist_cache_dir, lib_ctx })
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

    pub async fn connect_remote_control_proxy(&self) -> Result<zx_types::zx_handle_t> {
        let proxy = self.injector.remote_factory().await?;
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
