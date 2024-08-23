// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_hardware_sensors_realm::{RealmFactoryRequest, RealmFactoryRequestStream};
use fuchsia_component::client;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use futures::{StreamExt, TryStreamExt};

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(mut stream: RealmFactoryRequestStream) {
    let mut realms = vec![];
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let id_gen = sandbox::CapabilityIdGenerator::new();

    let result: Result<(), Error> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                RealmFactoryRequest::CreateRealm { dictionary, responder } => {
                    let realm = create_realm().await?;
                    let dict_ref = realm.root.controller().get_exposed_dictionary().await?.unwrap();
                    let dict_id = id_gen.next();
                    store
                        .import(dict_id, fsandbox::Capability::Dictionary(dict_ref))
                        .await
                        .unwrap()
                        .unwrap();
                    store
                        .dictionary_legacy_export(dict_id, dictionary.into_channel())
                        .await
                        .unwrap()
                        .unwrap();
                    realms.push(realm);
                    let _ = responder.send(Ok(()));
                }
                RealmFactoryRequest::_UnknownMethod { .. } => unimplemented!(),
            }
        }

        Ok(())
    }
    .await;

    if let Err(err) = result {
        // Sensors tests allow error logs so we panic to ensure test failure.
        panic!("{:?}", err);
    }
}

async fn create_realm() -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;

    let playback_ref = builder
        .add_child(
            "sensors_playback",
            "sensors_playback_with_test_data#meta/sensors_playback.cm",
            ChildOptions::new(),
        )
        .await?;

    // Route the Driver and Playback protocols to the test root.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.hardware.sensors.Driver"))
                .capability(Capability::protocol_by_name("fuchsia.hardware.sensors.Playback"))
                .from(&playback_ref)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;
    Ok(realm)
}
