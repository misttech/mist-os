// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::RequestStream;

use {
    anyhow::{Context as _, Error},
    fidl::endpoints::{ControlHandle, ServerEnd, UnknownMethodType},
    fidl::{AsHandleRef, Event, Status},
    fidl_fidl_serversuite::{
        AjarTargetControllerMarker, AjarTargetMarker, AjarTargetRequest, AjarTargetServerPair,
        AnyTarget, ClosedTargetControllerMarker, ClosedTargetControllerRequest, ClosedTargetMarker,
        ClosedTargetRequest, ClosedTargetServerPair, ClosedTargetTwoWayResultRequest,
        ClosedTargetTwoWayTablePayloadResponse, ClosedTargetTwoWayUnionPayloadRequest,
        ClosedTargetTwoWayUnionPayloadResponse, Elements, Empty, EncodingFailureKind,
        LargeMessageTargetControllerMarker, LargeMessageTargetMarker,
        LargeMessageTargetOneWayMethod, LargeMessageTargetRequest, LargeMessageTargetServerPair,
        OpenTargetControllerMarker, OpenTargetControllerRequest,
        OpenTargetFlexibleTwoWayErrRequest, OpenTargetFlexibleTwoWayFieldsErrRequest,
        OpenTargetMarker, OpenTargetRequest, OpenTargetServerPair,
        OpenTargetStrictTwoWayErrRequest, OpenTargetStrictTwoWayFieldsErrRequest, RunnerRequest,
        RunnerRequestStream, SendEventError, Test, UnknownMethodType as DynsuiteUnknownMethodType,
    },
    fuchsia_component::server::ServiceFs,
    futures::prelude::*,
};

fn peek_to_anyhow<P, E>(peeker: P) -> impl FnOnce(E) -> Error
where
    P: FnOnce(&Error),
    E: Into<Error>,
{
    |e| {
        let e = e.into();
        peeker(&e);
        e
    }
}

const EXPECT_ONE_WAY: &'static str = "failed to report one way request";
const EXPECT_REPLY_FAILED: &'static str = "failed to send reply";

fn get_teardown_reason(error: &Error) -> fidl_fidl_serversuite::TeardownReason {
    use fidl_fidl_serversuite::TeardownReason;
    match error.downcast_ref::<fidl::Error>() {
        Some(error) => match error {
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
            fidl::Error::UnknownOrdinal { .. }
            | fidl::Error::InvalidResponseTxid
            | fidl::Error::UnexpectedSyncResponse => TeardownReason::UnexpectedMessage,
            fidl::Error::UnsupportedMethod { .. } => {
                panic!("UnsupportedMethod should not be seen on server side")
            }
            fidl::Error::ClientChannelClosed { .. } => TeardownReason::ChannelPeerClosed,
            _ => TeardownReason::Other,
        },
        None => TeardownReason::Other,
    }
}

fn classify_send_event_error(_err: fidl::Error) -> SendEventError {
    SendEventError::OtherError
}

