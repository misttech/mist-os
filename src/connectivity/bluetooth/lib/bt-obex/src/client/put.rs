// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use tracing::trace;

use crate::client::SrmOperation;
use crate::error::Error;
use crate::header::{Header, HeaderIdentifier, HeaderSet, SingleResponseMode};
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
    /// The status of SRM for this operation. By default, SRM will be enabled if the transport
    /// supports it. However, it may be disabled if the peer requests to disable it.
    srm: SingleResponseMode,
}

impl<'a> PutOperation<'a> {
    pub fn new(transport: ObexTransport<'a>) -> Self {
        let srm = transport.srm_supported().into();
        Self { transport, is_started: false, srm }
    }

    #[cfg(test)]
    fn new_started(transport: ObexTransport<'a>) -> Self {
        let mut this = Self::new(transport);
        this.is_started = true;
        this
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
        // SRM is considered active if this is a subsequent PUT request & the transport supports it.
        let srm_active = self.is_started && self.srm_mode() == SingleResponseMode::Enable;
        let (opcode, request, expected_response_code) = if final_ {
            (OpCode::PutFinal, RequestPacket::new_put_final(headers), ResponseCode::Ok)
        } else {
            (OpCode::Put, RequestPacket::new_put(headers), ResponseCode::Continue)
        };
        trace!("Making outgoing PUT request: {request:?}");
        self.transport.send(request)?;
        trace!("Successfully made PUT request");
        // Expect a response if this is the final PUT request or if SRM is inactive, in which case
        // every request must be responded to.
        if final_ || !srm_active {
            let response = self.transport.receive_response(opcode).await?;
            response.expect_code(opcode, expected_response_code).map(Into::into)
        } else {
            Ok(HeaderSet::new())
        }
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
    /// The returned informational headers will be empty if Single Response Mode is enabled for the
    /// operation. Only the final write request (`Self::write_final`) will potentially return a
    /// non-empty set of headers.
    pub async fn write(&mut self, data: &[u8], mut headers: HeaderSet) -> Result<HeaderSet, Error> {
        Self::validate_headers(&headers)?;
        // Try to enable SRM if this is the first packet of the operation.
        if !self.is_started {
            self.try_enable_srm(&mut headers)?;
        }
        headers.add(Header::Body(data.to_vec()))?;
        let response_headers = self.do_put(false, headers).await?;
        if !self.is_started {
            self.check_response_for_srm(&response_headers);
        }
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

impl SrmOperation for PutOperation<'_> {
    const OPERATION_TYPE: OpCode = OpCode::Put;

    fn srm_mode(&self) -> SingleResponseMode {
        self.srm
    }

    fn set_srm(&mut self, mode: SingleResponseMode) {
        self.srm = mode;
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
    use crate::transport::test_utils::{
        expect_code, expect_request, expect_request_and_reply, new_manager,
    };
    use crate::transport::ObexTransportManager;

    fn setup_put_operation(mgr: &ObexTransportManager) -> PutOperation<'_> {
        let transport = mgr.try_new_operation().expect("can start operation");
        PutOperation::new(transport)
    }

    #[fuchsia::test]
    fn put_operation_single_chunk_is_ok() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
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
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
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
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
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
        let (manager, mut remote) = new_manager(/* srm_supported */ false);
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
        let (manager, _remote) = new_manager(/* srm_supported */ false);
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
        let (manager, _remote) = new_manager(/* srm_supported */ false);

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
        let (manager, _remote) = new_manager(/* srm_supported */ false);
        let operation = setup_put_operation(&manager);

        // Trying to terminate early doesn't work as the operation has not started.
        let headers =
            HeaderSet::from_header(Header::Description("terminating test".into())).unwrap();
        let terminate_result = operation.terminate(headers).await;
        assert_matches!(terminate_result, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    fn put_operation_srm_enabled_is_ok() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ true);
        let mut operation = setup_put_operation(&manager);

        {
            let first_buf = [1, 2, 3];
            // Even though the input headers are empty, we should prefer to enable SRM.
            let put_fut = operation.write(&first_buf[..], HeaderSet::new());
            pin_mut!(put_fut);
            let _ = exec.run_until_stalled(&mut put_fut).expect_pending("waiting for response");

            // Expect the outgoing request with the SRM Header. Peer responds positively with a SRM
            // Enable response.
            let expectation = |request: RequestPacket| {
                assert_eq!(*request.code(), OpCode::Put);
                let headers = HeaderSet::from(request);
                assert!(headers.contains_header(&HeaderIdentifier::Body));
                assert!(headers.contains_header(&HeaderIdentifier::SingleResponseMode));
            };
            let response_headers =
                HeaderSet::from_header(SingleResponseMode::Enable.into()).unwrap();
            let response = ResponsePacket::new_no_data(ResponseCode::Continue, response_headers);
            expect_request_and_reply(&mut exec, &mut remote, expectation, response);
            let _received_headers = exec
                .run_until_stalled(&mut put_fut)
                .expect("response received")
                .expect("valid response");
        }
        // At this point SRM is enabled for the duration of the operation.
        assert_eq!(operation.srm, SingleResponseMode::Enable);
        // Second write doesn't require a response.
        {
            let second_buf = [4, 5, 6];
            let put_fut2 = operation.write(&second_buf[..], HeaderSet::new());
            pin_mut!(put_fut2);
            let _ = exec
                .run_until_stalled(&mut put_fut2)
                .expect("ready without peer response")
                .expect("success");
            let expectation = |request: RequestPacket| {
                assert_eq!(*request.code(), OpCode::Put);
                let headers = HeaderSet::from(request);
                assert!(headers.contains_header(&HeaderIdentifier::Body));
                assert!(!headers.contains_header(&HeaderIdentifier::SingleResponseMode));
            };
            expect_request(&mut exec, &mut remote, expectation);
        }

        // Only the final write request will result in a response.
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
    fn client_disable_srm_mid_operation_is_ignored() {
        let mut exec = fasync::TestExecutor::new();
        let (manager, mut remote) = new_manager(/* srm_supported */ true);
        let transport = manager.try_new_operation().expect("can start operation");
        let mut operation = PutOperation::new_started(transport);

        // SRM is enabled for the duration of the operation.
        assert_eq!(operation.srm, SingleResponseMode::Enable);

        // Client tries to disable SRM in a subsequent write attempt. No-op.
        {
            let headers = HeaderSet::from_header(SingleResponseMode::Disable.into()).unwrap();
            let put_fut = operation.write(&[], headers);
            pin_mut!(put_fut);
            let _ = exec
                .run_until_stalled(&mut put_fut)
                .expect("ready without peer response")
                .expect("success");
            let expectation = |request: RequestPacket| {
                assert_eq!(*request.code(), OpCode::Put);
            };
            expect_request(&mut exec, &mut remote, expectation);
        }
        // SRM is still enabled.
        assert_eq!(operation.srm, SingleResponseMode::Enable);
    }

    #[fuchsia::test]
    fn application_select_srm_success() {
        let _exec = fasync::TestExecutor::new();
        let (manager, _remote) = new_manager(/* srm_supported */ false);
        let mut operation = setup_put_operation(&manager);
        assert_eq!(operation.srm, SingleResponseMode::Disable);
        // The application requesting to disable SRM when it isn't supported is OK.
        let mut headers = HeaderSet::from_header(SingleResponseMode::Disable.into()).unwrap();
        assert_matches!(operation.try_enable_srm(&mut headers), Ok(()));
        assert_eq!(operation.srm, SingleResponseMode::Disable);

        // The application requesting to disable SRM when it is supported is OK.
        let (manager, _remote) = new_manager(/* srm_supported */ true);
        let mut operation = setup_put_operation(&manager);
        assert_eq!(operation.srm, SingleResponseMode::Enable);
        let mut headers = HeaderSet::from_header(SingleResponseMode::Disable.into()).unwrap();
        assert_matches!(operation.try_enable_srm(&mut headers), Ok(()));
        assert_eq!(operation.srm, SingleResponseMode::Disable);

        // The application requesting to enable SRM when it is supported is OK.
        let (manager, _remote) = new_manager(/* srm_supported */ true);
        let mut operation = setup_put_operation(&manager);
        assert_eq!(operation.srm, SingleResponseMode::Enable);
        let mut headers = HeaderSet::from_header(SingleResponseMode::Enable.into()).unwrap();
        assert_matches!(operation.try_enable_srm(&mut headers), Ok(()));
        assert_eq!(operation.srm, SingleResponseMode::Enable);
    }

    #[fuchsia::test]
    fn application_enable_srm_when_not_supported_is_error() {
        let _exec = fasync::TestExecutor::new();
        let (manager, _remote) = new_manager(/* srm_supported */ false);
        let mut operation = setup_put_operation(&manager);
        assert_eq!(operation.srm, SingleResponseMode::Disable);
        let mut headers = HeaderSet::from_header(SingleResponseMode::Enable.into()).unwrap();
        assert_matches!(operation.try_enable_srm(&mut headers), Err(Error::OperationError { .. }));
        assert_eq!(operation.srm, SingleResponseMode::Disable);
    }

    #[fuchsia::test]
    fn peer_srm_response() {
        let _exec = fasync::TestExecutor::new();
        let (manager, _remote) = new_manager(/* srm_supported */ false);
        let mut operation = setup_put_operation(&manager);
        // An enable response from the peer when SRM is disabled locally should not enable SRM.
        let headers = HeaderSet::from_header(SingleResponseMode::Enable.into()).unwrap();
        operation.check_response_for_srm(&headers);
        assert_eq!(operation.srm, SingleResponseMode::Disable);
        // A disable response from the peer when SRM is disabled locally is a no-op.
        let headers = HeaderSet::from_header(SingleResponseMode::Disable.into()).unwrap();
        operation.check_response_for_srm(&headers);
        assert_eq!(operation.srm, SingleResponseMode::Disable);

        let (manager, _remote) = new_manager(/* srm_supported */ true);
        let mut operation = setup_put_operation(&manager);
        // An enable response from the peer when SRM is enable is a no-op.
        let headers = HeaderSet::from_header(SingleResponseMode::Enable.into()).unwrap();
        operation.check_response_for_srm(&headers);
        assert_eq!(operation.srm, SingleResponseMode::Enable);
        // A disable response from the peer when SRM is enabled should disable SRM.
        let headers = HeaderSet::from_header(SingleResponseMode::Disable.into()).unwrap();
        operation.check_response_for_srm(&headers);
        assert_eq!(operation.srm, SingleResponseMode::Disable);

        let (manager, _remote) = new_manager(/* srm_supported */ true);
        let mut operation = setup_put_operation(&manager);
        // A response with no SRM header should be treated like a disable request.
        operation.check_response_for_srm(&HeaderSet::new());
        assert_eq!(operation.srm, SingleResponseMode::Disable);
    }
}
