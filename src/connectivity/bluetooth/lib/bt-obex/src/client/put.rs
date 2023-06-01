// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use tracing::trace;

use crate::error::Error;
use crate::header::{Header, HeaderIdentifier, HeaderSet};
use crate::operation::{OpCode, RequestPacket, ResponseCode};
use crate::transport::ObexTransport;

/// Represents an in-progress PUT Operation.
/// Defined in OBEX 1.5 Section 3.4.3.
///
/// Example Usage:
/// ```
/// let obex_client = ObexClient::new(..);
/// let put_operation = obex_client.put()?;
/// let user_data: Vec<u8> = vec![];
/// for user_data_chunk in user_data.chunks(50) {
///   let received_headers = put_operation.write(&user_data_chunk[..], HeaderSet::new()).await?;
/// }
/// // `PutOperation::write_final` must be called before it is dropped. An empty payload is OK.
/// let final_headers = put_operation.write_final(&[], HeaderSet::new()).await?;
/// // PUT operation is complete and `put_operation` is consumed.
/// ```
#[must_use]
#[derive(Debug)]
pub struct PutOperation<'a> {
    /// The L2CAP or RFCOMM connection to the remote peer.
    transport: ObexTransport<'a>,
    /// Whether the operation is in-progress or not - one or more PUT requests have been made.
    is_started: bool,
}

impl<'a> PutOperation<'a> {
    pub fn new(transport: ObexTransport<'a>) -> Self {
        Self { transport, is_started: false }
    }

    /// Returns Error if the `headers` contain non-informational OBEX Headers.
    fn validate_headers(headers: &HeaderSet) -> Result<(), Error> {
        if headers.contains_header(&HeaderIdentifier::Body) {
            return Err(Error::operation(OpCode::Put, "info headers can't contain body"));
        }
        if headers.contains_header(&HeaderIdentifier::EndOfBody) {
            return Err(Error::operation(OpCode::Put, "info headers can't contain end of body"));
        }
        Ok(())
    }

    /// Attempts to initiate a PUT operation with the `final_` bit set.
    /// Returns the peer response headers on success, Error otherwise.
    async fn do_put(&mut self, final_: bool, headers: HeaderSet) -> Result<HeaderSet, Error> {
        let (opcode, request, expected_response_code) = if final_ {
            (OpCode::PutFinal, RequestPacket::new_put_final(headers), ResponseCode::Ok)
        } else {
            (OpCode::Put, RequestPacket::new_put(headers), ResponseCode::Continue)
        };
        trace!("Making outgoing PUT request: {request:?}");
        self.transport.send(request)?;
        trace!("Successfully made PUT request");
        let response = self.transport.receive_response(opcode).await?;
        response.expect_code(opcode, expected_response_code).map(Into::into)
    }

    /// Attempts to delete an object from the remote OBEX server specified by the provided
    /// `headers`.
    /// Returns the informational headers from the peer response on success, Error otherwise.
    pub async fn delete(mut self, headers: HeaderSet) -> Result<HeaderSet, Error> {
        Self::validate_headers(&headers)?;
        // No Body or EndOfBody Headers are included in a delete request.
        // See OBEX 1.5 Section 3.4.3.6.
        self.do_put(true, headers).await
    }

    /// Attempts to write the `data` object to the remote OBEX server.
    /// Returns the informational headers from the peer response on success, Error otherwise.
    pub async fn write(&mut self, data: &[u8], mut headers: HeaderSet) -> Result<HeaderSet, Error> {
        Self::validate_headers(&headers)?;
        headers.add(Header::Body(data.to_vec()))?;
        let response_headers = self.do_put(false, headers).await?;
        self.is_started = true;
        Ok(response_headers)
    }

    /// Attempts to write the final `data` object to the remote OBEX server.
    /// This must be called before the PutOperation object is dropped.
    /// Returns the informational headers from the peer response on success, Error otherwise.
    ///
    /// The PUT operation is considered complete after this.
    pub async fn write_final(
        mut self,
        data: &[u8],
        mut headers: HeaderSet,
    ) -> Result<HeaderSet, Error> {
        Self::validate_headers(&headers)?;
        headers.add(Header::EndOfBody(data.to_vec()))?;
        self.do_put(true, headers).await
    }