async fn run_closed_target_server(
    controller: ServerEnd<ClosedTargetControllerMarker>,
    sut: ServerEnd<ClosedTargetMarker>,
) -> Result<(), Error> {
    let controller_stream = controller.into_stream()?;
    let controller_handle = &controller_stream.control_handle();

    let sut_stream = sut.into_stream().map_err(peek_to_anyhow(|e| {
        _ = controller_handle.send_will_teardown(get_teardown_reason(e));
    }))?;
    let sut_handle = &sut_stream.control_handle();

    let controller_runner = controller_stream
        .map(|result| result.context("failed controller request"))
        .try_for_each(|request| async move {
            println!("Got controller request");
            match request {
                ClosedTargetControllerRequest::CloseWithEpitaph {
                    epitaph_status,
                    control_handle: _,
                } => {
                    sut_handle.shutdown_with_epitaph(Status::from_raw(epitaph_status));
                    println!("Sent shutdown_with_epitaph");
                }
            }
            Ok(())
        });

    let sut_runner = sut_stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                ClosedTargetRequest::OneWayNoPayload { control_handle: _ } => {
                    println!("OneWayNoPayload");
                    controller_handle
                        .send_received_one_way_no_payload()
                        .expect("sending ReceivedOneWayNoPayload failed");
                }
                ClosedTargetRequest::TwoWayNoPayload { responder } => {
                    println!("TwoWayNoPayload");
                    responder.send().expect("failed to send two way payload response");
                }
                ClosedTargetRequest::TwoWayStructPayload { v, responder } => {
                    println!("TwoWayStructPayload");
                    responder.send(v).expect("failed to send two way payload response");
                }
                ClosedTargetRequest::TwoWayTablePayload { payload, responder } => {
                    println!("TwoWayTablePayload");
                    responder
                        .send(ClosedTargetTwoWayTablePayloadResponse {
                            v: payload.v,
                            ..ClosedTargetTwoWayTablePayloadResponse::EMPTY
                        })
                        .expect("failed to send two way payload response");
                }
                ClosedTargetRequest::TwoWayUnionPayload { payload, responder } => {
                    println!("TwoWayUnionPayload");
                    let v = match payload {
                        ClosedTargetTwoWayUnionPayloadRequest::V(v) => v,
                        _ => {
                            panic!("unexpected union value");
                        }
                    };
                    responder
                        .send(&mut ClosedTargetTwoWayUnionPayloadResponse::V(v))
                        .expect("failed to send two way payload response");
                }
                ClosedTargetRequest::TwoWayResult { payload, responder } => {
                    println!("TwoWayResult");
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
                    let basic_info = handle
                        .as_handle_ref()
                        .basic_info()
                        .expect("failed to get basic handle info");
                    let rights = fidl_zx::Rights::from_bits(basic_info.rights.bits())
                        .expect("bits should be valid");
                    responder.send(rights).expect("failed to send response");
                }
                ClosedTargetRequest::GetSignalableEventRights { handle, responder } => {
                    let basic_info = handle
                        .as_handle_ref()
                        .basic_info()
                        .expect("failed to get basic handle info");
                    let rights = fidl_zx::Rights::from_bits(basic_info.rights.bits())
                        .expect("bits should be valid");
                    responder.send(rights).expect("failed to send response");
                }
                ClosedTargetRequest::EchoAsTransferableSignalableEvent { handle, responder } => {
                    responder.send(fidl::Event::from(handle)).expect("failed to send response");
                }
                ClosedTargetRequest::ByteVectorSize { vec, responder } => {
                    responder.send(vec.len().try_into().unwrap()).expect("failed to send response");
                }
                ClosedTargetRequest::HandleVectorSize { vec, responder } => {
                    responder.send(vec.len().try_into().unwrap()).expect("failed to send response");
                }
                ClosedTargetRequest::CreateNByteVector { n, responder } => {
                    let bytes: Vec<u8> = vec![0; n.try_into().unwrap()];
                    responder.send(&bytes).expect("failed to send response");
                }
                ClosedTargetRequest::CreateNHandleVector { n, responder } => {
                    let mut handles: Vec<fidl::Event> = Vec::new();
                    for _ in 0..n {
                        handles.push(Event::create());
                    }
                    responder.send(&mut handles.into_iter()).expect("failed to send response");
                }
            }
            Ok(())
        })
        .map_err(peek_to_anyhow(|e| {
            _ = controller_handle.send_will_teardown(get_teardown_reason(e));
        }));

    future::try_join(controller_runner, sut_runner).await?;
    Ok(())
}

