// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{Error, Result},
    config_lib::Config,
    fidl_fuchsia_testing_harness::RealmProxy_RequestStream,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    futures::{StreamExt, TryFutureExt},
    realm_proxy::service::{handle_request_stream, RealmInstanceProxy},
    tracing::{error, info},
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
    RealmProxy(RealmProxy_RequestStream),
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    info!("starting");

    let config = ConfigValues::from(Config::take_from_startup_handle());

    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingService::RealmProxy);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |IncomingService::RealmProxy(stream)| {
        run_server(config.clone(), stream).unwrap_or_else(|e| error!("{:?}", e))
    })
    .await;

    Ok(())
}

async fn run_server(config: ConfigValues, stream: RealmProxy_RequestStream) -> Result<(), Error> {
    let realm = build_realm(config).await?;
    let proxy = RealmInstanceProxy::from_instance(realm);
    handle_request_stream(proxy, stream).await?;

    Ok(())
}

pub async fn build_realm(config: ConfigValues) -> Result<RealmInstance, Error> {
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