    /// Request to terminate a multi-packet PUT request early.
    /// Returns the informational headers from the peer response on success, Error otherwise.
    /// If Error is returned, there are no guarantees about the synchronization between the local
    /// OBEX client and remote OBEX server.
    pub async fn terminate(mut self, headers: HeaderSet) -> Result<HeaderSet, Error> {
        let opcode = OpCode::Abort;
        if !self.is_started {
            return Err(Error::operation(opcode, "can't abort PUT that hasn't started"));
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
    use futures::pin_mut;

    use crate::header::{Header, HeaderIdentifier};
    use crate::operation::ResponsePacket;
    use crate::transport::test_utils::{expect_code, expect_request_and_reply, new_manager};
    use crate::transport::ObexTransportManager;

    fn setup_put_operation(mgr: &ObexTransportManager) -> PutOperation<'_> {
        let transport = mgr.try_new_operation().expect("can start operation");
        PutOperation::new(transport)
    }

    #[fuchsia::test]
    fn put_operation_single_chunk_is_ok() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager();
        let operation = setup_put_operation(&manager);

        let payload = vec![5, 6, 7, 8, 9];
        let headers = HeaderSet::from_headers(vec![
            Header::Type("file".into()),
            Header::Name("foobar.txt".into()),
        ])
        .unwrap();
        let put_fut = operation.write_final(&payload[..], headers);
        pin_mut!(put_fut);
        let _ = exec.run_until_stalled(&mut put_fut).expect_pending("waiting for response");
        let response = ResponsePacket::new_no_data(ResponseCode::Ok, HeaderSet::new());
        let expectation = |request: RequestPacket| {
            assert_eq!(*request.code(), OpCode::PutFinal);
            let headers = HeaderSet::from(request);
            assert!(!headers.contains_header(&HeaderIdentifier::Body));
            assert!(headers.contains_headers(&vec![
                HeaderIdentifier::EndOfBody,
                HeaderIdentifier::Type,
                HeaderIdentifier::Name
            ]));
        };
        expect_request_and_reply(&mut exec, &mut remote, expectation, response);
        let _received_headers = exec
            .run_until_stalled(&mut put_fut)
            .expect("response received")
            .expect("valid response");
    }

    #[fuchsia::test]
    fn put_operation_multiple_chunks_is_ok() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager();
        let mut operation = setup_put_operation(&manager);

        let payload: Vec<u8> = (1..100).collect();
        for chunk in payload.chunks(20) {
            let put_fut = operation.write(&chunk[..], HeaderSet::new());
            pin_mut!(put_fut);
            let _ = exec.run_until_stalled(&mut put_fut).expect_pending("waiting for response");
            let response = ResponsePacket::new_no_data(ResponseCode::Continue, HeaderSet::new());
            let expectation = |request: RequestPacket| {
                assert_eq!(*request.code(), OpCode::Put);
                let headers = HeaderSet::from(request);
                assert!(headers.contains_header(&HeaderIdentifier::Body));
            };
            expect_request_and_reply(&mut exec, &mut remote, expectation, response);
            let _received_headers = exec
                .run_until_stalled(&mut put_fut)
                .expect("response received")
                .expect("valid response");
        }

