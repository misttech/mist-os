// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::Channel;
use futures::stream::{FusedStream, TryStreamExt};
use packet_encoding::Encodable;
use std::cell::{RefCell, RefMut};
use tracing::{info, trace};

use crate::error::{Error, PacketError};
use crate::operation::{OpCode, ResponsePacket};

/// The underlying communication protocol used for the OBEX transport.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TransportType {
    L2cap,
    Rfcomm,
}

impl TransportType {
    fn srm_supported(&self) -> bool {
        match &self {
            // Per GOEP Section 7.1, SRM can be used with the L2CAP transport.
            Self::L2cap => true,
            // Neither the OBEX nor GOEP specifications explicitly state that SRM cannot be used
            // with the RFCOMM transport. However, all qualification tests and spec language
            // suggest that SRM is to be used only on the L2CAP transport.
            Self::Rfcomm => false,
        }
    }
}

/// Holds the underlying RFCOMM or L2CAP transport for an OBEX operation.
#[derive(Debug)]
pub struct ObexTransport<'a> {
    /// A mutable reference to the permit given to the operation.
    /// The L2CAP or RFCOMM connection to the remote peer.
    channel: RefMut<'a, Channel>,
    /// The type of transport used in the OBEX connection.
    type_: TransportType,
}

impl<'a> ObexTransport<'a> {
    pub fn new(channel: RefMut<'a, Channel>, type_: TransportType) -> Self {
        Self { channel, type_ }
    }

    /// Returns true if this transport supports the Single Response Mode (SRM) feature.
    pub fn srm_supported(&self) -> bool {
        self.type_.srm_supported()
    }

    /// Encodes and sends the OBEX `data` to the remote peer.
    /// Returns Error if the send operation could not be completed.
    pub fn send(&self, data: impl Encodable<Error = PacketError>) -> Result<(), Error> {
        let mut buf = vec![0; data.encoded_len()];
        data.encode(&mut buf[..])?;
        let _ = self.channel.as_ref().write(&buf)?;
        Ok(())
    }

    /// Attempts to receive and parse an OBEX response packet from the `channel`.
    /// Returns the parsed packet on success, Error otherwise.
    // TODO(fxbug.dev/125307): Make this more generic to decode either request or response packets
    // when OBEX Server functionality is needed.
    pub async fn receive_response(&mut self, code: OpCode) -> Result<ResponsePacket, Error> {
        if self.channel.is_terminated() {
            return Err(Error::PeerDisconnected);
        }

        match self.channel.try_next().await? {
            Some(raw_data) => {
                let decoded = ResponsePacket::decode(&raw_data[..], code)?;
                trace!("Received response: {decoded:?}");
                Ok(decoded)
            }
            None => {
                info!("OBEX transport closed");
                Err(Error::PeerDisconnected)
            }
        }
    }
}

/// Manages the transport connection (L2CAP/RFCOMM) to a remote peer.
/// Provides a reservation system for acquiring the transport for an in-progress OBEX operation.
#[derive(Debug)]
pub struct ObexTransportManager {
    /// Holds the underlying transport. The type of transport is indicated by the `type_` field.
    /// There can only be one operation outstanding at any time. A mutable reference to the
    /// `Channel` will be held by the `ObexTransport` during an ongoing operation and is
    /// assigned using `ObexTransportManager::try_new_operation`. On operation termination (e.g.
    /// `ObexTransport` is dropped), the `Channel` will be available for subsequent mutable access.
    channel: RefCell<Channel>,
    /// The transport type (L2CAP or RFCOMM) for the `channel`.
    type_: TransportType,
}

impl ObexTransportManager {
    pub fn new(channel: Channel, type_: TransportType) -> Self {
        Self { channel: RefCell::new(channel), type_ }
    }

