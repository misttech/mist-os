// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::{
        endpoints::{ControlHandle, UnknownMethodType},
        AsHandleRef, Event, Status,
    },
    fidl_fidl_serversuite::{
        AjarTargetControlHandle, AjarTargetControllerControlHandle,
        AjarTargetControllerRequestStream, AjarTargetRequest, AjarTargetRequestStream,
        AjarTargetServerPair, AnyTarget, ClosedTargetControlHandle,
        ClosedTargetControllerControlHandle, ClosedTargetControllerRequest,
        ClosedTargetControllerRequestStream, ClosedTargetRequest, ClosedTargetRequestStream,
        ClosedTargetServerPair, ClosedTargetTwoWayResultRequest,
        ClosedTargetTwoWayTablePayloadResponse, ClosedTargetTwoWayUnionPayloadRequest,
        ClosedTargetTwoWayUnionPayloadResponse, Empty, OpenTargetControlHandle,
        OpenTargetControllerControlHandle, OpenTargetControllerRequest,
        OpenTargetControllerRequestStream, OpenTargetFlexibleTwoWayErrRequest,
        OpenTargetFlexibleTwoWayFieldsErrRequest, OpenTargetRequest, OpenTargetRequestStream,
        OpenTargetServerPair, OpenTargetStrictTwoWayErrRequest,
        OpenTargetStrictTwoWayFieldsErrRequest, RunnerRequest, RunnerRequestStream, SendEventError,
        Test, UnknownMethodType as DynsuiteUnknownMethodType,
    },
    fuchsia_component::server::ServiceFs,
    futures::prelude::*,
};

const DISABLED_TESTS: &[Test] = &[
    // This is always disabled so that we can make sure IsTestEnabled() works.
    Test::IgnoreDisabled,
    // TODO(fxbug.dev/120742): Should validate txid.
    Test::OneWayWithNonZeroTxid,
    Test::TwoWayNoPayloadWithZeroTxid,
    // TODO(fxbug.dev/99738): Should reject V1 wire format.
    Test::V1TwoWayNoPayload,
    Test::V1TwoWayStructPayload,
    // TODO(fxbug.dev/74241): There are many tests where the harness sends
    // something invalid and expects a PEER_CLOSED. Right now the Rust bindings
    // don't actually close the channel until everything is dropped, so to make
    // things get dropped and avoid tests hanging, we shut down the controller
    // after sending WillTeardown. That works for most tests, but breaks
    // EventSendingDoNotReportPeerClosed where the harness calls SendStrictEvent
    // after closing the target channel. Once we make channel closure happen
    // more promptly, we can remove the ctl_handle.shutdown() calls below and
    // re-enable this test.
    Test::EventSendingDoNotReportPeerClosed,
];

fn get_teardown_reason(error: Option<fidl::Error>) -> fidl_fidl_serversuite::TeardownReason {
    use fidl_fidl_serversuite::TeardownReason;
    let error = match error {
        // TODO(fxbug.dev/120696): This is not always correct. The stream also
        // completes without error when shutdown() is called.
        None => return TeardownReason::ChannelPeerClosed,
        Some(e) => e,
    };
    match error {
        fidl::Error::InvalidBoolean
        | fidl::Error::InvalidHeader
        | fidl::Error::IncompatibleMagicNumber(_)
        | fidl::Error::Invalid
        | fidl::Error::OutOfRange
        | fidl::Error::ExtraBytes
        | fidl::Error::ExtraHandles
        | fidl::Error::NonZeroPadding { .. }
        | fidl::Error::MaxRecursionDepth
        | fidl::Error::NotNullable
        | fidl::Error::UnexpectedNullRef
        | fidl::Error::Utf8Error
        | fidl::Error::InvalidBitsValue
        | fidl::Error::InvalidEnumValue
        | fidl::Error::UnknownUnionTag
        | fidl::Error::InvalidPresenceIndicator
        | fidl::Error::InvalidInlineBitInEnvelope
        | fidl::Error::InvalidInlineMarkerInEnvelope
        | fidl::Error::InvalidNumBytesInEnvelope
        | fidl::Error::InvalidHostHandle
        | fidl::Error::IncorrectHandleSubtype { .. }
        | fidl::Error::MissingExpectedHandleRights { .. } => TeardownReason::DecodingError,

        fidl::Error::UnknownOrdinal { .. } | fidl::Error::InvalidResponseTxid => {
            TeardownReason::UnexpectedMessage
        }

        fidl::Error::UnsupportedMethod { .. }
        | fidl::Error::ClientChannelClosed { .. }
        | fidl::Error::UnexpectedSyncResponse { .. } => {
            panic!("{error:?} should not be seen on server side")
        }

        _ => TeardownReason::Other,
    }
}

