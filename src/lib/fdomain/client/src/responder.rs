// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{ordinals, Error};
use fidl_fuchsia_fdomain as proto;
use futures::channel::oneshot::Sender;

/// Sum type over oneshot senders which carry the responses to FDomain FIDL
/// requests back to the requesting code.
///
/// A couple of the variants also carry the HID the original operation was
/// performed on. This is used when we get a write error in response to those
/// operations and need to clear it by sending an AcknowledgeWriteError message.
pub(crate) enum Responder {
    Namespace(Sender<Result<(), Error>>),
    CreateChannel(Sender<Result<(), Error>>),
    CreateSocket(Sender<Result<(), Error>>),
    CreateEventPair(Sender<Result<(), Error>>),
    CreateEvent(Sender<Result<(), Error>>),
    SetSocketDisposition(Sender<Result<(), Error>>),
    ReadSocket(Sender<Result<proto::SocketReadSocketResponse, Error>>),
    ReadChannel(Sender<Result<proto::ChannelMessage, Error>>),
    WriteSocket(Sender<Result<proto::SocketWriteSocketResponse, Error>>, proto::Hid),
    WriteChannel(Sender<Result<(), Error>>, proto::Hid),
    Close(Sender<Result<(), Error>>),
    Duplicate(Sender<Result<(), Error>>),
    Replace(Sender<Result<(), Error>>),
    Signal(Sender<Result<(), Error>>),
    SignalPeer(Sender<Result<(), Error>>),
    WaitForSignals(Sender<Result<proto::FDomainWaitForSignalsResponse, Error>>),

    // We always use the Ignore variant for these, but implementation is here
    // for posterity.
    _AcknowledgeWriteError(Sender<Result<(), Error>>),
    _ReadChannelStreamingStart(Sender<Result<(), Error>>),
    _ReadChannelStreamingStop(Sender<Result<(), Error>>),
    _ReadSocketStreamingStart(Sender<Result<(), Error>>),
    _ReadSocketStreamingStop(Sender<Result<(), Error>>),

    /// Used when we want to ignore the reply to a request.
    Ignore,
}

/// Result after calling a responder. Indicates whether we need to acknowledge a write error.
pub(crate) enum ResponderStatus {
    Ok,
    WriteErrorOccurred(proto::Hid),
}

impl Responder {
    /// Feed this responder a still-encoded FIDL request.
    pub(crate) fn handle(
        self,
        result: Result<(fidl_message::TransactionHeader, &[u8]), crate::InnerError>,
    ) -> fidl::Result<ResponderStatus> {
        match self {
            Responder::Namespace(sender) => {
                Responder::dispatch_handle("namespace", ordinals::NAMESPACE, sender, result)
            }
            Responder::CreateChannel(sender) => Responder::dispatch_handle(
                "create_channel",
                ordinals::CREATE_CHANNEL,
                sender,
                result,
            ),
            Responder::CreateSocket(sender) => {
                Responder::dispatch_handle("create_socket", ordinals::CREATE_SOCKET, sender, result)
            }
            Responder::CreateEventPair(sender) => Responder::dispatch_handle(
                "create_event_pair",
                ordinals::CREATE_EVENT_PAIR,
                sender,
                result,
            ),
            Responder::CreateEvent(sender) => {
                Responder::dispatch_handle("create_event", ordinals::CREATE_EVENT, sender, result)
            }
            Responder::SetSocketDisposition(sender) => Responder::dispatch_handle(
                "set_socket_disposition",
                ordinals::SET_SOCKET_DISPOSITION,
                sender,
                result,
            ),
            Responder::ReadSocket(sender) => {
                Responder::dispatch_handle("read_socket", ordinals::READ_SOCKET, sender, result)
            }
            Responder::ReadChannel(sender) => {
                Responder::dispatch_handle("read_channel", ordinals::READ_CHANNEL, sender, result)
            }
            Responder::WriteSocket(sender, handle) => {
                Responder::dispatch_handle_etc::<_, proto::WriteSocketError>(
                    "write_socket",
                    ordinals::WRITE_SOCKET,
                    sender,
                    result,
                    Some(handle),
                )
            }
            Responder::WriteChannel(sender, handle) => {
                Responder::dispatch_handle_etc::<_, proto::WriteChannelError>(
                    "write_channel",
                    ordinals::WRITE_CHANNEL,
                    sender,
                    result,
                    Some(handle),
                )
            }
            Responder::_AcknowledgeWriteError(sender) => Responder::dispatch_handle(
                "acknowledge_write_error",
                ordinals::ACKNOWLEDGE_WRITE_ERROR,
                sender,
                result,
            ),
            Responder::WaitForSignals(sender) => Responder::dispatch_handle(
                "wait_for_signals",
                ordinals::WAIT_FOR_SIGNALS,
                sender,
                result,
            ),
            Responder::Close(sender) => {
                Responder::dispatch_handle("close", ordinals::CLOSE, sender, result)
            }
            Responder::Duplicate(sender) => {
                Responder::dispatch_handle("duplicate", ordinals::DUPLICATE, sender, result)
            }
            Responder::Replace(sender) => {
                Responder::dispatch_handle("replace", ordinals::REPLACE, sender, result)
            }
            Responder::Signal(sender) => {
                Responder::dispatch_handle("signal", ordinals::SIGNAL, sender, result)
            }
            Responder::SignalPeer(sender) => {
                Responder::dispatch_handle("signal_peer", ordinals::SIGNAL_PEER, sender, result)
            }
            Responder::_ReadChannelStreamingStart(sender) => Responder::dispatch_handle(
                "read_channel_streaming_start",
                ordinals::READ_CHANNEL_STREAMING_START,
                sender,
                result,
            ),
            Responder::_ReadChannelStreamingStop(sender) => Responder::dispatch_handle(
                "read_channel_streaming_stop",
                ordinals::READ_CHANNEL_STREAMING_STOP,
                sender,
                result,
            ),
            Responder::_ReadSocketStreamingStart(sender) => Responder::dispatch_handle(
                "read_socket_streaming_start",
                ordinals::READ_SOCKET_STREAMING_START,
                sender,
                result,
            ),
            Responder::_ReadSocketStreamingStop(sender) => Responder::dispatch_handle(
                "read_socket_streaming_stop",
                ordinals::READ_SOCKET_STREAMING_STOP,
                sender,
                result,
            ),
            Responder::Ignore => Ok(ResponderStatus::Ok),
        }
    }

