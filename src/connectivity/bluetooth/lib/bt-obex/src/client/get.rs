// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use tracing::trace;

use crate::error::Error;
use crate::header::HeaderSet;
use crate::operation::{OpCode, RequestPacket, ResponseCode, ResponsePacket};
use crate::transport::ObexTransport;

/// Represents an in-progress GET Operation.
///
/// `GetOperation::start` _must_ be called before either `GetOperation::get_information` or
/// `GetOperation::get_data` is called.
///
/// Example Usage:
/// ```
/// let obex_client = ObexClient::new(..);
/// let initial_headers: HeaderSet = ...;
/// let get_operation = obex_client.get(initial_headers)?;
/// let received_headers = get_operation.start().await?;
/// // Process `received_headers` and optionally make another request for more information.
/// let additional_headers: HeaderSet = ...;
/// let received_headers = get_operation.get_information(additional_headers).await?;
/// // Make the "final" request to get data.
/// let body = get_operation.get_data().await?;
/// // Process `body` - `get_operation` is consumed and the operation is assumed to be complete.
/// ```
#[must_use]
#[derive(Debug)]
pub struct GetOperation<'a> {
    /// The initial set of headers used in the GET operation. This is Some<T> when the operation is
    /// first initialized, and None after `GetOperation::start` has been called. The operation is
    /// considered "in progress" when this is None.
    headers: Option<HeaderSet>,
    /// The L2CAP or RFCOMM connection to the remote peer.
    transport: ObexTransport<'a>,
}

impl<'a> GetOperation<'a> {
    pub fn new(headers: HeaderSet, transport: ObexTransport<'a>) -> Self {
        Self { headers: Some(headers), transport }
    }

    fn is_started(&self) -> bool {
        self.headers.is_none()
    }

    /// Processes the `response` to a GET request and returns the set of headers on success, Error
    /// otherwise.
    fn handle_get_response(response: ResponsePacket) -> Result<HeaderSet, Error> {
        response.expect_code(OpCode::Get, ResponseCode::Continue).map(Into::into)
    }

    /// Processes the `response` to a GETFINAL request and returns a flag indicating the
    /// terminal user data payload and the payload.
    fn handle_get_final_response(response: ResponsePacket) -> Result<(bool, Vec<u8>), Error> {
        // If the response code is OK then this is the final body packet of the GET request.
        if *response.code() == ResponseCode::Ok {
            // The EndOfBody Header contains the user data.
            return HeaderSet::from(response).remove_body(true).map(|eob| (true, eob));
        }

        // Otherwise, a Continue means there are more body packets left.
        let mut headers =
            response.expect_code(OpCode::GetFinal, ResponseCode::Continue).map(HeaderSet::from)?;
        // The Body Header contains the user data.
        headers.remove_body(false).map(|b| (false, b))
    }

    /// Makes a GET request with the final bit unset using the provided `headers`.
    /// Returns the headers included in the peer response.
    async fn do_get(&mut self, headers: HeaderSet) -> Result<HeaderSet, Error> {
        let request = RequestPacket::new_get(headers);
        trace!(?request, "Making outgoing GET request");
        self.transport.send(request)?;
        trace!("Successfully made GET request");
        let response = self.transport.receive_response(OpCode::Get).await?;
        let response_headers = Self::handle_get_response(response)?;
        Ok(response_headers)
    }

    /// Starts the operation by making a GET request to the remote peer with the headers provided
    /// in `GetOperation::new`.
    /// Returns the informational headers from the peer response on success, Error otherwise.
    pub async fn start(&mut self) -> Result<HeaderSet, Error> {
        if self.is_started() {
            return Err(Error::operation(OpCode::Get, "already started"));
        }
        let headers = self.headers.take().unwrap();
        self.do_get(headers).await
    }

    /// Request additional information about the payload.
    /// Returns the informational headers from the peer response on success, Error otherwise.
    ///
    /// Only one such request can be outstanding at a time.
    pub async fn get_information(&mut self, headers: HeaderSet) -> Result<HeaderSet, Error> {
        if !self.is_started() {
            return Err(Error::operation(OpCode::Get, "not started"));
        }
        // The client must provide headers.
        // TODO(fxbug.dev/125503): The Name or Type header must be provided at some point during the
        // operation. Track it across potentially multiple `get_information` requests.
        if headers.is_empty() {
            return Err(Error::operation(OpCode::Get, "missing headers"));
        }

        self.do_get(headers).await
    }

