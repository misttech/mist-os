// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl::endpoints;
use fidl_test_echoserver::{RealmFactoryRequest, RealmFactoryRequestStream, RealmOptions};
use fuchsia_component::client;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use futures::{StreamExt, TryStreamExt};
use log::*;
use {
    fidl_fidl_examples_routing_echo as fecho, fidl_fuchsia_component_sandbox as fsandbox,
    fuchsia_async as fasync,
};

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(mut stream: RealmFactoryRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let id_gen = sandbox::CapabilityIdGenerator::new();
    let result: Result<(), Error> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                RealmFactoryRequest::CreateRealm { options, responder } => {
                    let realm = create_realm(options).await?;

                    // Get a dict containing the capabilities exposed by the realm.
                    let exposed_dict =
                        realm.root.controller().get_exposed_dictionary().await?.unwrap();
                    let source_dict_id = id_gen.next();
                    store
                        .import(source_dict_id, fsandbox::Capability::Dictionary(exposed_dict))
                        .await
                        .unwrap()
                        .unwrap();
                    let dict_id = id_gen.next();
                    store.dictionary_copy(source_dict_id, dict_id).await?.unwrap();

                    // Mix in additional capabilities to the dict.
                    //
                    // TODO(https://fxbug.dev/298100106): Could RealmInstance expose a higher-level
                    // API for getting the realm's exposed dict, along with APIs that make it
                    // easy to add in more capabilities served by this component? For example:
                    //
                    // let mut bundle = realm.get_root_bundle().extend().await?.unwrap();
                    // let echo_request_stream =
                    //     bundles::add_fidl_service::<fecho::EchoMarker>(&bundle);
                    // "serves" the bundle over `bundle_server` which was sent from the client.
                    // Also moves `bundle` so it can't be modified further.
                    // bundle.serve(bundle_server).await?.unwrap();
                    //
                    // ... code to serve echo here ...

                    let (echo_receiver_client, echo_receiver_stream) =
                        endpoints::create_request_stream::<fsandbox::ReceiverMarker>();
                    let connector_id = id_gen.next();
                    store.connector_create(connector_id, echo_receiver_client).await?.unwrap();
                    store
                        .dictionary_insert(
                            dict_id,
                            &fsandbox::DictionaryItem {
                                key: "reverse-echo".into(),
                                value: connector_id,
                            },
                        )
                        .await?
                        .unwrap();
                    let (dictionary, server) = endpoints::create_endpoints();
                    store
                        .dictionary_legacy_export(dict_id, server.into_channel())
                        .await
                        .unwrap()
                        .unwrap();

                    // Serve the mixed-in capability.
                    task_group.spawn(async move {
                        let _realm = realm;
                        let _ = realm_proxy::service::handle_receiver::<fecho::EchoMarker, _, _>(
                            echo_receiver_stream,
                            handle_echo_request_stream,
                        )
                        .await
                        .map_err(|e| {
                            error!("Failed to serve echo stream: {}", e);
                        });
                    });

                    responder.send(Ok(dictionary))?;
                }
                RealmFactoryRequest::_UnknownMethod { .. } => unimplemented!(),
            }
        }

        task_group.join().await;
        Ok(())
    }
    .await;

    if let Err(err) = result {
        error!("{:?}", err);
    }
}

async fn create_realm(options: RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);

    let builder = RealmBuilder::new().await?;
    let echo =
        builder.add_child("echo", "echo_server#meta/default.cm", ChildOptions::new()).await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fecho::EchoMarker>())
                .from(&echo)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;
    Ok(realm)
}

async fn handle_echo_request_stream(mut stream: fecho::EchoRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fecho::EchoRequest::EchoString { value, responder } => {
                let value = value.map(|value| {
                    let mut reversed = String::with_capacity(value.len());
                    let mut chars: Vec<_> = value.chars().collect();
                    chars.reverse();
                    for ch in chars {
                        reversed.push(ch);
                    }
                    reversed
                });
                responder.send(value.as_deref()).unwrap();
            }
        }
    }
}
