// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Error},
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
        ClosedTargetTwoWayUnionPayloadResponse, Empty, EncodingFailureKind,
        LargeMessageTargetControlHandle, LargeMessageTargetControllerControlHandle,
        LargeMessageTargetControllerRequestStream, LargeMessageTargetOneWayMethod,
        LargeMessageTargetRequest, LargeMessageTargetRequestStream, LargeMessageTargetServerPair,
        OpenTargetControlHandle, OpenTargetControllerControlHandle, OpenTargetControllerRequest,
        OpenTargetControllerRequestStream, OpenTargetFlexibleTwoWayErrRequest,
        OpenTargetFlexibleTwoWayFieldsErrRequest, OpenTargetRequest, OpenTargetRequestStream,
        OpenTargetServerPair, OpenTargetStrictTwoWayErrRequest,
        OpenTargetStrictTwoWayFieldsErrRequest, RunnerRequest, RunnerRequestStream, SendEventError,
        Test, UnknownMethodType as DynsuiteUnknownMethodType,
    },
    fuchsia_component::server::ServiceFs,
    futures::prelude::*,
};

const EXPECT_ONE_WAY: &'static str = "failed to report one way request";
const EXPECT_REPLY_FAILED: &'static str = "failed to send reply";

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
        | fidl::Error::LargeMessageMissingHandles
        | fidl::Error::LargeMessageCouldNotReadVmo { .. }
        | fidl::Error::LargeMessageInfoMissized { .. }
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
        | fidl::Error::MissingExpectedHandleRights { .. }
        | fidl::Error::CannotStoreUnknownHandles => TeardownReason::DecodingError,

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

fn request_name(request: &impl std::fmt::Debug) -> String {
    let mut str = format!("{request:?}");
    str.truncate(str.find('{').expect("debug representation must contain '{'"));
    str
}

async fn run_closed_target_controller_server(
    mut stream: ClosedTargetControllerRequestStream,
    sut_handle: &ClosedTargetControlHandle,
) -> Result<(), fidl::Error> {
    println!("Running ClosedTargetController");
    while let Some(request) = stream.try_next().await? {
        println!("ClosedTargetController got request: {}", request_name(&request));
        match request {
            ClosedTargetControllerRequest::CloseWithEpitaph {
                epitaph_status,
                control_handle: _,
            } => {
                sut_handle.shutdown_with_epitaph(Status::from_raw(epitaph_status));
            }
        }
    }
    Ok(())
}

