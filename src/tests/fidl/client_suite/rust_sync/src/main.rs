// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Error},
    fidl_fidl_clientsuite::{
        AjarTargetEvent, AjarTargetEventReport, AjarTargetEventReporterSynchronousProxy,
        AjarTargetSynchronousProxy, ClosedTargetEvent, ClosedTargetEventReport,
        ClosedTargetEventReporterSynchronousProxy, ClosedTargetSynchronousProxy, Empty,
        EmptyResultClassification, EmptyResultWithErrorClassification, NonEmptyPayload,
        NonEmptyResultClassification, NonEmptyResultWithErrorClassification, OpenTargetEvent,
        OpenTargetEventReport, OpenTargetEventReporterSynchronousProxy, OpenTargetSynchronousProxy,
        RunnerRequest, RunnerRequestStream, TableResultClassification, Test,
        UnionResultClassification, UnknownEvent,
    },
    fidl_zx as _,
    fuchsia_component::server::ServiceFs,
    fuchsia_zircon as zx,
    fuchsia_zircon::AsHandleRef,
    futures::prelude::*,
    rust_util::classify_error,
};

async fn run_runner_server(stream: RunnerRequestStream) -> Result<(), Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                // Test management methods
                RunnerRequest::IsTestEnabled { test, responder } => {
                    let enabled = match test {
                        // TODO(fxbug.dev/99738): Rust bindings should reject V1
                        // wire format.
                        Test::V1TwoWayNoPayload | Test::V1TwoWayStructPayload => false,
                        // TODO(fxbug.dev/116294): Rust bindings should reject
                        // responses with the wrong ordinal.
                        Test::TwoWayWrongResponseOrdinal => false,
                        _ => true,
                    };
                    responder.send(enabled).context("sending response failed")
                }
                RunnerRequest::CheckAlive { responder } => {
                    responder.send().context("sending response failed")
                }
                // Closed target methods
                RunnerRequest::CallTwoWayNoPayload { target, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.two_way_no_payload(zx::Time::INFINITE) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallTwoWayStructPayload { target, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.two_way_struct_payload(zx::Time::INFINITE) {
                        Ok(some_field) => responder
                            .send(&mut NonEmptyResultClassification::Success(NonEmptyPayload {
                                some_field,
                            }))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut NonEmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallTwoWayTablePayload { target, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.two_way_table_payload(zx::Time::INFINITE) {
                        Ok(payload) => responder
                            .send(&mut TableResultClassification::Success(payload))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut TableResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallTwoWayUnionPayload { target, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.two_way_union_payload(zx::Time::INFINITE) {
                        Ok(payload) => responder
                            .send(&mut UnionResultClassification::Success(payload))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut UnionResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallTwoWayStructPayloadErr { target, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.two_way_struct_payload_err(zx::Time::INFINITE) {
                        Ok(Ok(some_field)) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::Success(
                                NonEmptyPayload { some_field },
                            ))
                            .context("sending response failed"),
                        Ok(Err(application_err)) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::ApplicationError(
                                application_err,
                            ))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::FidlError(
                                classify_error(err),
                            ))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallTwoWayStructRequest { target, request, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.two_way_struct_request(request.some_field, zx::Time::INFINITE) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallTwoWayTableRequest { target, request, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.two_way_table_request(&request, zx::Time::INFINITE) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallTwoWayUnionRequest { target, mut request, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.two_way_union_request(&mut request, zx::Time::INFINITE) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallOneWayNoRequest { target, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.one_way_no_request() {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallOneWayStructRequest { target, request, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.one_way_struct_request(request.some_field) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallOneWayTableRequest { target, request, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.one_way_table_request(&request) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallOneWayUnionRequest { target, mut request, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    match client.one_way_union_request(&mut request) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                // Open target methods
                RunnerRequest::CallStrictOneWay { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.strict_one_way() {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallFlexibleOneWay { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.flexible_one_way() {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallStrictTwoWay { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.strict_two_way(zx::Time::INFINITE) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallStrictTwoWayFields { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.strict_two_way_fields(zx::Time::INFINITE) {
                        Ok(some_field) => responder
                            .send(&mut NonEmptyResultClassification::Success(NonEmptyPayload {
                                some_field,
                            }))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut NonEmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallStrictTwoWayErr { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.strict_two_way_err(zx::Time::INFINITE) {
                        Ok(Ok(())) => responder
                            .send(&mut EmptyResultWithErrorClassification::Success(Empty))
                            .context("sending response failed"),
                        Ok(Err(application_err)) => responder
                            .send(&mut EmptyResultWithErrorClassification::ApplicationError(
                                application_err,
                            ))
                            .context("sending response failed"),
                        Err(fidl_err) => responder
                            .send(&mut EmptyResultWithErrorClassification::FidlError(
                                classify_error(fidl_err),
                            ))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallStrictTwoWayFieldsErr { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.strict_two_way_fields_err(zx::Time::INFINITE) {
                        Ok(Ok(some_field)) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::Success(
                                NonEmptyPayload { some_field },
                            ))
                            .context("sending response failed"),
                        Ok(Err(application_err)) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::ApplicationError(
                                application_err,
                            ))
                            .context("sending response failed"),
                        Err(fidl_err) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::FidlError(
                                classify_error(fidl_err),
                            ))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallFlexibleTwoWay { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.flexible_two_way(zx::Time::INFINITE) {
                        Ok(()) => responder
                            .send(&mut EmptyResultClassification::Success(Empty))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut EmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallFlexibleTwoWayFields { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.flexible_two_way_fields(zx::Time::INFINITE) {
                        Ok(some_field) => responder
                            .send(&mut NonEmptyResultClassification::Success(NonEmptyPayload {
                                some_field,
                            }))
                            .context("sending response failed"),
                        Err(err) => responder
                            .send(&mut NonEmptyResultClassification::FidlError(classify_error(err)))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallFlexibleTwoWayErr { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.flexible_two_way_err(zx::Time::INFINITE) {
                        Ok(Ok(())) => responder
                            .send(&mut EmptyResultWithErrorClassification::Success(Empty))
                            .context("sending response failed"),
                        Ok(Err(application_err)) => responder
                            .send(&mut EmptyResultWithErrorClassification::ApplicationError(
                                application_err,
                            ))
                            .context("sending response failed"),
                        Err(fidl_err) => responder
                            .send(&mut EmptyResultWithErrorClassification::FidlError(
                                classify_error(fidl_err),
                            ))
                            .context("sending response failed"),
                    }
                }
                RunnerRequest::CallFlexibleTwoWayFieldsErr { target, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    match client.flexible_two_way_fields_err(zx::Time::INFINITE) {
                        Ok(Ok(some_field)) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::Success(
                                NonEmptyPayload { some_field },
                            ))
                            .context("sending response failed"),
                        Ok(Err(application_err)) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::ApplicationError(
                                application_err,
                            ))
                            .context("sending response failed"),
                        Err(fidl_err) => responder
                            .send(&mut NonEmptyResultWithErrorClassification::FidlError(
                                classify_error(fidl_err),
                            ))
                            .context("sending response failed"),
                    }
                }
                // Event handling methods.
                RunnerRequest::ReceiveClosedEvents { target, reporter, responder } => {
                    let client = ClosedTargetSynchronousProxy::new(target.into_channel());
                    let reporter =
                        ClosedTargetEventReporterSynchronousProxy::new(reporter.into_channel());
                    std::thread::spawn(move || {
                        loop {
                            let report_result = match client.wait_for_event(zx::Time::INFINITE) {
                                Ok(ClosedTargetEvent::OnEventNoPayload {}) => reporter
                                    .report_event(&mut ClosedTargetEventReport::OnEventNoPayload(
                                        Empty {},
                                    )),
                                Ok(ClosedTargetEvent::OnEventStructPayload { some_field }) => {
                                    reporter.report_event(
                                        &mut ClosedTargetEventReport::OnEventStructPayload(
                                            NonEmptyPayload { some_field },
                                        ),
                                    )
                                }
                                Ok(ClosedTargetEvent::OnEventTablePayload { payload }) => reporter
                                    .report_event(
                                        &mut ClosedTargetEventReport::OnEventTablePayload(payload),
                                    ),
                                Ok(ClosedTargetEvent::OnEventUnionPayload { payload }) => reporter
                                    .report_event(
                                        &mut ClosedTargetEventReport::OnEventUnionPayload(payload),
                                    ),
                                Err(fidl_err @ fidl::Error::ClientEvent(_))
                                | Err(fidl_err @ fidl::Error::ClientChannelClosed { .. }) => {
                                    // Error receiving event or peer closed.
                                    // Just wait for reporter to close, don't
                                    // keep reading because any more reads will
                                    // likely also just error.
                                    let report_result = reporter.report_event(
                                        &mut ClosedTargetEventReport::FidlError(classify_error(
                                            fidl_err,
                                        )),
                                    );
                                    match report_result {
                                        // Reporter disconnected. We're done.
                                        Err(fidl::Error::ClientChannelClosed { .. }) => return,
                                        // Report succeeded. Wait for reporter
                                        // to close.
                                        Ok(()) => {
                                            reporter
                                                .into_channel()
                                                .wait_handle(
                                                    zx::Signals::CHANNEL_PEER_CLOSED,
                                                    zx::Time::INFINITE,
                                                )
                                                .expect("waiting for reporter to close failed");
                                            return;
                                        }
                                        Err(report_err) => {
                                            panic!("sending event report failed {}", report_err)
                                        }
                                    }
                                }
                                Err(fidl_err) => {
                                    reporter.report_event(&mut ClosedTargetEventReport::FidlError(
                                        classify_error(fidl_err),
                                    ))
                                }
                            };
                            match report_result {
                                // Reporter disconnected. We're done.
                                Err(fidl::Error::ClientChannelClosed { .. }) => return,
                                // Report succeeded. Continue accepting events.
                                Ok(()) => {}
                                Err(report_err) => {
                                    panic!("sending event report failed {}", report_err)
                                }
                            }
                        }
                    });
                    responder.send().context("sending response failed")
                }

                RunnerRequest::ReceiveAjarEvents { target, reporter, responder } => {
                    let client = AjarTargetSynchronousProxy::new(target.into_channel());
                    let reporter =
                        AjarTargetEventReporterSynchronousProxy::new(reporter.into_channel());
                    std::thread::spawn(move || {
                        loop {
                            let report_result = match client.wait_for_event(zx::Time::INFINITE) {
                                Ok(AjarTargetEvent::_UnknownEvent { ordinal, .. }) => reporter
                                    .report_event(&mut AjarTargetEventReport::UnknownEvent(
                                        UnknownEvent { ordinal },
                                    )),
                                Err(fidl_err @ fidl::Error::ClientEvent(_))
                                | Err(fidl_err @ fidl::Error::ClientChannelClosed { .. }) => {
                                    // Error receiving event or peer closed.
                                    // Just wait for reporter to close, don't
                                    // keep reading because any more reads will
                                    // likely also just error.
                                    let report_result = reporter.report_event(
                                        &mut AjarTargetEventReport::FidlError(classify_error(
                                            fidl_err,
                                        )),
                                    );
                                    match report_result {
                                        // Reporter disconnected. We're done.
                                        Err(fidl::Error::ClientChannelClosed { .. }) => return,
                                        // Report succeeded. Wait for reporter
                                        // to close.
                                        Ok(()) => {
                                            reporter
                                                .into_channel()
                                                .wait_handle(
                                                    zx::Signals::CHANNEL_PEER_CLOSED,
                                                    zx::Time::INFINITE,
                                                )
                                                .expect("waiting for reporter to close failed");
                                            return;
                                        }
                                        Err(report_err) => {
                                            panic!("sending event report failed {}", report_err)
                                        }
                                    }
                                }
                                Err(fidl_err) => reporter.report_event(
                                    &mut AjarTargetEventReport::FidlError(classify_error(fidl_err)),
                                ),
                            };
                            match report_result {
                                // Reporter disconnected. We're done.
                                Err(fidl::Error::ClientChannelClosed { .. }) => return,
                                // Report succeeded. Continue accepting events.
                                Ok(()) => {}
                                Err(report_err) => {
                                    panic!("sending event report failed {}", report_err)
                                }
                            }
                        }
                    });
                    responder.send().context("sending response failed")
                }
                RunnerRequest::ReceiveOpenEvents { target, reporter, responder } => {
                    let client = OpenTargetSynchronousProxy::new(target.into_channel());
                    let reporter =
                        OpenTargetEventReporterSynchronousProxy::new(reporter.into_channel());
                    std::thread::spawn(move || {
                        loop {
                            let report_result = match client.wait_for_event(zx::Time::INFINITE) {
                                Ok(OpenTargetEvent::StrictEvent {}) => reporter
                                    .report_event(&mut OpenTargetEventReport::StrictEvent(Empty)),
                                Ok(OpenTargetEvent::FlexibleEvent {}) => reporter
                                    .report_event(&mut OpenTargetEventReport::FlexibleEvent(Empty)),
                                Ok(OpenTargetEvent::_UnknownEvent { ordinal, .. }) => reporter
                                    .report_event(&mut OpenTargetEventReport::UnknownEvent(
                                        UnknownEvent { ordinal },
                                    )),
                                Err(fidl_err @ fidl::Error::ClientEvent(_))
                                | Err(fidl_err @ fidl::Error::ClientChannelClosed { .. }) => {
                                    // Error receiving event or peer closed.
                                    // Just wait for reporter to close, don't
                                    // keep reading because any more reads will
                                    // likely also just error.
                                    let report_result = reporter.report_event(
                                        &mut OpenTargetEventReport::FidlError(classify_error(
                                            fidl_err,
                                        )),
                                    );
                                    match report_result {
                                        // Reporter disconnected. We're done.
                                        Err(fidl::Error::ClientChannelClosed { .. }) => return,
                                        // Report succeeded. Wait for reporter
                                        // to close.
                                        Ok(()) => {
                                            reporter
                                                .into_channel()
                                                .wait_handle(
                                                    zx::Signals::CHANNEL_PEER_CLOSED,
                                                    zx::Time::INFINITE,
                                                )
                                                .expect("waiting for reporter to close failed");
                                            return;
                                        }
                                        Err(report_err) => {
                                            panic!("sending event report failed {}", report_err)
                                        }
                                    }
                                }
                                Err(fidl_err) => reporter.report_event(
                                    &mut OpenTargetEventReport::FidlError(classify_error(fidl_err)),
                                ),
                            };
                            match report_result {
                                // Reporter disconnected. We're done.
                                Err(fidl::Error::ClientChannelClosed { .. }) => return,
                                // Report succeeded. Continue accepting events.
                                Ok(()) => {}
                                Err(report_err) => {
                                    panic!("sending event report failed {}", report_err)
                                }
                            }
                        }
                    });
                    responder.send().context("sending response failed")
                }
            }
        })
        .await
}

enum IncomingService {
    Runner(RunnerRequestStream),
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingService::Runner);
    fs.take_and_serve_directory_handle().expect("serving directory failed");

    println!("Listening for incoming connections...");
    const MAX_CONCURRENT: usize = 10_000;
    fs.for_each_concurrent(MAX_CONCURRENT, |IncomingService::Runner(stream)| {
        run_runner_server(stream).unwrap_or_else(|e| panic!("runner server failed {:?}", e))
    })
    .await;

    Ok(())
}
