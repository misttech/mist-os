// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod realm_factory;

use anyhow::{Context, Result};
use fidl::endpoints::{self, ClientEnd, ControlHandle, Proxy};
use fidl_fuchsia_metrics_test as ffmt;
use fidl_test_time_realm as fttr;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_zircon_status as zx_status;
use futures::{StreamExt, TryStreamExt};
use std::sync::Arc;
use timekeeper_integration_lib::{PushSourcePuppet, RtcUpdates};

#[fuchsia::main(logging_tags = [ "test", "timekeeper", "test-realm-factory" ])]
async fn main() -> Result<()> {
    tracing::debug!("starting timekeeper test realm factory");
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: fttr::RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    // Since we will typically create a single test realm per test case,
    // we don't need to serve this concurrently.
    fs.for_each(serve_realm_factory).await;
    tracing::debug!("stopping timekeeper test realm factory");
    Ok(())
}

/// Serves the FIDL API for RTC updates.
async fn serve_rtc_updates(
    rtc_updates: RtcUpdates,
    mut stream: fttr::RtcUpdatesRequestStream,
) -> Result<()> {
    while let Some(Ok(request)) = stream.next().await {
        match request {
            fttr::RtcUpdatesRequest::Get { responder, .. } => {
                let updates = rtc_updates.to_vec();
                responder
                    .send(Ok((&updates[..], Default::default())))
                    .context("while responding to RtcUpdatesRequest.Get")?;
            }
            m @ fttr::RtcUpdatesRequest::_UnknownMethod { .. } => {
                // Have we expanded the FIDL API, but forgotten to add an implementation?
                tracing::warn!("RtcUpdates endpoint received an unknown FIDL request: {:?}", &m);
            }
        }
    }
    tracing::debug!("serve_push_puppet is terminating - all connections will be closed");
    Ok(())
}

/// Serves the FIDL API for controlling PushSourcePuppet.
async fn serve_push_puppet(
    puppet: Arc<PushSourcePuppet>,
    mut stream: fttr::PushSourcePuppetRequestStream,
) -> Result<()> {
    while let Some(Ok(request)) = stream.next().await {
        match request {
            fttr::PushSourcePuppetRequest::Crash { responder, .. } => {
                tracing::debug!("simulating crash...");
                puppet.simulate_crash();
                responder.send()?;
            }
            fttr::PushSourcePuppetRequest::GetLifetimeServedConnections { responder, .. } => {
                responder.send(puppet.lifetime_served_connections())?;
            }
            fttr::PushSourcePuppetRequest::SetSample { sample, responder, .. } => {
                tracing::debug!("setting sample: {:?}", &sample);
                puppet.set_sample(sample).await;
                responder.send()?;
            }
            fttr::PushSourcePuppetRequest::SetStatus { status, responder, .. } => {
                tracing::debug!("setting status: {:?}", &status);
                puppet.set_status(status).await;
                responder.send()?;
            }
            m @ fttr::PushSourcePuppetRequest::_UnknownMethod { .. } => {
                // Have we expanded the FIDL API, but forgotten to add an implementation?
                tracing::warn!(
                    "PushSourcePuppet endpoint received an unknown FIDL request: {:?}",
                    &m
                );
            }
        }
    }
    tracing::debug!("serve_push_puppet is terminating - all connections will be closed");
    Ok(())
}

/// Blocks and serves a standard Realm Factory.
async fn serve_realm_factory(mut stream: fttr::RealmFactoryRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    let result: Result<()> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            tracing::debug!("received a request: {:?}", &request);
            match request {
                fttr::RealmFactoryRequest::_UnknownMethod { control_handle, .. } => {
                    control_handle.shutdown_with_epitaph(zx_status::Status::NOT_SUPPORTED);
                    unimplemented!();
                }
                fttr::RealmFactoryRequest::CreateRealm {
                    options,
                    realm_server,
                    responder,
                    fake_utc_clock,
                } => {
                    let (
                        realm,
                        push_source_puppet,
                        _rtc_updates,
                        cobalt_proxy,
                        _fake_clock_controller,
                    ) = realm_factory::create_realm(options, fake_utc_clock).await?;

                    assert!(
                        _fake_clock_controller.is_none(),
                        "these tests never use the fake clock controller"
                    );

                    let (push_source_puppet_client_end, push_source_request_stream) =
                        endpoints::create_request_stream().expect("infallible");
                    task_group.spawn(async move {
                        serve_push_puppet(push_source_puppet, push_source_request_stream)
                            .await
                            .expect("serve_push_puppet should not return an error");
                    });

                    // Needs to be served by the test fixture.
                    let (rtc_updates, _rtc_updates_stream) =
                        endpoints::create_request_stream().expect("infallible");
                    task_group.spawn(async move {
                        serve_rtc_updates(_rtc_updates, _rtc_updates_stream)
                            .await
                            .expect("serve_rtc_updates should not return an error");
                    });

                    let realm_request_stream = realm_server.into_stream()?;
                    task_group.spawn(async move {
                        realm_proxy::service::serve(realm, realm_request_stream)
                            .await
                            .expect("realm_proxy::service::serve should not return an error");
                    });

                    // Leave the type annotation in place, it helps not to mix up the parameters in
                    // a call to `responder.send()` below.
                    let cobalt_metric_client: ClientEnd<ffmt::MetricEventLoggerQuerierMarker> =
                        ClientEnd::new(
                            cobalt_proxy.into_channel().expect("infallible").into_zx_channel(),
                        );

                    responder.send(Ok((
                        push_source_puppet_client_end,
                        fttr::CreateResponseOpts {
                            rtc_updates: rtc_updates.into(),
                            ..Default::default()
                        },
                        cobalt_metric_client,
                    )))?;
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
