// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::Channel;
use tracing::trace;

use crate::error::Error;
use crate::header::HeaderSet;
use crate::operation::{
    GetOperation, OpCode, RequestPacket, ResponseCode, ResponsePacket, MAX_PACKET_SIZE,
    MIN_MAX_PACKET_SIZE,
};
use crate::transport::ObexTransportManager;

/// Returns the maximum packet size that will be used for the OBEX session.
/// `transport_max` is the maximum size that the underlying transport (e.g. L2CAP, RFCOMM) supports.
fn max_packet_size_from_transport(transport_max: usize) -> u16 {
    let bounded = transport_max.clamp(MIN_MAX_PACKET_SIZE, MAX_PACKET_SIZE);
    bounded.try_into().expect("bounded by u16 max")
}

/// The Client role for an OBEX session.
/// Provides an interface for connecting to a remote OBEX server and initiating the operations
/// specified in OBEX 1.5.
#[derive(Debug)]
pub struct ObexClient {
    /// Whether the CONNECTED Operation has completed.
    connected: bool,
    /// The maximum OBEX packet length for this OBEX session.
    max_packet_size: u16,
    /// Manages the RFCOMM or L2CAP transport and provides a reservation system for starting
    /// new operations.
    transport: ObexTransportManager,
}

impl ObexClient {
    pub fn new(channel: Channel) -> Self {
        let max_packet_size = max_packet_size_from_transport(channel.max_tx_size());
        let transport = ObexTransportManager::new(channel);
        Self { connected: false, max_packet_size, transport }
    }

    fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    fn set_max_packet_size(&mut self, peer_max_packet_size: u16) {
        // We have no opinion on the preferred max packet size, so just use the peer's.
        self.max_packet_size = peer_max_packet_size;
        trace!("Max packet size set to {peer_max_packet_size}");
    }

    fn handle_connect_response(&mut self, response: ResponsePacket) -> Result<HeaderSet, Error> {
        let request = OpCode::Connect;
        let response = response.expect_code(request, ResponseCode::Ok)?;

        // Expect the 4 bytes of additional data. We negotiate the max packet length based on what
        // the peer requests. See OBEX 1.5 Section 3.4.1.
        if response.data().len() != request.response_data_length() {
            return Err(Error::response(request, "Invalid CONNECT data"));
        }
        let peer_max_packet_size = u16::from_be_bytes(response.data()[2..4].try_into().unwrap());
        self.set_max_packet_size(peer_max_packet_size);
        self.set_connected(true);
        Ok(response.into())
    }

    /// Initiates a CONNECT request to the remote peer.
    /// Returns the Headers associated with the response on success.
    /// Returns Error if the CONNECT operation could not be completed.
    pub async fn connect(&mut self, headers: HeaderSet) -> Result<HeaderSet, Error> {
        if self.connected {
            return Err(Error::operation(OpCode::Connect, "already connected"));
        }

        let response = {
            let request = RequestPacket::new_connect(self.max_packet_size, headers);
            let mut transport = self.transport.try_new_operation()?;
            trace!("Making outgoing CONNECT request: {request:?}");
            transport.send(request)?;
            trace!("Successfully made CONNECT request");
            transport.receive_response(OpCode::Connect).await?
        };

        let response_headers = self.handle_connect_response(response)?;
        Ok(response_headers)
    }

    /// Initializes a GET Operation to retrieve data from the remote OBEX Server.
    /// Returns a `GetOperation` on success, Error if the new operation couldn't be started.
    pub fn get(&mut self, headers: HeaderSet) -> Result<GetOperation<'_>, Error> {
        // A GET can only be initiated after the OBEX session is connected.
        if !self.connected {
            return Err(Error::operation(OpCode::Get, "session not connected"));
        }

        // Only one operation can be active at a time.
        let transport = self.transport.try_new_operation()?;
        Ok(GetOperation::new(headers, transport))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use async_utils::PollExt;
    use fuchsia_async as fasync;
    use futures::pin_mut;

    use crate::header::Header;
    use crate::transport::test_utils::expect_request_and_reply;

    #[fuchsia::test]
    fn max_packet_size_calculation() {
        // Value in [255, 65535] should be kept.
        let transport_max = 1000;
        assert_eq!(max_packet_size_from_transport(transport_max), 1000);

        // Lower bound should be enforced.
        let transport_max_small = 40;
        assert_eq!(max_packet_size_from_transport(transport_max_small), 255);

        // Upper bound should be enforced.
        let transport_max_large = 700000;
        assert_eq!(max_packet_size_from_transport(transport_max_large), std::u16::MAX);
    }

    /// Returns a new ObexClient and the remote end of the transport.
    /// If `connected` is set, returns an ObexClient in the connected state, indicating the
    /// completion of the OBEX CONNECT procedure.
    fn new_obex_client(connected: bool) -> (ObexClient, Channel) {
        let (local, remote) = Channel::create();
        let mut client = ObexClient::new(local);
        client.set_connected(connected);
        (client, remote)
    }

    #[fuchsia::test]
    fn obex_client_connect_success() {
        let mut exec = fasync::TestExecutor::new();
        let (mut client, mut remote) = new_obex_client(false);

        assert!(!client.connected);
        assert_eq!(client.max_packet_size, Channel::DEFAULT_MAX_TX as u16);

        {
            let headers = HeaderSet::from_headers(vec![]).unwrap();
            let connect_fut = client.connect(headers);
            pin_mut!(connect_fut);
            exec.run_until_stalled(&mut connect_fut).expect_pending("waiting for response");

            // Expect the Connect request on the remote and reply positively.
            let response_headers = HeaderSet::from_headers(vec![Header::ConnectionId(1)]).unwrap();
            let response = ResponsePacket::new(
                ResponseCode::Ok,
                vec![0x10, 0x00, 0xff, 0xff], // Version = 1.0, Flags = 0, Max packet = 0xffff
                response_headers.clone(),
            );
            expect_request_and_reply(&mut exec, &mut remote, OpCode::Connect, response);

            let connect_result = exec
                .run_until_stalled(&mut connect_fut)
                .expect("received response")
                .expect("response is ok");
            assert_eq!(connect_result, response_headers);
        }

        // Should be connected with the max packet size specified by the peer.
        assert!(client.connected);
        assert_eq!(client.max_packet_size, 0xffff);
    }

    #[fuchsia::test]
    async fn multiple_connect_is_error() {
        let (mut client, _remote) = new_obex_client(true);

        // Trying to connect again is an Error since it can only be done once.
        let result = client.connect(HeaderSet::new()).await;
        assert_matches!(result, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    fn get_before_connect_is_error() {
        let _exec = fasync::TestExecutor::new();
        let (mut client, _remote) = new_obex_client(false);

        let headers = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let get_result = client.get(headers);
        assert_matches!(get_result, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    fn sequential_get_operations_is_ok() {
        let _exec = fasync::TestExecutor::new();
        let (mut client, _remote) = new_obex_client(true);

        // Creating the first GET operation should succeed.
        let headers = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let _get_operation1 = client.get(headers.clone()).expect("can initialize first get");

        // After the first one "completes" (e.g. no longer held), it's okay to initiate a GET.
        drop(_get_operation1);
        let _get_operation2 = client.get(headers).expect("can initialize second get");
    }
}
