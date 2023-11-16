// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod realm_factory;

use anyhow::{Context, Result};
use fidl::endpoints::{self, ClientEnd, ControlHandle, Proxy};
use fidl_fuchsia_metrics_test as ffmt;
use fidl_fuchsia_testing as ffte;
use fidl_test_time_realm as fttr;
use fuchsia_async as fasync;
use fuchsia_component::{client, server::ServiceFs};
use fuchsia_zircon_status as zx_status;
use futures::{future::BoxFuture, FutureExt, StreamExt, TryStreamExt};
use std::sync::Arc;
use timekeeper_integration_lib::{PushSourcePuppet, RtcUpdates};

use fidl_fuchsia_testing as _; // TODO: fmil - Figure out why this is needed.

// Services served by this realm proxy.
enum Services {
    /// The test realm factory protocol.
    RealmFactory(fttr::RealmFactoryRequestStream),

    /// Fake clock readout.
    /// Forwarded to a component inside the `#realm_builder` collection.
    FakeClock(ffte::FakeClockRequestStream),

    /// A fake clock puppeteer interface. Used to manipulate the fake
    /// monotonic clock in tests.
    /// Forwarded to a component inside the `#realm_builder` collection.
    FakeClockControl(ffte::FakeClockControlRequestStream),
}

#[fuchsia::main(logging_tags = [ "test", "timekeeper-test-realm-factory" ])]
async fn main() -> Result<()> {
    tracing::debug!("starting timekeeper test realm factory");
    let mut fs = ServiceFs::new();
    fs.dir("svc")
        .add_fidl_service(Services::RealmFactory)
        .add_fidl_service(Services::FakeClock)
        .add_fidl_service(Services::FakeClockControl);
    fs.take_and_serve_directory_handle()?;
    // Since we will typically create a single test realm per test case,
    // we don't need to serve this in parallel.
    fs.for_each_concurrent(1, move |connection| match connection {
        Services::RealmFactory(stream) => serve_realm_factory(stream),
        Services::FakeClock(stream) => serve_fake_clock(stream),
        Services::FakeClockControl(stream) => serve_fake_clock_control(stream),
    })
    .await;
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

/// Converts the given proxy with a protocol marker `T` back into
/// the [ClientEnd] of the same type.
///
/// While the reverse conversion is straightforward, this conversion is a
/// bit convoluted, so here's a shortcut.
///
/// Type Args:
/// - `T`: a marker type for the specific protocol.
fn proxy_into_client_end<T>(proxy: T::Proxy) -> ClientEnd<T>
where
    T: endpoints::ProtocolMarker,
    <T as endpoints::ProtocolMarker>::Proxy: std::fmt::Debug,
{
    ClientEnd::new(proxy.into_channel().expect("infallible").into_zx_channel())
}

/// Returns a function that blocks and serves a fake clock request stream.
fn serve_fake_clock(mut stream: ffte::FakeClockRequestStream) -> BoxFuture<'static, ()> {
    async move {
        tracing::debug!("serve_fake_clock entry.");
        let _proxy = client::connect_to_protocol::<ffte::FakeClockMarker>()
            .expect("connect to fidl.fuchsia.testing.FakeClock");
        // Each request is forwarded, and the reply forwarded back.
        while let Some(request) = stream.next().await {
            match request {
                Ok(ffte::FakeClockRequest::Get { responder, .. }) => {
                    let result = _proxy.get().await.unwrap();
                    tracing::debug!("FakeClock/Get: result={}", result);
                    responder.send(result).unwrap();
                }
                Err(e) => {
                    tracing::error!("error while serving: {:?}", &e);
                }
                r @ _ => {
                    tracing::error!("FakeClock: unimplemented: {:?}", &r);
                }
            }
        }
        // If you see this message earlier than expected, ensure that your clients
        // don't close the connections prematurely.
        tracing::debug!("serve_fake_clock exiting.");
    }
    .boxed()
}

/// Returns a function that blocks and serves a fake clock control request stream.
fn serve_fake_clock_control(
    mut stream: ffte::FakeClockControlRequestStream,
) -> BoxFuture<'static, ()> {
    async move {
        tracing::debug!("serve_fake_clock_control entry.");
        let _proxy = client::connect_to_protocol::<ffte::FakeClockControlMarker>()
            .expect("connect to fidl.fuchsia.testing.FakeClockControl");
        while let Some(request) = stream.next().await {
            match request {
                Ok(_) => {
                    todo!();
                }
                Err(e) => {
                    tracing::error!("error while serving: {:?}", &e);
                }
            }
        }
        // If you see this message earlier than expected, ensure that your clients
        // don't close the connections prematurely.
        tracing::debug!("serve_fake_clock_control exiting.");
    }
    .boxed()
}

/// Returns a function that blocks and serves a standard Realm Factory.
fn serve_realm_factory(mut stream: fttr::RealmFactoryRequestStream) -> BoxFuture<'static, ()> {
    async move {
        let mut task_group = fasync::TaskGroup::new();
        let result: Result<()> = async move {
            while let Ok(Some(request)) = stream.try_next().await {
                tracing::trace!("received a request: {:?}", &request);
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
                            fake_clock_controller,
                        ) = realm_factory::create_realm(options, fake_utc_clock).await?;

                        let mut opts = fttr::CreateResponseOpts { ..Default::default() };

                        if let Some(ctl) = fake_clock_controller {
                            // Do something with the fake clock controller.
                            let (clock_control_proxy, clock_proxy) = ctl.into_components();
                            // Convert the above into client ends and add to opts.
                            let clock_control_client_end =
                                proxy_into_client_end(clock_control_proxy);
                            opts.clock_control = Some(clock_control_client_end);

                            let clock_client_end = proxy_into_client_end(clock_proxy);
                            opts.clock = Some(clock_client_end);
                            tracing::debug!("using fake clock controller");
                        }

                        let (push_source_puppet_client_end, push_source_request_stream) =
                            endpoints::create_request_stream().expect("infallible");
                        task_group.spawn(async move {
                            serve_push_puppet(push_source_puppet, push_source_request_stream)
                                .await
                                .expect("serve_push_puppet should not return an error");
                        });

                        // Needs to be served by the test fixture.
                        let (rtc_updates, rtc_updates_stream) =
                            endpoints::create_request_stream().expect("infallible");
                        task_group.spawn(async move {
                            serve_rtc_updates(_rtc_updates, rtc_updates_stream)
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
                            proxy_into_client_end(cobalt_proxy);

                        opts.rtc_updates = rtc_updates.into();
                        responder.send(Ok((
                            push_source_puppet_client_end,
                            opts,
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
    .boxed()
}
