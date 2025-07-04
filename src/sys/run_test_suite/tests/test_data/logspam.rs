// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fuchsia_component::server as fserver;
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, Ref, Route,
};
use futures::stream::{self as stream, StreamExt, TryStreamExt};
use futures::TryFutureExt;
use log::Log;
use std::sync::Arc;
use {fidl_fidl_examples_routing_echo as fecho, fuchsia_async as fasync};

async fn echo_server_mock(handles: LocalComponentHandles) -> Result<(), Error> {
    // Create a new ServiceFs to host FIDL protocols from
    let mut fs = fserver::ServiceFs::new();
    let mut tasks = vec![];
    let log_client = handles.connect_to_protocol()?;
    let publisher = diagnostics_log::Publisher::new(
        diagnostics_log::PublisherOptions::default().use_log_sink(log_client),
    )?;
    let publisher = Arc::new(publisher);

    // Add the echo protocol to the ServiceFs
    fs.dir("svc").add_fidl_service(move |mut stream: fecho::EchoRequestStream| {
        let publisher_clone = publisher.clone();
        tasks.push(fasync::Task::local(async move {
            while let Some(fecho::EchoRequest::EchoString { value, responder }) =
                stream.try_next().await.expect("failed to serve echo service")
            {
                let mut builder = log::Record::builder();
                builder.level(log::Level::Info);
                publisher_clone
                    .log(&builder.args(format_args!("Got echo request: {:?}", value)).build());
                responder.send(value.as_ref().map(|s| &**s)).expect("failed to send echo response");
            }
        }));
    });

    // Run the ServiceFs on the outgoing directory handle from the mock handles
    fs.serve_connection(handles.outgoing_dir)?;
    fs.collect::<()>().await;

    Ok(())
}

/// Create and launch a child that spams 1000 logs. Launching a child to send logs allows
/// us to more effectively spam logs, since logging on fuchsia has safeguards against a single
/// component spamming too many logs.
async fn spam_logs_from_child() -> Result<(), Error> {
    let builder = RealmBuilder::new().await?;

    let echo_server = builder
        .add_local_child(
            "echo_server",
            move |handles: LocalComponentHandles| Box::pin(echo_server_mock(handles)),
            ChildOptions::new(),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .from(Ref::parent())
                .to(&echo_server),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fidl.examples.routing.echo.Echo"))
                .from(&echo_server)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let echo: fecho::EchoProxy = realm.root.connect_to_protocol_at_exposed_dir()?;

    for _ in 0..500 {
        assert_eq!(echo.echo_string(Some("hello")).await?, Some("hello".to_owned()));
    }

    Ok(())
}

#[fuchsia::test]
async fn spam_logs() {
    stream::repeat_with(|| spam_logs_from_child().unwrap_or_else(|e| panic!("{:?}", e)))
        .take(300)
        .buffer_unordered(100)
        .collect::<Vec<_>>()
        .await;
}