    /// Request the user data payload from the remote OBEX server.
    /// Returns the byte payload on success, Error otherwise.
    ///
    /// The GET operation is considered complete after this.
    pub async fn get_data(mut self) -> Result<Vec<u8>, Error> {
        if !self.is_started() {
            return Err(Error::operation(OpCode::Get, "not started"));
        }

        let request = RequestPacket::new_get_final();
        let mut body = vec![];
        loop {
            trace!(?request, "Making outgoing GET final request");
            self.transport.send(request.clone())?;
            trace!("Successfully made GET final request");
            let response = self.transport.receive_response(OpCode::GetFinal).await?;
            let (final_packet, mut response_body) = Self::handle_get_final_response(response)?;
            body.append(&mut response_body);
            if final_packet {
                trace!("Found terminal GET final packet");
                break;
            }
        }
        Ok(body)
    }

    /// Request to terminate a multi-packet GET request early.
    /// Returns the informational headers from the peer response on success, Error otherwise.
    /// If Error is returned, there are no guarantees about the synchronization between the local
    /// OBEX client and remote OBEX server.
    pub async fn terminate(mut self, headers: HeaderSet) -> Result<HeaderSet, Error> {
        let opcode = OpCode::Abort;
        if !self.is_started() {
            return Err(Error::operation(opcode, "can't abort GET that hasn't started"));
        }

        let request = RequestPacket::new_abort(headers);
        trace!(?request, "Making outgoing {opcode:?} request");
        self.transport.send(request)?;
        trace!("Successfully made {opcode:?} request");
        let response = self.transport.receive_response(opcode).await?;
        response.expect_code(opcode, ResponseCode::Ok).map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use async_utils::PollExt;
    use fuchsia_async as fasync;
    use fuchsia_bluetooth::types::Channel;
    use futures::pin_mut;

    use crate::error::PacketError;
    use crate::header::{Header, HeaderIdentifier};
    use crate::transport::test_utils::{expect_code, expect_request_and_reply, new_manager};
    use crate::transport::ObexTransportManager;

    #[track_caller]
    fn do_start(
        exec: &mut fasync::TestExecutor,
        operation: &mut GetOperation<'_>,
        remote: &mut Channel,
        response_headers: HeaderSet,
    ) -> HeaderSet {
        let response_headers = {
            let start_fut = operation.start();
            pin_mut!(start_fut);
            exec.run_until_stalled(&mut start_fut).expect_pending("waiting for peer response");
            let response = ResponsePacket::new_no_data(ResponseCode::Continue, response_headers);
            expect_request_and_reply(exec, remote, expect_code(OpCode::Get), response);
            exec.run_until_stalled(&mut start_fut)
                .expect("response received")
                .expect("valid response")
        };
        response_headers
    }

    fn setup_get_operation(mgr: &ObexTransportManager, initial: HeaderSet) -> GetOperation<'_> {
        let transport = mgr.try_new_operation().expect("can start operation");
        GetOperation::new(initial, transport)
    }

    #[fuchsia::test]
    fn get_operation() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
        let initial = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let mut operation = setup_get_operation(&manager, initial);

        // The initial GET request should succeed and return the set of Headers specified by the
        // peer.
        {
            let response_headers = HeaderSet::from_header(Header::Name("bar".into())).unwrap();
            let received_headers =
                do_start(&mut exec, &mut operation, &mut remote, response_headers);
            assert!(received_headers.contains_header(&HeaderIdentifier::Name));
        }

        // A subsequent request for more information should return the set of additional Headers
        // specified by the peer.
        {
            let info_headers = HeaderSet::from_header(Header::Type("file".into())).unwrap();
            let info_fut = operation.get_information(info_headers);
            pin_mut!(info_fut);
            exec.run_until_stalled(&mut info_fut).expect_pending("waiting for peer response");
            let response_headers =
                HeaderSet::from_header(Header::Description("big file".into())).unwrap();
            let response = ResponsePacket::new_no_data(ResponseCode::Continue, response_headers);
            expect_request_and_reply(&mut exec, &mut remote, expect_code(OpCode::Get), response);
            let received_headers = exec
                .run_until_stalled(&mut info_fut)
                .expect("response received")
                .expect("valid response");
            assert!(received_headers.contains_header(&HeaderIdentifier::Description));
        }

