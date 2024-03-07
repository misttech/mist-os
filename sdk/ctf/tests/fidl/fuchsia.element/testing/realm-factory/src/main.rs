// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl::endpoints::ControlHandle,
    fidl_fuchsia_element::ManagerMarker,
    fidl_fuchsia_element_test::*,
    fidl_fuchsia_testing_harness::RealmProxy_RequestStream,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    fuchsia_zircon_status as zx_status,
    futures::{StreamExt, TryStreamExt},
    tracing::error,
};

enum IncomingService {
    RealmFactory(RealmFactoryRequestStream),
    RealmProxy(RealmProxy_RequestStream),
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc")
        .add_fidl_service(IncomingService::RealmFactory)
        .add_fidl_service(IncomingService::RealmProxy);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(service: IncomingService) {
    match service {
        IncomingService::RealmFactory(stream) => {
            if let Err(err) = handle_request_stream(stream).await {
                error!("{:?}", err);
            }
        }
        // TODO(299966655): Delete after we branch for F15.
        IncomingService::RealmProxy(stream) => {
            let options = RealmOptions::default();
            let realm = create_realm(options).await.expect("failed to build the test realm");
            realm_proxy::service::serve(realm, stream)
                .await
                .expect("failed to serve the realm proxy");
        }
    }
}

async fn handle_request_stream(mut stream: RealmFactoryRequestStream) -> Result<()> {
    let mut task_group = fasync::TaskGroup::new();
    let mut realms = vec![];

    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                let realm = create_realm(options).await?;
                let request_stream = realm_server.into_stream()?;
                task_group.spawn(async move {
                    realm_proxy::service::serve(realm, request_stream).await.unwrap();
                });
                responder.send(Ok(()))?;
            }
            RealmFactoryRequest::CreateRealm2 { options, dictionary, responder } => {
                let realm = create_realm(options).await?;
                realm.root.controller().get_exposed_dictionary(dictionary).await?.unwrap();
                realms.push(realm);
                responder.send(Ok(()))?;
            }

            RealmFactoryRequest::_UnknownMethod { control_handle, .. } => {
                control_handle.shutdown_with_epitaph(zx_status::Status::NOT_SUPPORTED);
                return Ok(());
            }
        }
    }

    task_group.join().await;
    Ok(())
}

async fn create_realm(_: RealmOptions) -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let session =
        builder.add_child("session", "#meta/reference-session.cm", ChildOptions::new()).await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<ManagerMarker>())
                .from(&session)
                .to(Ref::parent()),
        )
        .await?;
    Ok(builder.build().await?)
}