    /// Complete the `handle` method for a `Responder`. Does not take the
    /// responder itself; when this is called the responder has been unwrapped,
    /// and the type arguments to this method encode what was learned from the
    /// variant.
    fn dispatch_handle<R: fidl_message::Body>(
        method_name: &'static str,
        ordinal: u64,
        sender: Sender<Result<R, Error>>,
        result: Result<(fidl_message::TransactionHeader, &[u8]), crate::InnerError>,
    ) -> fidl::Result<ResponderStatus> {
        Self::dispatch_handle_etc::<R, proto::Error>(method_name, ordinal, sender, result, None)
    }

    /// Same as `dispatch_handle` except the error type is generic, whereas it
    /// may only be `proto::Error` for `dispatch_handle`.
    fn dispatch_handle_etc<R: fidl_message::Body, S: Into<Error> + fidl_message::ErrorType>(
        method_name: &'static str,
        ordinal: u64,
        sender: Sender<Result<R, Error>>,
        result: Result<(fidl_message::TransactionHeader, &[u8]), crate::InnerError>,
        write_notify: Option<proto::Hid>,
    ) -> fidl::Result<ResponderStatus> {
        match result {
            Ok((header, body)) => {
                if header.ordinal != ordinal {
                    return Err(fidl::Error::InvalidResponseTxid);
                }
                let (res, ret) = match fidl_message::decode_response_flexible_result::<R, S>(
                    header, body,
                ) {
                    Ok(fidl_message::MaybeUnknown::Known(x)) => {
                        let status = if let (Some(handle), true) = (write_notify, x.is_err()) {
                            ResponderStatus::WriteErrorOccurred(handle)
                        } else {
                            ResponderStatus::Ok
                        };

                        (x.map_err(Into::into), Ok(status))
                    },
                    Ok(fidl_message::MaybeUnknown::Unknown) => {
                        (Err(Error::Protocol(fidl::Error::UnsupportedMethod {
                            method_name,
                            protocol_name:
                            <proto::FDomainMarker as fidl::endpoints::ProtocolMarker>::DEBUG_NAME
                        })), Ok(ResponderStatus::Ok))
                    }
                    Err(e) => {
                        (Err(Error::Protocol(e.clone())), Err(e))
                    }
                };
                let _ = sender.send(res);
                ret
            }
            Err(e) => {
                let _ = sender.send(Err(e.into()));
                Ok(ResponderStatus::Ok)
            }
        }
    }
}