async fn run_closed_target_server(
    mut stream: ClosedTargetRequestStream,
    ctl_handle: &ClosedTargetControllerControlHandle,
) -> Result<(), fidl::Error> {
    println!("Running ClosedTarget");
    while let Some(request) = stream.try_next().await? {
        println!("ClosedTarget got request: {}", request_name(&request));
        match request {
            ClosedTargetRequest::OneWayNoPayload { control_handle: _ } => {
                ctl_handle
                    .send_received_one_way_no_payload()
                    .expect("sending ReceivedOneWayNoPayload failed");
            }
            ClosedTargetRequest::TwoWayNoPayload { responder } => {
                responder.send().expect("failed to send two way payload response");
            }
            ClosedTargetRequest::TwoWayStructPayload { v, responder } => {
                responder.send(v).expect("failed to send two way payload response");
            }
            ClosedTargetRequest::TwoWayTablePayload { payload, responder } => {
                responder
                    .send(&ClosedTargetTwoWayTablePayloadResponse {
                        v: payload.v,
                        ..Default::default()
                    })
                    .expect("failed to send two way payload response");
            }
            ClosedTargetRequest::TwoWayUnionPayload { payload, responder } => {
                let v = match payload {
                    ClosedTargetTwoWayUnionPayloadRequest::V(v) => v,
                    _ => {
                        panic!("unexpected union value");
                    }
                };
                responder
                    .send(&ClosedTargetTwoWayUnionPayloadResponse::V(v))
                    .expect("failed to send two way payload response");
            }
            ClosedTargetRequest::TwoWayResult { payload, responder } => {
                match payload {
                    ClosedTargetTwoWayResultRequest::Payload(value) => {
                        responder.send(&mut Ok(value))
                    }
                    ClosedTargetTwoWayResultRequest::Error(value) => {
                        responder.send(&mut Err(value))
                    }
                }
                .expect("failed to send two way payload response");
            }
            ClosedTargetRequest::GetHandleRights { handle, responder } => {
                let basic_info =
                    handle.as_handle_ref().basic_info().expect("failed to get basic handle info");
                let rights = fidl_zx::Rights::from_bits(basic_info.rights.bits())
                    .expect("bits should be valid");
                responder.send(rights).expect("failed to send response");
            }
            ClosedTargetRequest::GetSignalableEventRights { handle, responder } => {
                let basic_info =
                    handle.as_handle_ref().basic_info().expect("failed to get basic handle info");
                let rights = fidl_zx::Rights::from_bits(basic_info.rights.bits())
                    .expect("bits should be valid");
                responder.send(rights).expect("failed to send response");
            }
            ClosedTargetRequest::EchoAsTransferableSignalableEvent { handle, responder } => {
                responder.send(fidl::Event::from(handle))?;
            }
            ClosedTargetRequest::ByteVectorSize { vec, responder } => {
                responder.send(vec.len().try_into().unwrap()).expect("failed to send response");
            }
            ClosedTargetRequest::HandleVectorSize { vec, responder } => {
                responder.send(vec.len().try_into().unwrap()).expect("failed to send response");
            }
            ClosedTargetRequest::CreateNByteVector { n, responder } => {
                let bytes: Vec<u8> = vec![0; n.try_into().unwrap()];
                responder.send(&bytes)?;
            }
            ClosedTargetRequest::CreateNHandleVector { n, responder } => {
                let mut handles: Vec<fidl::Event> = Vec::new();
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
) -> Result<(), fidl::Error> {
    println!("Running AjarTargetController");
    while let Some(request) = stream.try_next().await? {
        println!("AjarTargetController got request: {}", request_name(&request));
        match request {}
    }
    Ok(())
}

async fn run_ajar_target_server(
    mut stream: AjarTargetRequestStream,
    ctl_handle: &AjarTargetControllerControlHandle,
) -> Result<(), fidl::Error> {
    println!("Running AjarTarget");
    while let Some(request) = stream.try_next().await? {
        println!("AjarTarget got request: {}", request_name(&request));
        match request {
            AjarTargetRequest::_UnknownMethod { ordinal, control_handle: _, .. } => {
                ctl_handle
                    .send_received_unknown_method(ordinal, DynsuiteUnknownMethodType::OneWay)
                    .expect("failed to report unknown method call");
            }
        }
    }
    Ok(())
}

async fn run_open_target_controller_server(
    mut stream: OpenTargetControllerRequestStream,
    sut_handle: &OpenTargetControlHandle,
) -> Result<(), fidl::Error> {
    println!("Running OpenTargetController");
    while let Some(request) = stream.try_next().await? {
        println!("OpenTargetController got request: {}", request_name(&request));
        match request {
            OpenTargetControllerRequest::SendStrictEvent { responder } => {
                let res = sut_handle.send_strict_event().map_err(classify_send_event_error);
                responder.send(res).expect(EXPECT_REPLY_FAILED);
            }
            OpenTargetControllerRequest::SendFlexibleEvent { responder } => {
                let res = sut_handle.send_flexible_event().map_err(classify_send_event_error);
                responder.send(res).expect(EXPECT_REPLY_FAILED);
            }
        }
    }
    Ok(())
}

async fn run_open_target_server(
    mut stream: OpenTargetRequestStream,
    ctl_handle: &OpenTargetControllerControlHandle,
) -> Result<(), fidl::Error> {
    println!("Running OpenTarget");
    while let Some(request) = stream.try_next().await? {
        println!("OpenTarget got request: {}", request_name(&request));
        match request {
            OpenTargetRequest::StrictOneWay { .. } => {
                ctl_handle.send_received_strict_one_way().expect(EXPECT_ONE_WAY);
            }
            OpenTargetRequest::FlexibleOneWay { .. } => {
                ctl_handle.send_received_flexible_one_way().expect(EXPECT_ONE_WAY);
            }
            OpenTargetRequest::StrictTwoWay { responder } => {
                responder.send().expect(EXPECT_REPLY_FAILED);
            }
            OpenTargetRequest::StrictTwoWayFields { reply_with, responder } => {
                responder.send(reply_with).expect(EXPECT_REPLY_FAILED);
            }
            OpenTargetRequest::StrictTwoWayErr { payload, responder } => match payload {
                OpenTargetStrictTwoWayErrRequest::ReplySuccess(Empty) => {
                    responder.send(Ok(())).expect(EXPECT_REPLY_FAILED);
                }
                OpenTargetStrictTwoWayErrRequest::ReplyError(reply_error) => {
                    responder.send(Err(reply_error)).expect(EXPECT_REPLY_FAILED);
                }
            },
            OpenTargetRequest::StrictTwoWayFieldsErr { payload, responder } => match payload {
                OpenTargetStrictTwoWayFieldsErrRequest::ReplySuccess(reply_success) => {
                    responder.send(&mut Ok(reply_success)).expect(EXPECT_REPLY_FAILED);
                }
                OpenTargetStrictTwoWayFieldsErrRequest::ReplyError(reply_error) => {
                    responder.send(&mut Err(reply_error)).expect(EXPECT_REPLY_FAILED);
                }
            },
            OpenTargetRequest::FlexibleTwoWay { responder } => {
                responder.send().expect(EXPECT_REPLY_FAILED);
            }
            OpenTargetRequest::FlexibleTwoWayFields { reply_with, responder } => {
                responder.send(reply_with).expect(EXPECT_REPLY_FAILED);
            }
            OpenTargetRequest::FlexibleTwoWayErr { payload, responder } => match payload {
                OpenTargetFlexibleTwoWayErrRequest::ReplySuccess(Empty) => {
                    responder.send(Ok(())).expect(EXPECT_REPLY_FAILED);
                }
                OpenTargetFlexibleTwoWayErrRequest::ReplyError(reply_error) => {
                    responder.send(Err(reply_error)).expect(EXPECT_REPLY_FAILED);
                }
            },
            OpenTargetRequest::FlexibleTwoWayFieldsErr { payload, responder } => match payload {
                OpenTargetFlexibleTwoWayFieldsErrRequest::ReplySuccess(reply_success) => {
                    responder.send(&mut Ok(reply_success)).expect(EXPECT_REPLY_FAILED);
                }
                OpenTargetFlexibleTwoWayFieldsErrRequest::ReplyError(reply_error) => {
                    responder.send(&mut Err(reply_error)).expect(EXPECT_REPLY_FAILED);
                }
            },
            OpenTargetRequest::_UnknownMethod {
                ordinal,
                unknown_method_type,
                control_handle: _,
                ..
            } => {
                let unknown_method_type = match unknown_method_type {
                    UnknownMethodType::OneWay => DynsuiteUnknownMethodType::OneWay,
                    UnknownMethodType::TwoWay => DynsuiteUnknownMethodType::TwoWay,
                };
                ctl_handle
                    .send_received_unknown_method(ordinal, unknown_method_type)
                    .expect("failed to report unknown method call");
            }
        }
    }
    Ok(())
}

async fn run_large_message_target_controller_server(
    mut stream: LargeMessageTargetControllerRequestStream,
    _sut_handle: &LargeMessageTargetControlHandle,
) -> Result<(), fidl::Error> {
    println!("Running LargeMessageTargetController");
    while let Some(request) = stream.try_next().await? {
        println!("LargeMessageTargetController got request: {}", request_name(&request));
        match request {}
    }
    Ok(())
}

async fn run_large_message_target_server(
    mut stream: LargeMessageTargetRequestStream,
    ctl_handle: &LargeMessageTargetControllerControlHandle,
) -> Result<(), fidl::Error> {
    println!("Running LargeMessageTarget");
    while let Some(request) = stream.try_next().await? {
        println!("LargeMessageTarget got request: {}", request_name(&request));
        match request {
            LargeMessageTargetRequest::DecodeBoundedKnownToBeSmall {
                bytes: _,
                control_handle: _,
            } => {
                ctl_handle
                    .send_received_one_way(
                        LargeMessageTargetOneWayMethod::DecodeBoundedKnownToBeSmall,
                    )
                    .expect(EXPECT_ONE_WAY);
            }
            LargeMessageTargetRequest::DecodeBoundedMaybeLarge { bytes: _, control_handle: _ } => {
                ctl_handle
                    .send_received_one_way(LargeMessageTargetOneWayMethod::DecodeBoundedMaybeLarge)
                    .expect(EXPECT_ONE_WAY);
            }
            LargeMessageTargetRequest::DecodeSemiBoundedBelievedToBeSmall {
                payload: _,
                control_handle: _,
            } => {
                ctl_handle
                    .send_received_one_way(
                        LargeMessageTargetOneWayMethod::DecodeSemiBoundedBelievedToBeSmall,
                    )
                    .expect(EXPECT_ONE_WAY);
            }
            LargeMessageTargetRequest::DecodeSemiBoundedMaybeLarge {
                payload: _,
                control_handle: _,
            } => {
                ctl_handle
                    .send_received_one_way(
                        LargeMessageTargetOneWayMethod::DecodeSemiBoundedMaybeLarge,
                    )
                    .expect(EXPECT_ONE_WAY);
            }
            LargeMessageTargetRequest::DecodeUnboundedMaybeLargeValue {
                bytes: _,
                control_handle: _,
            } => {
                ctl_handle
                    .send_received_one_way(
                        LargeMessageTargetOneWayMethod::DecodeUnboundedMaybeLargeValue,
                    )
                    .expect(EXPECT_ONE_WAY);
            }
            LargeMessageTargetRequest::DecodeUnboundedMaybeLargeResource {
                elements: _,
                control_handle: _,
            } => {
                ctl_handle
                    .send_received_one_way(
                        LargeMessageTargetOneWayMethod::DecodeUnboundedMaybeLargeResource,
                    )
                    .expect(EXPECT_ONE_WAY);
            }
            LargeMessageTargetRequest::EncodeBoundedKnownToBeSmall { bytes, responder } => {
                responder.send(&bytes).expect(EXPECT_REPLY_FAILED);
            }
            LargeMessageTargetRequest::EncodeBoundedMaybeLarge { bytes, responder } => {
                responder.send(&bytes).expect(EXPECT_REPLY_FAILED);
            }
            LargeMessageTargetRequest::EncodeSemiBoundedBelievedToBeSmall {
                payload,
                responder,
            } => {
                responder.send(&payload).expect(EXPECT_REPLY_FAILED);
            }
            LargeMessageTargetRequest::EncodeSemiBoundedMaybeLarge { payload, responder } => {
                responder.send(&payload).expect(EXPECT_REPLY_FAILED);
            }
            LargeMessageTargetRequest::EncodeUnboundedMaybeLargeValue { bytes, responder } => {
                responder.send(&bytes).expect(EXPECT_REPLY_FAILED);
            }
            LargeMessageTargetRequest::EncodeUnboundedMaybeLargeResource {
                populate_unset_handles,
                data,
                responder,
            } => {
                let mut elements = data.elements;
                if populate_unset_handles {
                    for element in elements.iter_mut() {
                        element.handle.get_or_insert_with(|| Event::create().into());
                    }
                }

                if let Err(err) = responder.send(elements) {
                    match err {
                        fidl::Error::LargeMessage64Handles => ctl_handle
                            .send_reply_encoding_failed(EncodingFailureKind::LargeMessage64Handles)
                            .expect("failed to report reply encoding failure"),
                        err => panic!("expected fidl::Error::LargeMessage64Handles, got: {err:?}"),
                    }
                }
            }
            LargeMessageTargetRequest::_UnknownMethod {
                ordinal,
                unknown_method_type,
                control_handle: _,
                ..
            } => {
                let unknown_method_type = match unknown_method_type {
                    UnknownMethodType::OneWay => DynsuiteUnknownMethodType::OneWay,
                    UnknownMethodType::TwoWay => DynsuiteUnknownMethodType::TwoWay,
                };
                ctl_handle
                    .send_received_unknown_method(ordinal, unknown_method_type)
                    .expect("failed to report unknown method call");
            }
        }
    }
    Ok(())
}

async fn run_runner_server(mut stream: RunnerRequestStream) -> Result<(), Error> {
    while let Some(request) = stream.try_next().await? {
        match request {
            RunnerRequest::IsTestEnabled { test, responder } => {
                let enabled = match test {
                    Test::IgnoreDisabled => {
                        // This case will forever be false, as it is intended to validate the
                        // "test disabling" functionality of the runner itself.
                        false
                    }
                    Test::OneWayWithNonZeroTxid | Test::TwoWayNoPayloadWithZeroTxid => {
                        // TODO(fxbug.dev/120742): Always validate txid.
                        false
                    }
                    Test::V1TwoWayNoPayload | Test::V1TwoWayStructPayload => {
                        // TODO(fxbug.dev/99738): Rust bindings should reject V1 wire format.
                        false
                    }
                    Test::EventSendingDoNotReportPeerClosed => {
                        // TODO(fxbug.dev/74241): There are many tests where the harness sends
                        // something invalid and expects a PEER_CLOSED. Right now the Rust bindings
                        // don't actually close the channel until everything is dropped, so to make
                        // things get dropped and avoid tests hanging, we shut down the controller
                        // after sending WillTeardown. That works for most tests, but breaks
                        // EventSendingDoNotReportPeerClosed where the harness calls SendStrictEvent
                        // after closing the target channel. Once we make channel closure happen
                        // more promptly, we can remove the ctl_handle.shutdown() calls below and
                        // re-enable this test.
                        false
                    }
                    _ => true,
                };
                responder.send(enabled)?;
            }
            RunnerRequest::IsTeardownReasonSupported { responder } => {
                responder.send(true)?;
            }
            RunnerRequest::Start { target, responder } => {
                println!("Runner.Start() called");
                match target {
                    AnyTarget::ClosedTarget(ClosedTargetServerPair { controller: ctl, sut }) => {
                        responder.send().expect("sending response failed");
                        let (ctl_stream, ctl_handle) = ctl.into_stream_and_control_handle()?;
                        let (sut_stream, sut_handle) = sut.into_stream_and_control_handle()?;
                        let ctl_fut = run_closed_target_controller_server(ctl_stream, &sut_handle)
                            .map(|result| result.context("serving controller"));
                        let sut_fut =
                            run_closed_target_server(sut_stream, &ctl_handle).map(|result| {
                                if let Err(ref err) = result {
                                    println!("ClosedTarget failed: {err:?}");
                                }
                                _ = ctl_handle
                                    .send_will_teardown(get_teardown_reason(result.err()));
                                ctl_handle.shutdown();
                                Ok(())
                            });
                        future::try_join(ctl_fut, sut_fut).await?;
                    }
                    AnyTarget::AjarTarget(AjarTargetServerPair { controller: ctl, sut }) => {
                        responder.send().expect("sending response failed");
                        let (ctl_stream, ctl_handle) = ctl.into_stream_and_control_handle()?;
                        let (sut_stream, sut_handle) = sut.into_stream_and_control_handle()?;
                        let ctl_fut = run_ajar_target_controller_server(ctl_stream, &sut_handle)
                            .map(|result| result.context("serving controller"));
                        let sut_fut =
                            run_ajar_target_server(sut_stream, &ctl_handle).map(|result| {
                                if let Err(ref err) = result {
                                    println!("AjarTarget failed: {err:?}");
                                }
                                _ = ctl_handle
                                    .send_will_teardown(get_teardown_reason(result.err()));
                                ctl_handle.shutdown();
                                Ok(())
                            });
                        future::try_join(ctl_fut, sut_fut).await?;
                    }
                    AnyTarget::OpenTarget(OpenTargetServerPair { controller: ctl, sut }) => {
                        responder.send().expect("sending response failed");
                        let (ctl_stream, ctl_handle) = ctl.into_stream_and_control_handle()?;
                        let (sut_stream, sut_handle) = sut.into_stream_and_control_handle()?;
                        let ctl_fut = run_open_target_controller_server(ctl_stream, &sut_handle)
                            .map(|result| result.context("serving controller"));
                        let sut_fut =
                            run_open_target_server(sut_stream, &ctl_handle).map(|result| {
                                if let Err(ref err) = result {
                                    println!("OpenTarget failed: {err:?}");
                                }
                                _ = ctl_handle
                                    .send_will_teardown(get_teardown_reason(result.err()));
                                ctl_handle.shutdown();
                                Ok(())
                            });
                        future::try_join(ctl_fut, sut_fut).await?;
                    }
                    AnyTarget::LargeMessageTarget(LargeMessageTargetServerPair {
                        controller: ctl,
                        sut,
                    }) => {
                        responder.send().expect("sending response failed");
                        let (ctl_stream, ctl_handle) = ctl.into_stream_and_control_handle()?;
                        let (sut_stream, sut_handle) = sut.into_stream_and_control_handle()?;
                        let ctl_fut =
                            run_large_message_target_controller_server(ctl_stream, &sut_handle)
                                .map(|result| result.context("serving controller"));
                        let sut_fut = run_large_message_target_server(sut_stream, &ctl_handle).map(
                            |result| {
                                if let Err(ref err) = result {
                                    println!("LargeMessageTarget failed: {err:?}");
                                }
                                _ = ctl_handle
                                    .send_will_teardown(get_teardown_reason(result.err()));
                                ctl_handle.shutdown();
                                Ok(())
                            },
                        );
                        future::try_join(ctl_fut, sut_fut).await?;
                    }
                }
            }
            RunnerRequest::CheckAlive { responder } => {
                responder.send().expect("sending response failed");
            }
        }
    }
    Ok(())
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
        run_runner_server(stream).unwrap_or_else(|e| panic!("Runner failed: {:?}", e))
    })
    .await;

    Ok(())
}
