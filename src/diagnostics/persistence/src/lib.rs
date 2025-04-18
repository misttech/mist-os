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

use anyhow::{bail, format_err, Context, Error};
use argh::FromArgs;
use fetcher::Fetcher;
use fidl::endpoints;
use fuchsia_component::client;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::component;
use fuchsia_inspect::health::Reporter;
use futures::{StreamExt, TryStreamExt};
use log::*;
use persist_server::PersistServer;
use persistence_config::Config;
use scheduler::Scheduler;
use zx::BootInstant;
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

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
    let mut health = component::health();
    let config =
        on_error!(persistence_config::load_configuration_files(), "Error loading configs: {}")?;
    let inspector = component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    file_handler::forget_old_data(&config);

    // Create the Inspect fetcher
    let (fetch_requester, _fetcher_task) =
        on_error!(Fetcher::new(&config), "Error initializing fetcher: {}")?;

    let scheduler = Scheduler::new(fetch_requester, &config);

    // Add a persistence fidl service for each service defined in the config files.
    let scope = fasync::Scope::new();
    let services_scope = scope.new_child_with_name("services");

    let _service_scopes = spawn_persist_services(&config, scheduler, &services_scope)
        .await
        .expect("Error spawning persist services");

    // Before serving previous data, wait until the post-boot system update check has finished.
    // Note: We're already accepting persist requests. If we receive a request, store
    // some data, and then cache is cleared after data is persisted, that data will be lost. This
    // is correct behavior - we don't want to remember anything from before the cache was cleared.
    scope.spawn(async move {
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
            inspect_server::serve_persisted_data(node);
            health.set_ok();
            info!("Diagnostics Persistence Service ready");
        });
        inspector.root().record_int(PUBLISHED_TIME_KEY, BootInstant::get().into_nanos());
    });

    scope.await;

    Ok(())
}

enum IncomingRequest {
    Router(fsandbox::DictionaryRouterRequestStream),
}

// Serve a DataPersistence capability for each service defined in `config` using
// a dynamic dictionary.
async fn spawn_persist_services(
    config: &Config,
    scheduler: Scheduler,
    scope: &fasync::Scope,
) -> Result<Vec<fasync::Scope>, Error> {
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let id_gen = sandbox::CapabilityIdGenerator::new();

    let services_dict = id_gen.next();
    store
        .dictionary_create(services_dict)
        .await
        .context("Failed to send FIDL to create dictionary")?
        .map_err(|e| format_err!("Failed to create dictionary: {e:?}"))?;

    // Register each service with the exposed CFv2 dynamic dictionary.
    let mut service_scopes = Vec::with_capacity(config.len());

    for (service_name, tags) in config {
        let connector_id = id_gen.next();
        let (receiver, receiver_stream) =
            endpoints::create_request_stream::<fsandbox::ReceiverMarker>();

        store
            .connector_create(connector_id, receiver)
            .await
            .context("Failed to send FIDL to create connector")?
            .map_err(|e| format_err!("Failed to create connector: {e:?}"))?;

        store
            .dictionary_insert(
                services_dict,
                &fsandbox::DictionaryItem {
                    key: format!("{}-{}", constants::PERSIST_SERVICE_NAME_PREFIX, service_name),
                    value: connector_id,
                },
            )
            .await
            .context(
                "Failed to send FIDL to insert into diagnostics-persist-capabilities dictionary",
            )?
            .map_err(|e| {
                format_err!(
                    "Failed to insert into diagnostics-persist-capabilities dictionary: {e:?}"
                )
            })?;

        let service_scope = scope.new_child_with_name(service_name);
        PersistServer::spawn(
            service_name.clone(),
            tags.keys().cloned().collect(),
            scheduler.clone(),
            &service_scope,
            receiver_stream,
        );
        service_scopes.push(service_scope);
    }

    // Expose the dynamic dictionary.
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingRequest::Router);
    fs.take_and_serve_directory_handle().expect("Failed to take service directory handle");
    scope.spawn(fs.for_each_concurrent(None, move |IncomingRequest::Router(mut stream)| {
        let store = store.clone();
        let id_gen = id_gen.clone();
        async move {
            while let Ok(Some(request)) = stream.try_next().await {
                match request {
                    fsandbox::DictionaryRouterRequest::Route { payload: _, responder } => {
                        let dup_dict_id = id_gen.next();
                        store.duplicate(services_dict, dup_dict_id).await.unwrap().unwrap();
                        let capability = store.export(dup_dict_id).await.unwrap().unwrap();
                        let fsandbox::Capability::Dictionary(dict) = capability else {
                            panic!("capability was not a dictionary? {capability:?}");
                        };
                        let _ = responder
                            .send(Ok(fsandbox::DictionaryRouterRouteResponse::Dictionary(dict)));
                    }
                    fsandbox::DictionaryRouterRequest::_UnknownMethod { ordinal, .. } => {
                        warn!(ordinal:%; "Unknown DictionaryRouter request");
                    }
                }
            }
        }
    }));

    Ok(service_scopes)
}