fn classify_send_event_error(_err: fidl::Error) -> SendEventError {
    SendEventError::OtherError
}

/// Returns the FIDL method name for a request/event enum value.
// TODO(fxbug.dev/127952): Provide this in FIDL bindings.
fn method_name(request_or_event: &impl std::fmt::Debug) -> String {
    let mut string = format!("{:?}", request_or_event);
    let len = string.find('{').unwrap_or(string.len());
    let len = string[..len].trim_end().len();
    string.truncate(len);
    assert!(
        string.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "failed to parse the method name from {:?}",
        request_or_event
    );
    string
}

// This is the error return type for functions that run a target server. They
// should only return an error when it is expected to occur during tests. On
// other errors (e.g. reply sending that never fails), they should panic.
type ErrorExpectedUnderTest = fidl::Error;

async fn run_closed_target_controller_server(
    mut stream: ClosedTargetControllerRequestStream,
    sut_handle: &ClosedTargetControlHandle,
) {
    println!("Running ClosedTargetController");
    while let Some(request) = stream.try_next().await.unwrap() {
        println!("Handling ClosedTargetController request: {}", method_name(&request));
        match request {
            ClosedTargetControllerRequest::CloseWithEpitaph { epitaph_status, .. } => {
                sut_handle.shutdown_with_epitaph(Status::from_raw(epitaph_status));
            }
        }
    }
}

async fn run_closed_target_server(
    mut stream: ClosedTargetRequestStream,
    ctl_handle: &ClosedTargetControllerControlHandle,
) -> Result<(), ErrorExpectedUnderTest> {
    println!("Running ClosedTarget");
    while let Some(request) = stream.try_next().await? {
        println!("Handling ClosedTarget request: {}", method_name(&request));
        match request {
            ClosedTargetRequest::OneWayNoPayload { .. } => {
                ctl_handle.send_received_one_way_no_payload().unwrap();
            }
            ClosedTargetRequest::TwoWayNoPayload { responder } => {
                responder.send().unwrap();
            }
            ClosedTargetRequest::TwoWayStructPayload { v, responder } => {
                responder.send(v).unwrap();
            }
            ClosedTargetRequest::TwoWayTablePayload { payload, responder } => {
                responder
                    .send(&ClosedTargetTwoWayTablePayloadResponse {
                        v: payload.v,
                        ..Default::default()
                    })
                    .unwrap();
            }
            ClosedTargetRequest::TwoWayUnionPayload { payload, responder } => {
                let ClosedTargetTwoWayUnionPayloadRequest::V(v) = payload else {
                    panic!("unexpected union value: {payload:?}");
                };
                responder.send(&ClosedTargetTwoWayUnionPayloadResponse::V(v)).unwrap();
            }
            ClosedTargetRequest::TwoWayResult { payload, responder } => match payload {
                ClosedTargetTwoWayResultRequest::Payload(value) => {
                    responder.send(Ok(&value)).unwrap();
                }
                ClosedTargetTwoWayResultRequest::Error(value) => {
                    responder.send(Err(value)).unwrap();
                }
            },
            ClosedTargetRequest::GetHandleRights { handle, responder } => {
                let basic_info = handle.as_handle_ref().basic_info().unwrap();
                let rights = fidl_zx::Rights::from_bits(basic_info.rights.bits()).unwrap();
                responder.send(rights).unwrap();
            }
            ClosedTargetRequest::GetSignalableEventRights { handle, responder } => {
                let basic_info = handle.as_handle_ref().basic_info().unwrap();
                let rights = fidl_zx::Rights::from_bits(basic_info.rights.bits()).unwrap();
                responder.send(rights).unwrap();
            }
            ClosedTargetRequest::EchoAsTransferableSignalableEvent { handle, responder } => {
                responder.send(fidl::Event::from(handle))?;
            }
            ClosedTargetRequest::ByteVectorSize { vec, responder } => {
                responder.send(vec.len().try_into().unwrap()).unwrap();
            }
            ClosedTargetRequest::HandleVectorSize { vec, responder } => {
                responder.send(vec.len().try_into().unwrap()).unwrap();
            }
            ClosedTargetRequest::CreateNByteVector { n, responder } => {
                let bytes: Vec<u8> = vec![0; n.try_into().unwrap()];
                responder.send(&bytes)?;
            }
            ClosedTargetRequest::CreateNHandleVector { n, responder } => {
                let mut handles = Vec::new();
                for _ in 0..n {
                    handles.push(Event::create());
                }
                responder.send(handles)?;
            }
        }
    }
    Ok(())
}

