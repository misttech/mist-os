// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod realm_factory;

use anyhow::Result;
use fidl::endpoints::ControlHandle;
use fidl_test_time_realm::{CreateResponse, RealmFactoryRequest, RealmFactoryRequestStream};
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_zircon_status as zx_status;
use futures::{StreamExt, TryStreamExt};

#[fuchsia::main(logging_tags = [ "timekeeper", "test-realm-factory" ])]
async fn main() -> Result<()> {
    tracing::debug!("starting timekeeper test realm factory");
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    // Since we will typically create a single test realm per test case,
    // we don't need to serve this concurrently.
    fs.for_each(serve_realm_factory).await;
    Ok(())
}

/// Blocks and serves a standard Realm Factory.
async fn serve_realm_factory(mut stream: RealmFactoryRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    let result: Result<()> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            tracing::debug!("received a request: {:?}", &request);
            match request {
                RealmFactoryRequest::_UnknownMethod { control_handle, .. } => {
                    control_handle.shutdown_with_epitaph(zx_status::Status::NOT_SUPPORTED);
                    unimplemented!();
                }
                RealmFactoryRequest::CreateRealm {
                    options,
                    realm_server,
                    responder,
                    fake_utc_clock,
                } => {
                    let realm = realm_factory::create_realm(options, fake_utc_clock).await?;
                    let request_stream = realm_server.into_stream()?;
                    task_group.spawn(async move {
                        realm_proxy::service::serve(realm, request_stream).await.unwrap();
                    });
                    responder.send(Ok(CreateResponse::default()))?;
                }
            }
        }

        tracing::debug!("waiting for the realms to complete");
        task_group.join().await;
        Ok(())
    }
    .await;

    if let Err(err) = result {
        // we panic to ensure test failure.
        panic!("{:?}", err);
    }
}