    fn new_permit(&self) -> Result<RefMut<'_, Channel>, Error> {
        self.channel.try_borrow_mut().map_err(|_| Error::OperationInProgress)
    }

    pub fn try_new_operation(&self) -> Result<ObexTransport<'_>, Error> {
        // Only one operation can be outstanding at a time.
        let channel = self.new_permit()?;
        Ok(ObexTransport::new(channel, self.type_))
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use super::*;

    use async_test_helpers::expect_stream_item;
    use fuchsia_async as fasync;
    use packet_encoding::Decodable;

    use crate::operation::RequestPacket;

    /// Set `srm_supported` to true to build a transport that supports the OBEX SRM feature.
    pub(crate) fn new_manager(srm_supported: bool) -> (ObexTransportManager, Channel) {
        let (local, remote) = Channel::create();
        let type_ = if srm_supported { TransportType::L2cap } else { TransportType::Rfcomm };
        let manager = ObexTransportManager::new(local, type_);
        (manager, remote)
    }

    #[derive(Clone)]
    pub struct TestPacket(pub u8);

    impl Encodable for TestPacket {
        type Error = PacketError;
        fn encoded_len(&self) -> usize {
            1
        }
        fn encode(&self, buf: &mut [u8]) -> Result<(), Self::Error> {
            buf[0] = self.0;
            Ok(())
        }
    }

    impl Decodable for TestPacket {
        type Error = PacketError;
        fn decode(buf: &[u8]) -> Result<Self, Self::Error> {
            Ok(TestPacket(buf[0]))
        }
    }

    #[track_caller]
    pub fn reply(channel: &mut Channel, response: ResponsePacket) {
        let mut response_buf = vec![0; response.encoded_len()];
        response.encode(&mut response_buf[..]).expect("can encode response");
        let _ = channel.as_ref().write(&response_buf[..]).expect("write to channel success");
    }

    #[track_caller]
    pub fn expect_request<F>(exec: &mut fasync::TestExecutor, channel: &mut Channel, expectation: F)
    where
        F: FnOnce(RequestPacket),
    {
        let request_raw = expect_stream_item(exec, channel).expect("request");
        let request = RequestPacket::decode(&request_raw[..]).expect("can decode request");
        expectation(request);
    }

    /// Expects a request packet on the `channel` and validates the contents with the provided
    /// `expectation`. Sends a `response` back on the channel.
    #[track_caller]
    pub fn expect_request_and_reply<F>(
        exec: &mut fasync::TestExecutor,
        channel: &mut Channel,
        expectation: F,
        response: ResponsePacket,
    ) where
        F: FnOnce(RequestPacket),
    {
        expect_request(exec, channel, expectation);
        reply(channel, response)
    }

    pub fn expect_code(code: OpCode) -> impl FnOnce(RequestPacket) {
        let f = move |request: RequestPacket| {
            assert_eq!(*request.code(), code);
        };
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;

    use async_utils::PollExt;
    use fuchsia_async as fasync;
    use futures::pin_mut;

    use crate::header::HeaderSet;
    use crate::operation::{RequestPacket, ResponseCode};
    use crate::transport::test_utils::{
        expect_code, expect_request_and_reply, new_manager, TestPacket,
    };

    #[fuchsia::test]
    fn transport_manager_new_operation() {
        let _exec = fasync::TestExecutor::new();
        let (manager, _remote) = new_manager(/* srm_supported */ false);

        // Nothing should be in progress.
        assert_matches!(manager.new_permit(), Ok(_));

        // Should be able to start a new operation.
        let transport1 = manager.try_new_operation().expect("can start operation");
        // Trying to start another should be an Error.
        assert_matches!(manager.try_new_operation(), Err(Error::OperationInProgress));

        // Once the first finishes, another can be claimed.
        drop(transport1);
        let transport2 = manager.try_new_operation().expect("can start another operation");
        let request = RequestPacket::new_connect(100, HeaderSet::new());
        transport2.send(request).expect("can send request");
    }

    #[fuchsia::test]
    fn send_and_receive() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
        let mut transport = manager.try_new_operation().expect("can start operation");

        // Local makes a request
        let request = RequestPacket::new_connect(100, HeaderSet::new());
        transport.send(request).expect("can send request");
        // Remote end should receive it - send an example response back.
        let peer_response =
            ResponsePacket::new(ResponseCode::Ok, vec![0x10, 0x00, 0x00, 0xff], HeaderSet::new());
        expect_request_and_reply(
            &mut exec,
            &mut remote,
            expect_code(OpCode::Connect),
            peer_response,
        );
        // Expect it on the ObexTransport
        let receive_fut = transport.receive_response(OpCode::Connect);
        pin_mut!(receive_fut);
        let received_response = exec
            .run_until_stalled(&mut receive_fut)
            .expect("stream item from response")
            .expect("valid response");
        assert_eq!(*received_response.code(), ResponseCode::Ok);
    }

    #[fuchsia::test]
    async fn send_while_channel_closed_is_error() {
        let (manager, remote) = new_manager(/* srm_supported */ false);
        let transport = manager.try_new_operation().expect("can start operation");
        drop(remote);

        let data = TestPacket(10);
        let send_result = transport.send(data.clone());
        assert_matches!(send_result, Err(Error::IOError(_)));
        // Trying again is still an Error.
        let send_result = transport.send(data.clone());
        assert_matches!(send_result, Err(Error::IOError(_)));
    }

    #[fuchsia::test]
    async fn receive_while_channel_closed_is_error() {
        let (manager, remote) = new_manager(/* srm_supported */ false);
        let mut transport = manager.try_new_operation().expect("can start operation");
        drop(remote);

        let receive_result = transport.receive_response(OpCode::Connect).await;
        assert_matches!(receive_result, Err(Error::PeerDisconnected));
        // Trying again is handled gracefully - still an Error.
        let receive_result = transport.receive_response(OpCode::Connect).await;
        assert_matches!(receive_result, Err(Error::PeerDisconnected));
    }
}