async fn run_ajar_target_controller_server(
    mut stream: AjarTargetControllerRequestStream,
    _sut_handle: &AjarTargetControlHandle,
) {
    println!("Running AjarTargetController");
    while let Some(request) = stream.try_next().await.unwrap() {
        println!("Handling AjarTargetController request: {}", method_name(&request));
        match request {}
    }
}

async fn run_ajar_target_server(
    mut stream: AjarTargetRequestStream,
    ctl_handle: &AjarTargetControllerControlHandle,
) -> Result<(), ErrorExpectedUnderTest> {
    println!("Running AjarTarget");
    while let Some(request) = stream.try_next().await? {
        println!("Handling AjarTarget request: {}", method_name(&request));
        match request {
            AjarTargetRequest::_UnknownMethod { ordinal, .. } => {
                ctl_handle
                    .send_received_unknown_method(ordinal, DynsuiteUnknownMethodType::OneWay)
                    .unwrap();
            }
        }
    }
    Ok(())
}

async fn run_open_target_controller_server(
    mut stream: OpenTargetControllerRequestStream,
    sut_handle: &OpenTargetControlHandle,
) {
    println!("Running OpenTargetController");
    while let Some(request) = stream.try_next().await.unwrap() {
        println!("Handling OpenTargetController request: {}", method_name(&request));
        match request {
            OpenTargetControllerRequest::SendStrictEvent { responder } => {
                let res = sut_handle.send_strict_event().map_err(classify_send_event_error);
                responder.send(res).unwrap();
            }
            OpenTargetControllerRequest::SendFlexibleEvent { responder } => {
                let res = sut_handle.send_flexible_event().map_err(classify_send_event_error);
                responder.send(res).unwrap();
            }
        }
    }
}

async fn run_open_target_server(
    mut stream: OpenTargetRequestStream,
    ctl_handle: &OpenTargetControllerControlHandle,
) -> Result<(), ErrorExpectedUnderTest> {
    println!("Running OpenTarget");
    while let Some(request) = stream.try_next().await? {
        println!("Handling OpenTarget request: {}", method_name(&request));
        match request {
            OpenTargetRequest::StrictOneWay { .. } => {
                ctl_handle.send_received_strict_one_way().unwrap();
            }
            OpenTargetRequest::FlexibleOneWay { .. } => {
                ctl_handle.send_received_flexible_one_way().unwrap();
            }
            OpenTargetRequest::StrictTwoWay { responder } => {
                responder.send().unwrap();
            }
            OpenTargetRequest::StrictTwoWayFields { reply_with, responder } => {
                responder.send(reply_with).unwrap();
            }
            OpenTargetRequest::StrictTwoWayErr { payload, responder } => match payload {
                OpenTargetStrictTwoWayErrRequest::ReplySuccess(Empty) => {
                    responder.send(Ok(())).unwrap();
                }
                OpenTargetStrictTwoWayErrRequest::ReplyError(reply_error) => {
                    responder.send(Err(reply_error)).unwrap();
                }
            },
            OpenTargetRequest::StrictTwoWayFieldsErr { payload, responder } => match payload {
                OpenTargetStrictTwoWayFieldsErrRequest::ReplySuccess(reply_success) => {
                    responder.send(Ok(reply_success)).unwrap();
                }
                OpenTargetStrictTwoWayFieldsErrRequest::ReplyError(reply_error) => {
                    responder.send(Err(reply_error)).unwrap();
                }
            },
            OpenTargetRequest::FlexibleTwoWay { responder } => {
                responder.send().unwrap();
            }
            OpenTargetRequest::FlexibleTwoWayFields { reply_with, responder } => {
                responder.send(reply_with).unwrap();
            }
            OpenTargetRequest::FlexibleTwoWayErr { payload, responder } => match payload {
                OpenTargetFlexibleTwoWayErrRequest::ReplySuccess(Empty) => {
                    responder.send(Ok(())).unwrap();
                }
                OpenTargetFlexibleTwoWayErrRequest::ReplyError(reply_error) => {
                    responder.send(Err(reply_error)).unwrap();
                }
            },
            OpenTargetRequest::FlexibleTwoWayFieldsErr { payload, responder } => match payload {
                OpenTargetFlexibleTwoWayFieldsErrRequest::ReplySuccess(reply_success) => {
                    responder.send(Ok(reply_success)).unwrap();
                }
                OpenTargetFlexibleTwoWayFieldsErrRequest::ReplyError(reply_error) => {
                    responder.send(Err(reply_error)).unwrap();
                }
            },
            OpenTargetRequest::_UnknownMethod { ordinal, unknown_method_type, .. } => {
                let unknown_method_type = match unknown_method_type {
                    UnknownMethodType::OneWay => DynsuiteUnknownMethodType::OneWay,
                    UnknownMethodType::TwoWay => DynsuiteUnknownMethodType::TwoWay,
                };
                ctl_handle.send_received_unknown_method(ordinal, unknown_method_type).unwrap();
            }
        }
    }
    Ok(())
}

