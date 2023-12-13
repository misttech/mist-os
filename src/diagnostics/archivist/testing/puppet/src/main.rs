// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This program serves the fuchsia.archivist.test.Puppet protocol.
//
// It is meant to be controlled by a test suite and will emit log messages
// and inspect data as requested. This output can be retrieved from the
// archivist under test using fuchsia.diagnostics.ArchiveAccessor.
//
// For full documentation, see //src/diagnostics/archivist/testing/realm-factory/README.md

use anyhow::Error;
use diagnostics_hierarchy::Property;
use fidl::endpoints::create_request_stream;
use fuchsia_async::Timer;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter, Inspector};
use fuchsia_zircon::Duration;
use futures::{FutureExt, StreamExt, TryStreamExt};
use tracing::error;

use fidl_fuchsia_archivist_test as fpuppet;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    let _inspect_server_task = inspect_runtime::publish(
        component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );

    fs.dir("svc").add_fidl_service(|stream: fpuppet::PuppetRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_puppet).await;
    Ok(())
}

async fn serve_puppet(mut stream: fpuppet::PuppetRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        handle_puppet_request(request)
            .await
            .unwrap_or_else(|e| error!(?e, "handle_puppet_request"));
    }
}

async fn handle_puppet_request(request: fpuppet::PuppetRequest) -> Result<(), Error> {
    match request {
        fpuppet::PuppetRequest::EmitExampleInspectData { rows, columns, .. } => {
            inspect_testing::emit_example_inspect_data(inspect_testing::Options {
                rows: rows as usize,
                columns: columns as usize,
                extra_number: None,
            })
            .await?;
            Ok(())
        }
        fpuppet::PuppetRequest::RecordLazyValues { key, responder } => {
            let (client, requests) = create_request_stream()?;
            responder.send(client)?;
            record_lazy_values(key, requests).await?;
            Ok(())
        }
        fpuppet::PuppetRequest::RecordString { key, value, .. } => {
            component::inspector().root().record_string(key, value);
            Ok(())
        }
        fpuppet::PuppetRequest::RecordInt { key, value, .. } => {
            component::inspector().root().record_int(key, value);
            Ok(())
        }
        fpuppet::PuppetRequest::SetHealthOk { responder } => {
            component::health().set_ok();
            responder.send()?;
            Ok(())
        }
        fpuppet::PuppetRequest::Println { message, .. } => {
            println!("{message}");
            Ok(())
        }
        fpuppet::PuppetRequest::Eprintln { message, .. } => {
            eprintln!("{message}");
            Ok(())
        }
        fpuppet::PuppetRequest::_UnknownMethod { .. } => unreachable!(),
    }
}

// Converts InspectPuppet requests into callbacks that report inspect values lazily.
// The values aren't truly lazy since they're computed in the client before the inspect
// data is fetched. They're just lazily reported.
async fn record_lazy_values(
    key: String,
    mut stream: fpuppet::LazyInspectPuppetRequestStream,
) -> Result<(), Error> {
    let mut properties = vec![];
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fpuppet::LazyInspectPuppetRequest::RecordString { key, value, .. } => {
                properties.push(Property::String(key, value));
            }
            fpuppet::LazyInspectPuppetRequest::RecordInt { key, value, .. } => {
                properties.push(Property::Int(key, value));
            }
            fpuppet::LazyInspectPuppetRequest::Commit { options, .. } => {
                component::inspector().root().record_lazy_values(key, move || {
                    let properties = properties.clone();
                    async move {
                        if options.hang.unwrap_or_default() {
                            Timer::new(Duration::from_minutes(60)).await;
                        }
                        let inspector = Inspector::default();
                        let node = inspector.root();
                        for property in properties.iter() {
                            match property {
                                Property::String(k, v) => node.record_string(k, v),
                                Property::Int(k, v) => node.record_int(k, *v),
                                _ => unimplemented!(),
                            }
                        }
                        Ok(inspector)
                    }
                    .boxed()
                });

                return Ok(()); // drop the connection.
            }
            fpuppet::LazyInspectPuppetRequest::_UnknownMethod { .. } => unreachable!(),
            _ => unimplemented!(),
        };
    }

    Ok(())
}
