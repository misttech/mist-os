// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod mocks;
mod realm_factory;
use crate::realm_factory::*;

use anyhow::{Error, Result};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_test_sampler::{RealmFactoryRequest, RealmFactoryRequestStream};
use fuchsia_component::client;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use log::error;

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
    let id_gen = sandbox::CapabilityIdGenerator::new();
    let store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>().unwrap();
    let result: Result<(), Error> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                RealmFactoryRequest::CreateRealm { options, dictionary, responder } => {
                    let realm = create_realm(options).await?;
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
                    responder.send(Ok(()))?;
                }

                RealmFactoryRequest::_UnknownMethod { .. } => todo!(),
            }
        }
        Ok(())
    }
    .await;

    if let Err(err) = result {
        error!("{:?}", err);
    }
}
