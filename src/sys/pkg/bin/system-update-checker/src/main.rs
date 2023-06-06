// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![allow(clippy::let_unit_value)]
#![allow(clippy::type_complexity)]

mod apply;
mod channel;
mod channel_handler;
mod check;
mod connect;
mod errors;
mod poller;
mod rate_limiter;
mod update_manager;
mod update_monitor;
mod update_service;

use {
    crate::{
        channel_handler::ChannelHandler,
        poller::run_periodic_update_check,
        update_service::{RealUpdateManager, UpdateService},
    },
    anyhow::{anyhow, Context as _, Error},
    fidl_fuchsia_update_channel::ProviderRequestStream,
    fidl_fuchsia_update_channelcontrol::ChannelControlRequestStream,
    fidl_fuchsia_update_ext::{CheckOptions, Initiator},
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect as finspect,
    fuchsia_url::AbsolutePackageUrl,
    fuchsia_zircon as zx,
    futures::{prelude::*, stream::FuturesUnordered},
    std::{sync::Arc, time::Duration},
    tracing::{error, warn},
};

const MAX_CONCURRENT_CONNECTIONS: usize = 100;
const DEFAULT_UPDATE_PACKAGE_URL: &str = "fuchsia-pkg://fuchsia.com/update";

/// Static service configuration options.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Config {
    poll_frequency: Option<zx::Duration>,
    update_package_url: Option<AbsolutePackageUrl>,
}

impl Config {
    pub fn poll_frequency(&self) -> Option<zx::Duration> {
        self.poll_frequency
    }

    pub fn update_package_url(&self) -> Option<&AbsolutePackageUrl> {
        self.update_package_url.as_ref()
    }
}

#[fuchsia::main(threads = 1, logging_tags = ["system-update-checker"])]
async fn main() -> Result<(), Error> {
    main_inner().await.map_err(|err| {
        // Use anyhow to print the error chain.
        let err = anyhow!(err);
        error!("error running system-update-checker: {:#}", err);
        err
    })
}

async fn main_inner() -> Result<(), Error> {
    let config = Config::default();
    if let Some(url) = config.update_package_url() {
        warn!("Ignoring custom update package url: {}", url);
    }

    let inspector = finspect::Inspector::default();

    let target_channel_manager =
        channel::TargetChannelManager::new(connect::ServiceConnector, "/config/data");
    if let Err(e) = target_channel_manager.update().await {
        error!("while updating the target channel: {:#}", anyhow!(e));
    }
    let target_channel_manager = Arc::new(target_channel_manager);

    let futures = FuturesUnordered::new();

    let (current_channel_manager, current_channel_notifier) =
        channel::build_current_channel_manager_and_notifier(connect::ServiceConnector).await?;
    futures.push(current_channel_notifier.run().boxed());
    let current_channel_manager = Arc::new(current_channel_manager);

    let (mut update_manager, update_manager_fut) = RealUpdateManager::new(
        Arc::clone(&target_channel_manager),
        inspector.root().create_child("update-manager"),
    )
    .await
    .start();
    futures.push(update_manager_fut.boxed());

    let mut fs = ServiceFs::new();
    let update_manager_clone = update_manager.clone();
    let channel_handler =
        Arc::new(ChannelHandler::new(current_channel_manager, target_channel_manager));
    let channel_handler_clone = Arc::clone(&channel_handler);
    let channel_handler_provider_clone = Arc::clone(&channel_handler);

    fs.dir("svc")
        .add_fidl_service(move |stream| {
            IncomingServices::Manager(stream, UpdateService::new(update_manager_clone.clone()))
        })
        .add_fidl_service(move |stream| {
            IncomingServices::Provider(stream, Arc::clone(&channel_handler_provider_clone))
        })
        .add_fidl_service(move |stream| {
            IncomingServices::ChannelControl(stream, Arc::clone(&channel_handler_clone))
        });

    inspect_runtime::serve(&inspector, &mut fs)?;

    fs.take_and_serve_directory_handle().context("ServiceFs::take_and_serve_directory_handle")?;
    futures.push(
        fs.for_each_concurrent(MAX_CONCURRENT_CONNECTIONS, |incoming_service| {
            handle_incoming_service(incoming_service)
                .unwrap_or_else(|e| error!("error handling client connection: {:#}", anyhow!(e)))
        })
        .boxed(),
    );

    futures.push(run_periodic_update_check(update_manager.clone(), &config).boxed());

    // In order to ensure cobalt gets a valid channel quickly, we sometimes perform an additional
    // update check 60 seconds in.
    futures.push(
        async move {
            if config.poll_frequency().is_some() {
                fasync::Timer::new(Duration::from_secs(60)).await;
                let options = CheckOptions::builder().initiator(Initiator::Service).build();
                if let Err(e) = update_manager.try_start_update(options, None).await {
                    warn!("Update check failed with error: {:?}", e);
                }
            }
        }
        .boxed(),
    );

    futures.collect::<()>().await;

    Ok(())
}

enum IncomingServices {
    Manager(fidl_fuchsia_update::ManagerRequestStream, UpdateService),
    Provider(ProviderRequestStream, Arc<ChannelHandler>),
    ChannelControl(ChannelControlRequestStream, Arc<ChannelHandler>),
}

async fn handle_incoming_service(incoming_service: IncomingServices) -> Result<(), Error> {
    match incoming_service {
        IncomingServices::Manager(request_stream, mut update_service) => {
            update_service.handle_request_stream(request_stream).await
        }
        IncomingServices::Provider(request_stream, handler) => {
            handler.handle_provider_request_stream(request_stream).await
        }
        IncomingServices::ChannelControl(request_stream, handler) => {
            handler.handle_control_request_stream(request_stream).await
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
pub struct ConfigBuilder(Config);

#[cfg(test)]
impl ConfigBuilder {
    pub fn new() -> Self {
        Self(Config::default())
    }

    pub fn poll_frequency(mut self, duration: impl Into<zx::Duration>) -> Self {
        self.0.poll_frequency = Some(duration.into());
        self
    }

    pub fn build(self) -> Config {
        self.0
    }
}

#[cfg(test)]
impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}