async fn run_ajar_target_server(
    controller: ServerEnd<AjarTargetControllerMarker>,
    sut: ServerEnd<AjarTargetMarker>,
) -> Result<(), Error> {
    println!("Running Ajar Target");
    let controller_stream = controller.into_stream()?;
    let controller_handle = &controller_stream.control_handle();

    let controller_runner = controller_stream
        .map(|result| result.context("failed controller request"))
        .try_for_each(|request| async move { match request {} });

    let sut_runner = sut
        .into_stream()
        .map_err(peek_to_anyhow(|e| {
            _ = controller_handle.send_will_teardown(get_teardown_reason(e));
        }))?
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                AjarTargetRequest::_UnknownMethod { ordinal, control_handle: _, .. } => {
                    controller_handle
                        .send_received_unknown_method(ordinal, DynsuiteUnknownMethodType::OneWay)
                        .expect("failed to report unknown method call");
                }
            }
            Ok(())
        })
        .map_err(peek_to_anyhow(|e| {
            _ = controller_handle.send_will_teardown(get_teardown_reason(e));
        }));

    future::try_join(controller_runner, sut_runner).await?;
    Ok(())
}

async fn run_open_target_server(
    controller: ServerEnd<OpenTargetControllerMarker>,
    sut: ServerEnd<OpenTargetMarker>,
) -> Result<(), Error> {
    println!("Running Open Target");
    let controller_stream = controller.into_stream()?;
    let controller_handle = &controller_stream.control_handle();
    let sut_stream = sut.into_stream().map_err(peek_to_anyhow(|e| {
        _ = controller_handle.send_will_teardown(get_teardown_reason(e));
    }))?;
    let sut_handle = &sut_stream.control_handle();

    let controller_runner = controller_stream
        .map(|result| result.context("failed controller request"))
        .try_for_each(|request| async move {
            match request {
                OpenTargetControllerRequest::SendStrictEvent { responder } => {
                    let mut res = sut_handle.send_strict_event().map_err(classify_send_event_error);
                    responder.send(&mut res).expect(EXPECT_REPLY_FAILED);
                }
                OpenTargetControllerRequest::SendFlexibleEvent { responder } => {
                    let mut res =
                        sut_handle.send_flexible_event().map_err(classify_send_event_error);
                    responder.send(&mut res).expect(EXPECT_REPLY_FAILED);
                }
            }
            Ok(())
        });

    let sut_runner = sut_stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                OpenTargetRequest::StrictOneWay { .. } => {
                    controller_handle.send_received_strict_one_way().expect(EXPECT_ONE_WAY);
                }
                OpenTargetRequest::FlexibleOneWay { .. } => {
                    controller_handle.send_received_flexible_one_way().expect(EXPECT_ONE_WAY);
                }
                OpenTargetRequest::StrictTwoWay { responder } => {
                    responder.send().expect(EXPECT_REPLY_FAILED);
                }
                OpenTargetRequest::StrictTwoWayFields { reply_with, responder } => {
                    responder.send(reply_with).expect(EXPECT_REPLY_FAILED);
                }
                OpenTargetRequest::StrictTwoWayErr { payload, responder } => match payload {
                    OpenTargetStrictTwoWayErrRequest::ReplySuccess(Empty) => {
                        responder.send(&mut Ok(())).expect(EXPECT_REPLY_FAILED);
                    }
                    OpenTargetStrictTwoWayErrRequest::ReplyError(reply_error) => {
                        responder.send(&mut Err(reply_error)).expect(EXPECT_REPLY_FAILED);
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
                        responder.send(&mut Ok(())).expect(EXPECT_REPLY_FAILED);
                    }
                    OpenTargetFlexibleTwoWayErrRequest::ReplyError(reply_error) => {
                        responder.send(&mut Err(reply_error)).expect(EXPECT_REPLY_FAILED);
                    }
                },
                OpenTargetRequest::FlexibleTwoWayFieldsErr { payload, responder } => {
                    match payload {
                        OpenTargetFlexibleTwoWayFieldsErrRequest::ReplySuccess(reply_success) => {
                            responder.send(&mut Ok(reply_success)).expect(EXPECT_REPLY_FAILED);
                        }
                        OpenTargetFlexibleTwoWayFieldsErrRequest::ReplyError(reply_error) => {
                            responder.send(&mut Err(reply_error)).expect(EXPECT_REPLY_FAILED);
                        }
                    }
                }
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
                    controller_handle
                        .send_received_unknown_method(ordinal, unknown_method_type)
                        .expect("failed to report unknown method call");
                }
            }
            Ok(())
        })
        .map_err(peek_to_anyhow(|e| {
            _ = controller_handle.send_will_teardown(get_teardown_reason(e));
        }));

    future::try_join(controller_runner, sut_runner).await?;
    Ok(())
}

