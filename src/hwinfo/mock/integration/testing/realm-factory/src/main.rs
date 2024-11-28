// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl::endpoints::{ControlHandle, DiscoverableProtocolMarker};
use fidl_test_mock::{RealmFactoryRequest, RealmFactoryRequestStream, RealmOptions};
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use futures::{StreamExt, TryStreamExt};
use tracing::*;
use {
    fidl_fuchsia_hwinfo as fhwinfo, fidl_fuchsia_hwinfo_mock as fhwinfo_mock,
    fuchsia_async as fasync, zx_status,
};

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    info!("starting");

    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(mut stream: RealmFactoryRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    let result: Result<(), Error> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                RealmFactoryRequest::_UnknownMethod { control_handle, .. } => {
                    control_handle.shutdown_with_epitaph(zx_status::Status::NOT_SUPPORTED);
                    return Ok(());
                }
                RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                    let realm = create_realm(options).await?;
                    let request_stream = realm_server.into_stream();
                    task_group.spawn(async move {
                        realm_proxy::service::serve(realm, request_stream).await.unwrap();
                    });
                    responder.send(Ok(()))?;
                }
            }
        }
        task_group.join().await;
        Ok(())
    }
    .await;
    if let Err(err) = result {
        error!("RealmFactory server loop exited with error: {:?}", err);
    }
}

async fn create_realm(options: RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);

    let builder = RealmBuilder::new().await?;

    let mock = builder.add_child("mock", "hwinfo-mock#meta/mock.cm", ChildOptions::new()).await?;
    for protocol in vec![
        fhwinfo::BoardMarker::PROTOCOL_NAME,
        fhwinfo::ProductMarker::PROTOCOL_NAME,
        fhwinfo::DeviceMarker::PROTOCOL_NAME,
        fhwinfo_mock::SetterMarker::PROTOCOL_NAME,
    ] {
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name(protocol))
                    .from(&mock)
                    .to(Ref::parent()),
            )
            .await?;
    }

    let realm = builder.build().await?;
    Ok(realm)
}
