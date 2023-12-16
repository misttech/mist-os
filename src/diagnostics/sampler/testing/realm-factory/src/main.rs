// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod mocks;
mod realm_factory;
use crate::realm_factory::*;

use {
    anyhow::{Error, Result},
    fidl_test_sampler::{RealmFactoryRequest, RealmFactoryRequestStream},
    fuchsia_component::server::ServiceFs,
    futures::{StreamExt, TryStreamExt},
    tracing::error,
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
    let mut realms = vec![];
    let result: Result<(), Error> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                RealmFactoryRequest::CreateRealm { options, dict_server, responder } => {
                    let realm = create_realm(options).await?;
                    realm.root.controller().get_exposed_dict(dict_server).await?.unwrap();
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
