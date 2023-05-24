// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{Error, Result},
    async_trait::async_trait,
    config_lib::Config,
    fidl_fuchsia_testing_harness::{
        RealmFactoryMarker, RealmFactoryRequest, RealmFactoryRequestStream,
        RealmProxy_RequestStream,
    },
    fidl_server::*,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    futures::StreamExt,
    tracing::info,
};

#[derive(Clone)]
pub struct ConfigValues {
    child_url: String,
    child_protocols: Vec<String>,
}

impl From<Config> for ConfigValues {
    fn from(value: Config) -> Self {
        Self { child_url: value.proxied_component_url, child_protocols: value.proxied_protocols }
    }
}

enum IncomingService {
    RealmFactory(RealmFactoryRequestStream),
    RealmProxy(RealmProxy_RequestStream),
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    info!("starting");

    let config = ConfigValues::from(Config::take_from_startup_handle());

    let mut fs = ServiceFs::new();
    fs.dir("svc")
        .add_fidl_service(IncomingService::RealmFactory)
        .add_fidl_service(IncomingService::RealmProxy);

    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |service| async {
        match service {
            IncomingService::RealmFactory(stream) => {
                let config = config.clone();
                serve_async_concurrent(stream, None, RealmFactoryServer::new(config))
                    .await
                    .expect("failed to serve the realm factory");
            }
            IncomingService::RealmProxy(stream) => {
                let config = config.clone();
                let realm = build_realm(config).await.expect("failed to build the test realm");
                realm_proxy::service::serve(realm, stream)
                    .await
                    .expect("failed to serve the realm proxy");
            }
        }
    })
    .await;

    Ok(())
}

// Implements the RealmFactory protocol.
struct RealmFactoryServer {
    config: ConfigValues,
}

impl RealmFactoryServer {
    fn new(config: ConfigValues) -> Self {
        Self { config }
    }
}

#[async_trait]
impl AsyncRequestHandler<RealmFactoryMarker> for RealmFactoryServer {
    async fn handle_request(&self, request: RealmFactoryRequest) -> Result<(), Error> {
        match request {
            RealmFactoryRequest::CreateRealm { realm_server, responder } => {
                let realm = build_realm(self.config.clone()).await?;
                let request_stream = realm_server.into_stream()?;
                responder.send(Ok(()))?;
                realm_proxy::service::serve(realm, request_stream).await.expect("serve");
            }
        }

        Ok(())
    }
}

async fn build_realm(config: ConfigValues) -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let child =
        builder.add_child("proxied_child", config.child_url, ChildOptions::new().eager()).await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .from(Ref::parent())
                .to(&child),
        )
        .await?;

    for protocol in config.child_protocols.iter() {
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name(protocol))
                    .from(&child)
                    .to(Ref::parent()),
            )
            .await?;
    }

    let instance = builder.build().await?;
    Ok(instance)
}
