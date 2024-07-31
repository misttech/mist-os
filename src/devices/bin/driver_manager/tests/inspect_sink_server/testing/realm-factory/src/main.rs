// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl_test_drivermanager::*;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{RealmBuilder, RealmInstance};
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use futures::{StreamExt, TryStreamExt};
use tracing::*;
use {fidl_fuchsia_driver_test as fdt, fuchsia_async as fasync};

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(stream: RealmFactoryRequestStream) {
    if let Err(err) = handle_request_stream(stream).await {
        error!("{:?}", err);
    }
}

async fn handle_request_stream(mut stream: RealmFactoryRequestStream) -> Result<()> {
    let mut task_group = fasync::TaskGroup::new();
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            RealmFactoryRequest::CreateRealm { realm_server, responder } => {
                let realm = create_realm().await?;
                let request_stream = realm_server.into_stream()?;
                task_group.spawn(async move {
                    realm_proxy::service::serve(realm, request_stream).await.unwrap();
                });
                responder.send(Ok(()))?;
            }

            RealmFactoryRequest::_UnknownMethod { .. } => unreachable!(),
        }
    }

    task_group.join().await;
    Ok(())
}

async fn create_realm() -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;
    let instance = builder.build().await?;
    instance.driver_test_realm_start(fdt::RealmArgs::default()).await?;
    Ok(instance)
}
