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

    pub async fn new(lib_ctx: Weak<LibContext>) -> Result<Self> {
        let runtime_args = ffx_config::runtime::populate_runtime(&[], None)?;
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
        let version_info = ffx_build_version::build_info();
        let injector = Box::new(Injection::new(
            context.clone(),
            DaemonVersionCheck::SameVersionInfo(version_info),
            hoist.clone(),
            None,
            None,
        ));
        Ok(Self { context, injector, _hoist_cache_dir, lib_ctx })
    }

    pub async fn open_daemon_protocol(&self, protocol: String) -> Result<zx_types::zx_handle_t> {
        let proxy = self.injector.daemon_factory().await?;
        let (client, server) = fidl::Channel::create();
        proxy.connect_to_protocol(protocol.as_str(), server).await?.map_err(fxe)?;
        let res = client.raw_handle();
        std::mem::forget(client);
        Ok(res)
    }

    pub async fn open_target_proxy(&self) -> Result<zx_types::zx_handle_t> {
        let proxy = self.injector.target_factory().await?;
        let hdl = proxy.into_channel().map_err(fxe)?.into_zx_channel();
        let res = hdl.raw_handle();
        std::mem::forget(hdl);
        Ok(res)
    }

    pub async fn open_remote_control_proxy(&self) -> Result<zx_types::zx_handle_t> {
        let proxy = self.injector.remote_factory().await?;
        let hdl = proxy.into_channel().map_err(fxe)?.into_zx_channel();
        let res = hdl.raw_handle();
        std::mem::forget(hdl);
        Ok(res)
    }

    pub async fn open_device_proxy(
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