async fn run_large_message_target_server(
    controller: ServerEnd<LargeMessageTargetControllerMarker>,
    sut: ServerEnd<LargeMessageTargetMarker>,
) -> Result<(), Error> {
    let controller_stream = controller.into_stream()?;
    let controller_handle = &controller_stream.control_handle();

    let controller_stream = controller_stream
        .map(|result| result.context("failed controller request"))
        .try_for_each(|request| async move { match request {} });

    let sut_stream = sut
        .into_stream()
        .map_err(peek_to_anyhow(|e| {
            _ = controller_handle.send_will_teardown(get_teardown_reason(e));
        }))?
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                LargeMessageTargetRequest::DecodeBoundedKnownToBeSmall {
                    bytes: _,
                    control_handle: _,
                } => {
                    controller_handle
                        .send_received_one_way(
                            LargeMessageTargetOneWayMethod::DecodeBoundedKnownToBeSmall,
                        )
                        .expect(EXPECT_ONE_WAY);
                }
                LargeMessageTargetRequest::DecodeBoundedMaybeLarge {
                    bytes: _,
                    control_handle: _,
                } => {
                    controller_handle
                        .send_received_one_way(
                            LargeMessageTargetOneWayMethod::DecodeBoundedMaybeLarge,
                        )
                        .expect(EXPECT_ONE_WAY);
                }
                LargeMessageTargetRequest::DecodeSemiBoundedBelievedToBeSmall {
                    payload: _,
                    control_handle: _,
                } => {
                    controller_handle
                        .send_received_one_way(
                            LargeMessageTargetOneWayMethod::DecodeSemiBoundedBelievedToBeSmall,
                        )
                        .expect(EXPECT_ONE_WAY);
                }
                LargeMessageTargetRequest::DecodeSemiBoundedMaybeLarge {
                    payload: _,
                    control_handle: _,
                } => {
                    controller_handle
                        .send_received_one_way(
                            LargeMessageTargetOneWayMethod::DecodeSemiBoundedMaybeLarge,
                        )
                        .expect(EXPECT_ONE_WAY);
                }
                LargeMessageTargetRequest::DecodeUnboundedMaybeLargeValue {
                    bytes: _,
                    control_handle: _,
                } => {
                    controller_handle
                        .send_received_one_way(
                            LargeMessageTargetOneWayMethod::DecodeUnboundedMaybeLargeValue,
                        )
                        .expect(EXPECT_ONE_WAY);
                }
                LargeMessageTargetRequest::DecodeUnboundedMaybeLargeResource {
                    elements: _,
                    control_handle: _,
                } => {
                    controller_handle
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
                    mut payload,
                    responder,
                } => {
                    responder.send(&mut payload).expect(EXPECT_REPLY_FAILED);
                }
                LargeMessageTargetRequest::EncodeSemiBoundedMaybeLarge {
                    mut payload,
                    responder,
                } => {
                    responder.send(&mut payload).expect(EXPECT_REPLY_FAILED);
                }
                LargeMessageTargetRequest::EncodeUnboundedMaybeLargeValue { bytes, responder } => {
                    responder.send(&bytes).expect(EXPECT_REPLY_FAILED);
                }
                LargeMessageTargetRequest::EncodeUnboundedMaybeLargeResource {
                    populate_unset_handles,
                    mut data,
                    responder,
                } => {
                    let mut elements = TryInto::<[&mut Elements; 64]>::try_into(
                        data.elements
                            .iter_mut()
                            .map(|element| {
                                if populate_unset_handles {
                                    match element.handle {
                                        None => {
                                            element.handle = Some(Event::create().into());
                                        }
                                        _ => {}
                                    }
                                }
                                element
                            })
                            .collect::<Vec<&mut Elements>>(),
                    )
                    .unwrap();

                    if let Err(err) = responder.send(&mut elements) {
                        match err {
                            fidl::Error::LargeMessage64Handles => controller_handle
                                .send_reply_encoding_failed(
                                    EncodingFailureKind::LargeMessage64Handles,
                                )
                                .expect("failed to report reply encoding failure"),
                            _ => panic!("{}", EXPECT_REPLY_FAILED),
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
                    controller_handle
                        .send_received_unknown_method(ordinal, unknown_method_type)
                        .expect("failed to report unknown method call");
                }
            }
            Ok(())
        })
        .map_err(peek_to_anyhow(|e| {
            _ = controller_handle.send_will_teardown(get_teardown_reason(e));
        }));

    future::try_join(controller_stream, sut_stream).await?;
    Ok(())
}

