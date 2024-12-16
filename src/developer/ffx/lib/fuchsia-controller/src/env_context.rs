// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::LibContext;
use anyhow::Result;
use async_lock::Mutex;
use camino::Utf8PathBuf;
use errors::ffx_error;
use ffx_config::environment::ExecutableKind;
use ffx_config::EnvironmentContext;
use ffx_target::connection::Connection;
use ffx_target::ssh_connector::SshConnector;
use fidl::endpoints::Proxy;
use fidl::AsHandleRef;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;
use zx_types;

fn unspecified_target() -> anyhow::Error {
    anyhow::anyhow!(concat!(
        "no device has been specified for this `Context`. ",
        "A device must be specified in order to connect to the remote control proxy"
    ))
}

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
    target_spec: Option<String>,
    device_connection: Mutex<Option<Connection>>,
    pub(crate) context: EnvironmentContext,
}

async fn new_device_connection(
    ctx: &EnvironmentContext,
    target_spec: &Option<String>,
) -> Result<Connection> {
    let resolution = ffx_target::resolve_target_address(target_spec, ctx).await?;
    let addr = resolution.addr()?;
    let connector = SshConnector::new(addr, ctx).await?;
    Ok(Connection::new(connector).await?)
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
                false,
            )
            .map_err(fxe)?,
            None => EnvironmentContext::detect(
                ExecutableKind::Test,
                runtime_args,
                &current_dir,
                env_path,
                false,
            )
            .map_err(fxe)?,
        };
        let _ = ffx_config::init(&context);
        let cache_path = context.get_cache_path()?;
        std::fs::create_dir_all(&cache_path)?;
        let target_spec = ffx_target::get_target_specifier(&context).await?;
        let device_connection = Mutex::new(None);
        Ok(Self { context, device_connection, target_spec, lib_ctx })
    }

    async fn invariant_check(&self) -> Result<()> {
        if self.target_spec.is_none() {
            return Err(unspecified_target());
        }
        let mut device_connection = self.device_connection.lock().await;
        if device_connection.is_none() {
            *device_connection =
                Some(new_device_connection(&self.context, &self.target_spec).await?);
        }
        Ok(())
    }

    pub async fn connect_remote_control_proxy(&self) -> Result<zx_types::zx_handle_t> {
        self.invariant_check().await?;
        let proxy = self.device_connection.lock().await.as_ref().unwrap().rcs_proxy().await?;
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
        self.invariant_check().await?;
        let rcs_proxy = self.device_connection.lock().await.as_ref().unwrap().rcs_proxy().await?;
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

    pub async fn target_wait(&self, timeout: u64, offline: bool) -> Result<()> {
        let cmd = ffx_wait_args::WaitOptions { timeout, down: offline };
        let tool = ffx_wait::WaitOperation {
            cmd,
            env: self.context.clone(),
            waiter: ffx_wait::DeviceWaiterImpl,
        };
        tool.wait_impl().await.map_err(Into::into)
    }
}
