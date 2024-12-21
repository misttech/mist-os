// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! `diagnostics-persistence` component persists Inspect VMOs and serves them at the next boot.

mod constants;
mod fetcher;
mod file_handler;
mod inspect_server;
mod persist_server;
mod scheduler;

use anyhow::{bail, Error};
use argh::FromArgs;
use fetcher::Fetcher;
use fuchsia_async::TaskGroup;
use fuchsia_component::server::{ServiceFs, ServiceObj};
use fuchsia_inspect::component;
use fuchsia_inspect::health::Reporter;
use fuchsia_sync::Mutex;
use futures::future::join;
use futures::StreamExt;
use log::*;
use persist_server::PersistServer;
use persistence_config::Config;
use scheduler::Scheduler;
use std::sync::Arc;
use zx::BootInstant;

/// The name of the subcommand and the logs-tag.
pub const PROGRAM_NAME: &str = "persistence";
pub const PERSIST_NODE_NAME: &str = "persist";
/// Added after persisted data is fully published
pub const PUBLISHED_TIME_KEY: &str = "published";

/// Command line args
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "persistence")]
pub struct CommandLine {}

// on_error logs any errors from `value` and then returns a Result.
// value must return a Result; error_message must contain one {} to put the error in.
macro_rules! on_error {
    ($value:expr, $error_message:expr) => {
        $value.or_else(|e| {
            let message = format!($error_message, e);
            warn!("{}", message);
            bail!("{}", message)
        })
    };
}

pub async fn main(_args: CommandLine) -> Result<(), Error> {
    info!("Starting Diagnostics Persistence Service service");
    let config =
        on_error!(persistence_config::load_configuration_files(), "Error loading configs: {}")?;
    let inspector = component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    info!("Rotating directories");
    file_handler::shuffle_at_boot();

    let mut fs = ServiceFs::new();

    component::health().set_starting_up();

    // Create the Inspect fetcher
    let (fetch_requester, _fetcher_task) =
        on_error!(Fetcher::new(&config), "Error initializing fetcher: {}")?;

    let scheduler = Scheduler::new(fetch_requester, &config);

    // Add a persistence fidl service for each service defined in the config files.
    let _server_tasks = spawn_persist_services(config, &mut fs, scheduler);
    fs.take_and_serve_directory_handle()?;

    // Before serving previous data, wait until the post-boot system update check has finished.
    // Note: We're already accepting persist requess. If we receive a request, store
    // some data, and then cache is cleared after data is persisted, that data will be lost. This
    // is correct behavior - we don't want to remember anything from before the cache was cleared.
    let publish_fut = async move {
        info!("Waiting for post-boot update check...");
        match fuchsia_component::client::connect_to_protocol::<fidl_fuchsia_update::ListenerMarker>(
        ) {
            Ok(proxy) => match proxy.wait_for_first_update_check_to_complete().await {
                Ok(()) => {}
                Err(e) => {
                    warn!(e:?; "Error waiting for first update check; not publishing.");
                    return;
                }
            },
            Err(e) => {
                warn!(
                    e:?;
                    "Unable to connect to fuchsia.update.Listener; will publish immediately."
                );
            }
        }
        // Start serving previous boot data
        info!("...Update check has completed; publishing previous boot data");
        inspector.root().record_child(PERSIST_NODE_NAME, |node| {
            inspect_server::serve_persisted_data(node).unwrap_or_else(|e| {
                error!(
                    "{} {} {:?}",
                    "Serving persisted data experienced critical failure.", "No data available:", e,
                )
            });
            component::health().set_ok();
            info!("Diagnostics Persistence Service ready");
        });
        inspector.root().record_int(PUBLISHED_TIME_KEY, BootInstant::get().into_nanos());
    };

    join(fs.collect::<()>(), publish_fut).await;
    Ok(())
}

// Takes a config and adds all the persist services defined in those configs to the servicefs of
// the component.
#[must_use]
fn spawn_persist_services(
    config: Config,
    fs: &mut ServiceFs<ServiceObj<'static, ()>>,
    scheduler: Scheduler,
) -> Arc<Mutex<TaskGroup>> {
    let mut started_persist_services = 0;
    // We want fault tolerance if only a subset of the service configs fail to initialize.
    let task_holder = Arc::new(Mutex::new(TaskGroup::new()));
    for (service_name, tags) in config {
        info!("Launching persist service for {service_name}");
        PersistServer::create(
            service_name.clone(),
            tags.keys().cloned().collect(),
            scheduler.clone(),
        )
        .launch_server(task_holder.clone(), fs);
        started_persist_services += 1;
    }
    info!("Started {} persist services", started_persist_services);
    task_holder
}