        // The multi-packet user payload should be returned at the end of the operation. Because
        // the operation is taken by value, it is considered complete after this step and resources
        // are freed.
        let data_fut = operation.get_data();
        pin_mut!(data_fut);
        exec.run_until_stalled(&mut data_fut).expect_pending("waiting for peer response");
        let response_headers1 = HeaderSet::from_header(Header::Body(vec![1, 2, 3])).unwrap();
        let response1 = ResponsePacket::new_no_data(ResponseCode::Continue, response_headers1);
        expect_request_and_reply(&mut exec, &mut remote, expect_code(OpCode::GetFinal), response1);
        exec.run_until_stalled(&mut data_fut)
            .expect_pending("waiting for additional peer responses");
        // The OK response code indicates the last user data packet.
        let response_headers2 = HeaderSet::from_header(Header::EndOfBody(vec![4, 5, 6])).unwrap();
        let response2 = ResponsePacket::new_no_data(ResponseCode::Ok, response_headers2);
        expect_request_and_reply(&mut exec, &mut remote, expect_code(OpCode::GetFinal), response2);
        // All user data packets are received and the concatenated data object is returned.
        let user_data = exec
            .run_until_stalled(&mut data_fut)
            .expect("received all responses")
            .expect("valid user data");
        assert_eq!(user_data, vec![1, 2, 3, 4, 5, 6]);
    }

    #[fuchsia::test]
    fn get_operation_terminate_success() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
        let initial = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let mut operation = setup_get_operation(&manager, initial);

        // Start the GET operation.
        let response_headers = HeaderSet::from_header(Header::Name("bar".into())).unwrap();
        let _received_headers = do_start(&mut exec, &mut operation, &mut remote, response_headers);

        // Terminating early is OK. It should consume the operation and be considered complete.
        let headers = HeaderSet::from_header(Header::Name("terminated".into())).unwrap();
        let terminate_fut = operation.terminate(headers);
        pin_mut!(terminate_fut);
        let _ =
            exec.run_until_stalled(&mut terminate_fut).expect_pending("waiting for peer response");
        let response = ResponsePacket::new_no_data(ResponseCode::Ok, HeaderSet::new());
        expect_request_and_reply(&mut exec, &mut remote, expect_code(OpCode::Abort), response);
    }

    #[fuchsia::test]
    fn get_operation_multiple_start_is_error() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
        let initial = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let mut operation = setup_get_operation(&manager, initial);

        // First time start is OK.
        let response_headers = HeaderSet::from_header(Header::Name("bar".into())).unwrap();
        let _received_headers = do_start(&mut exec, &mut operation, &mut remote, response_headers);
        // Trying to start again is an Error.
        {
            let start_fut1 = operation.start();
            pin_mut!(start_fut1);
            let start_result =
                exec.run_until_stalled(&mut start_fut1).expect("resolved with error");
            assert_matches!(start_result, Err(Error::OperationError { .. }));
        }
    }

    #[fuchsia::test]
    fn get_operation_information_error() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
        let initial = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let mut operation = setup_get_operation(&manager, initial);

        // Trying to get additional information before it is started is an Error.
        {
            let headers = HeaderSet::from_header(Header::Name("foobar".into())).unwrap();
            let get_info_fut = operation.get_information(headers);
            pin_mut!(get_info_fut);
            let get_info_result =
                exec.run_until_stalled(&mut get_info_fut).expect("resolves with error");
            assert_matches!(get_info_result, Err(Error::OperationError { .. }));
        }

        // Set started.
        let response_headers = HeaderSet::from_header(Header::Name("bar".into())).unwrap();
        let _received_headers = do_start(&mut exec, &mut operation, &mut remote, response_headers);

        // Trying to get additional information without providing any headers is an Error.
        let get_info_fut = operation.get_information(HeaderSet::new());
        pin_mut!(get_info_fut);
        let get_info_result =
            exec.run_until_stalled(&mut get_info_fut).expect("resolves with error");
        assert_matches!(get_info_result, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    fn get_operation_data_before_start_is_error() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, _remote) = new_manager(/* srm_supported */ false);
        let initial = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let operation = setup_get_operation(&manager, initial);

        // Trying to get the user data before it is started is an Error.
        let get_data_fut = operation.get_data();
        pin_mut!(get_data_fut);
        let get_data_result =
            exec.run_until_stalled(&mut get_data_fut).expect("resolves with error");
        assert_matches!(get_data_result, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    fn get_operation_data_peer_disconnect_is_error() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
        let initial = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let mut operation = setup_get_operation(&manager, initial);

        // Set started.
        let response_headers = HeaderSet::from_header(Header::Name("bar".into())).unwrap();
        let _received_headers = do_start(&mut exec, &mut operation, &mut remote, response_headers);

        // Before the client can get the user data, the peer disconnects.
        drop(remote);
        let get_data_fut = operation.get_data();
        pin_mut!(get_data_fut);
        let get_data_result =
            exec.run_until_stalled(&mut get_data_fut).expect("resolves with error");
        assert_matches!(get_data_result, Err(Error::IOError(_)));
    }

    #[fuchsia::test]
    async fn get_operation_terminate_before_start_error() {
        let (manager, _remote) = new_manager(/* srm_supported */ false);
        let initial = HeaderSet::from_header(Header::Name("bar".into())).unwrap();
        let operation = setup_get_operation(&manager, initial);

        // The GET operation is not in progress yet so trying to terminate will fail.
        let terminate_result = operation.terminate(HeaderSet::new()).await;
        assert_matches!(terminate_result, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    fn handle_get_response_success() {
        let headers = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let response = ResponsePacket::new_no_data(ResponseCode::Continue, headers.clone());
        let result = GetOperation::handle_get_response(response).expect("valid response");
        assert_eq!(result, headers);
    }

    #[fuchsia::test]
    fn handle_get_response_error() {
        let headers = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        // Expect the Continue, not Ok.
        let response1 = ResponsePacket::new_no_data(ResponseCode::Ok, headers.clone());
        assert_matches!(
            GetOperation::handle_get_response(response1),
            Err(Error::PeerRejected { .. })
        );

        // Expect the Continue, not anything else.
        let response1 = ResponsePacket::new_no_data(ResponseCode::NotFound, headers);
        assert_matches!(
            GetOperation::handle_get_response(response1),
            Err(Error::PeerRejected { .. })
        );
    }

    #[fuchsia::test]
    fn handle_get_final_response_success() {
        let headers = HeaderSet::from_header(Header::EndOfBody(vec![1, 2])).unwrap();
        let response1 = ResponsePacket::new_no_data(ResponseCode::Ok, headers);
        let result1 = GetOperation::handle_get_final_response(response1).expect("valid response");
        assert_eq!(result1, (true, vec![1, 2]));

        let headers = HeaderSet::from_header(Header::Body(vec![1, 3, 5])).unwrap();
        let response2 = ResponsePacket::new_no_data(ResponseCode::Continue, headers);
        let result2 = GetOperation::handle_get_final_response(response2).expect("valid response");
        assert_eq!(result2, (false, vec![1, 3, 5]));
    }

    #[fuchsia::test]
    fn get_final_response_error() {
        // A non-success error code is an Error.
        let headers = HeaderSet::from_header(Header::EndOfBody(vec![1, 2])).unwrap();
        let response1 = ResponsePacket::new_no_data(ResponseCode::Forbidden, headers);
        assert_matches!(
            GetOperation::handle_get_final_response(response1),
            Err(Error::PeerRejected { .. })
        );

        // A final packet with no EndOfBody payload is an Error.
        let response2 = ResponsePacket::new_no_data(ResponseCode::Ok, HeaderSet::new());
        assert_matches!(
            GetOperation::handle_get_final_response(response2),
            Err(Error::Packet(PacketError::Data(_)))
        );

        // A non-final packet with no Body payload is an Error.
        let response3 = ResponsePacket::new_no_data(ResponseCode::Continue, HeaderSet::new());
        assert_matches!(
            GetOperation::handle_get_final_response(response3),
            Err(Error::Packet(PacketError::Data(_)))
        );
    }
}