async fn handle_runner_request(request: RunnerRequest) {
    match request {
        RunnerRequest::CheckAlive { responder } => {
            responder.send().unwrap();
        }
        RunnerRequest::IsTestEnabled { test, responder } => {
            responder.send(!DISABLED_TESTS.contains(&test)).unwrap()
        }
        RunnerRequest::IsTeardownReasonSupported { responder } => {
            responder.send(true).unwrap();
        }
        RunnerRequest::Start { target, responder } => match target {
            AnyTarget::ClosedTarget(ClosedTargetServerPair { controller: ctl, sut }) => {
                let (ctl_stream, ctl_handle) = ctl.into_stream_and_control_handle().unwrap();
                let (sut_stream, sut_handle) = sut.into_stream_and_control_handle().unwrap();
                let ctl_fut = run_closed_target_controller_server(ctl_stream, &sut_handle);
                let sut_fut = run_closed_target_server(sut_stream, &ctl_handle).map(|result| {
                    if let Err(ref err) = result {
                        println!("ClosedTarget failed: {err:?}");
                    }
                    ctl_handle.send_will_teardown(get_teardown_reason(result.err())).unwrap();
                    ctl_handle.shutdown();
                });
                responder.send().unwrap();
                future::join(ctl_fut, sut_fut).await;
            }
            AnyTarget::AjarTarget(AjarTargetServerPair { controller: ctl, sut }) => {
                let (ctl_stream, ctl_handle) = ctl.into_stream_and_control_handle().unwrap();
                let (sut_stream, sut_handle) = sut.into_stream_and_control_handle().unwrap();
                let ctl_fut = run_ajar_target_controller_server(ctl_stream, &sut_handle);
                let sut_fut = run_ajar_target_server(sut_stream, &ctl_handle).map(|result| {
                    if let Err(ref err) = result {
                        println!("AjarTarget failed: {err:?}");
                    }
                    ctl_handle.send_will_teardown(get_teardown_reason(result.err())).unwrap();
                    ctl_handle.shutdown();
                });
                responder.send().unwrap();
                future::join(ctl_fut, sut_fut).await;
            }
            AnyTarget::OpenTarget(OpenTargetServerPair { controller: ctl, sut }) => {
                let (ctl_stream, ctl_handle) = ctl.into_stream_and_control_handle().unwrap();
                let (sut_stream, sut_handle) = sut.into_stream_and_control_handle().unwrap();
                let ctl_fut = run_open_target_controller_server(ctl_stream, &sut_handle);
                let sut_fut = run_open_target_server(sut_stream, &ctl_handle).map(|result| {
                    if let Err(ref err) = result {
                        println!("OpenTarget failed: {err:?}");
                    }
                    ctl_handle.send_will_teardown(get_teardown_reason(result.err())).unwrap();
                    ctl_handle.shutdown();
                });
                responder.send().unwrap();
                future::join(ctl_fut, sut_fut).await;
            }
        },
    }
}

enum IncomingService {
    Runner(RunnerRequestStream),
}

#[fuchsia::main]
async fn main() {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingService::Runner);
    fs.take_and_serve_directory_handle().unwrap();

    println!("Listening for incoming connections...");
    const MAX_CONCURRENT: usize = 10_000;
    fs.for_each_concurrent(MAX_CONCURRENT, |IncomingService::Runner(mut stream)| async move {
        println!("Received connection, serving requests...");
        while let Some(request) = stream.try_next().await.unwrap() {
            println!("Handling Runner request: {}", method_name(&request));
            handle_runner_request(request).await;
        }
    })
    .await;
}