async fn run_runner_server(stream: RunnerRequestStream) -> Result<(), Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                RunnerRequest::IsTestEnabled { test, responder } => {
                    let enabled = match test {
                        Test::IgnoreDisabled => {
                            // This case will forever be false, as it is intended to validate the
                            // "test disabling" functionality of the runner itself.
                            false
                        }
                        // TODO(fxbug.dev/74241): shutdown_with_epitaph sends an epitaph and puts
                        // the server in a shutting down state, but doesn't actually close the
                        // channel until dropped.
                        Test::ServerSendsEpitaph => false,
                        Test::OneWayWithNonZeroTxid
                        | Test::TwoWayNoPayloadWithZeroTxid
                        | Test::ServerSendsTooFewRights
                        | Test::ResponseExceedsByteLimit
                        | Test::ResponseExceedsHandleLimit => false,
                        Test::V1TwoWayNoPayload | Test::V1TwoWayStructPayload => {
                            // TODO(fxbug.dev/99738): Rust bindings should reject V1 wire format.
                            false
                        }
                        Test::EventSendingDoNotReportPeerClosed
                        | Test::ReplySendingDoNotReportPeerClosed => {
                            // TODO(fxbug.dev/113160): Peer-closed errors should be
                            // hidden from one-way calls.
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
                        AnyTarget::ClosedTarget(ClosedTargetServerPair { controller, sut }) => {
                            responder.send().expect("sending response failed");
                            run_closed_target_server(controller, sut).await.unwrap_or_else(|e| {
                                println!("closed target server failed {:?}", e);
                            });
                        }
                        AnyTarget::AjarTarget(AjarTargetServerPair { controller, sut }) => {
                            responder.send().expect("sending response failed");
                            run_ajar_target_server(controller, sut).await.unwrap_or_else(|e| {
                                println!("ajar target server failed {:?}", e);
                            });
                        }
                        AnyTarget::OpenTarget(OpenTargetServerPair { controller, sut }) => {
                            responder.send().expect("sending response failed");
                            run_open_target_server(controller, sut).await.unwrap_or_else(|e| {
                                println!("open target server failed {:?}", e);
                            });
                        }
                        AnyTarget::LargeMessageTarget(LargeMessageTargetServerPair {
                            controller,
                            sut,
                        }) => {
                            responder.send().expect("sending response failed");
                            run_large_message_target_server(controller, sut).await.unwrap_or_else(
                                |e| {
                                    println!("large message target server failed {:?}", e);
                                },
                            );
                        }
                    }
                }
                RunnerRequest::CheckAlive { responder } => {
                    responder.send().expect("sending response failed");
                }
            }
            Ok(())
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