        // Can send final response that is empty to complete the operation.
        let put_final_fut = operation.write_final(&[], HeaderSet::new());
        pin_mut!(put_final_fut);
        let _ = exec.run_until_stalled(&mut put_final_fut).expect_pending("waiting for response");
        let response = ResponsePacket::new_no_data(ResponseCode::Ok, HeaderSet::new());
        let expectation = |request: RequestPacket| {
            assert_eq!(*request.code(), OpCode::PutFinal);
            let headers = HeaderSet::from(request);
            assert!(headers.contains_header(&HeaderIdentifier::EndOfBody));
        };
        expect_request_and_reply(&mut exec, &mut remote, expectation, response);
        let _ = exec
            .run_until_stalled(&mut put_final_fut)
            .expect("response received")
            .expect("valid response");
    }

    #[fuchsia::test]
    fn put_operation_delete_is_ok() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager();
        let operation = setup_put_operation(&manager);

        let headers = HeaderSet::from_headers(vec![
            Header::Description("deleting file".into()),
            Header::Name("foobar.txt".into()),
        ])
        .unwrap();
        let put_fut = operation.delete(headers);
        pin_mut!(put_fut);
        let _ = exec.run_until_stalled(&mut put_fut).expect_pending("waiting for response");
        let response = ResponsePacket::new_no_data(ResponseCode::Ok, HeaderSet::new());
        let expectation = |request: RequestPacket| {
            assert_eq!(*request.code(), OpCode::PutFinal);
            let headers = HeaderSet::from(request);
            assert!(!headers.contains_header(&HeaderIdentifier::Body));
            assert!(!headers.contains_header(&HeaderIdentifier::EndOfBody));
        };
        expect_request_and_reply(&mut exec, &mut remote, expectation, response);
        let _ = exec
            .run_until_stalled(&mut put_fut)
            .expect("response received")
            .expect("valid response");
    }

    #[fuchsia::test]
    fn put_operation_terminate_success() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager();
        let mut operation = setup_put_operation(&manager);

        // Write the first chunk of data to "start" the operation.
        {
            let put_fut = operation.write(&[1, 2, 3, 4, 5], HeaderSet::new());
            pin_mut!(put_fut);
            let _ = exec.run_until_stalled(&mut put_fut).expect_pending("waiting for response");
            let response = ResponsePacket::new_no_data(ResponseCode::Continue, HeaderSet::new());
            expect_request_and_reply(&mut exec, &mut remote, expect_code(OpCode::Put), response);
            let _received_headers = exec
                .run_until_stalled(&mut put_fut)
                .expect("response received")
                .expect("valid response");
        }

        // Terminating early should be Ok - peer acknowledges.
        let terminate_fut = operation.terminate(HeaderSet::new());
        pin_mut!(terminate_fut);
        let _ = exec.run_until_stalled(&mut terminate_fut).expect_pending("waiting for response");
        let response = ResponsePacket::new_no_data(ResponseCode::Ok, HeaderSet::new());
        expect_request_and_reply(&mut exec, &mut remote, expect_code(OpCode::Abort), response);
        let _received_headers = exec
            .run_until_stalled(&mut terminate_fut)
            .expect("response received")
            .expect("valid response");
    }

    #[fuchsia::test]
    async fn put_with_body_header_is_error() {
        let (manager, _remote) = new_manager();
        let mut operation = setup_put_operation(&manager);

        let payload = vec![1, 2, 3];
        // The payload should only be included as an argument. All other headers must be
        // informational.
        let body_headers = HeaderSet::from_headers(vec![
            Header::Body(payload.clone()),
            Header::Name("foobar.txt".into()),
        ])
        .unwrap();
        let result = operation.write(&payload[..], body_headers.clone()).await;
        assert_matches!(result, Err(Error::OperationError { .. }));

        // EndOfBody header is also an Error.
        let eob_headers = HeaderSet::from_headers(vec![
            Header::EndOfBody(payload.clone()),
            Header::Name("foobar1.txt".into()),
        ])
        .unwrap();
        let result = operation.write(&payload[..], eob_headers.clone()).await;
        assert_matches!(result, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    async fn delete_with_body_header_is_error() {
        let (manager, _remote) = new_manager();

        let payload = vec![1, 2, 3];
        // Body shouldn't be included in delete.
        let operation = setup_put_operation(&manager);
        let body_headers = HeaderSet::from_headers(vec![
            Header::Body(payload.clone()),
            Header::Name("foobar.txt".into()),
        ])
        .unwrap();
        let result = operation.delete(body_headers).await;
        assert_matches!(result, Err(Error::OperationError { .. }));

        // EndOfBody shouldn't be included in delete.
        let operation = setup_put_operation(&manager);
        let eob_headers = HeaderSet::from_headers(vec![
            Header::EndOfBody(payload.clone()),
            Header::Name("foobar1.txt".into()),
        ])
        .unwrap();
        let result = operation.delete(eob_headers).await;
        assert_matches!(result, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    async fn put_operation_terminate_before_start_error() {
        let (manager, _remote) = new_manager();
        let operation = setup_put_operation(&manager);

        // Trying to terminate early doesn't work as the operation has not started.
        let headers =
            HeaderSet::from_header(Header::Description("terminating test".into())).unwrap();
        let terminate_result = operation.terminate(headers).await;
        assert_matches!(terminate_result, Err(Error::OperationError { .. }));
    }
}
