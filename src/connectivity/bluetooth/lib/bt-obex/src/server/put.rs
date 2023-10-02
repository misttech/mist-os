// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use tracing::trace;

use crate::error::Error;
use crate::header::HeaderSet;
use crate::operation::{OpCode, RequestPacket, ResponseCode, ResponsePacket};
use crate::server::handler::ObexOperationError;
use crate::server::{ApplicationResponse, OperationRequest, ServerOperation};

/// The current state of the PUT operation.
#[derive(Debug)]
enum State {
    /// Receiving informational headers and data packets.
    Request { headers: HeaderSet, staged_data: Vec<u8> },
    /// The final request packet has been received.
    RequestPhaseComplete,
    /// The operation is complete.
    Complete,
}

/// An in-progress PUT operation.
pub struct PutOperation {
    state: State,
}

impl PutOperation {
    pub fn new() -> Self {
        Self { state: State::Request { headers: HeaderSet::new(), staged_data: Vec::new() } }
    }

    #[cfg(test)]
    fn new_at_state(state: State) -> Self {
        Self { state }
    }
}

impl ServerOperation for PutOperation {
    fn is_complete(&self) -> bool {
        matches!(self.state, State::Complete)
    }

    fn handle_peer_request(&mut self, request: RequestPacket) -> Result<OperationRequest, Error> {
        let code = *request.code();
        let mut request_headers = HeaderSet::from(request);
        match &mut self.state {
            State::Request { ref mut headers, ref mut staged_data } if code == OpCode::Put => {
                // A non-final PUT request may contain a Body header specifying user data (among
                // other informational headers).
                if let Ok(mut data) = request_headers.remove_body(/*final= */ false) {
                    staged_data.append(&mut data);
                }
                headers.try_append(request_headers)?;
                let response =
                    ResponsePacket::new_no_data(ResponseCode::Continue, HeaderSet::new());
                Ok(OperationRequest::SendPacket(response))
            }
            State::Request { ref mut headers, ref mut staged_data } if code == OpCode::PutFinal => {
                // A final PUT request may contain an EndOfBody header specifying user data (among
                // other informational headers).
                if let Ok(mut data) = request_headers.remove_body(/*final= */ true) {
                    staged_data.append(&mut data);
                }
                headers.try_append(request_headers)?;
                let request_headers = std::mem::replace(headers, HeaderSet::new());
                let request_data = std::mem::take(staged_data);
                self.state = State::RequestPhaseComplete;
                Ok(OperationRequest::PutApplicationData(request_data, request_headers))
            }
            _ => Err(Error::operation(OpCode::Put, "received invalid request")),
        }
    }

    fn handle_application_response(
        &mut self,
        response: Result<ApplicationResponse, ObexOperationError>,
    ) -> Result<ResponsePacket, Error> {
        // Only expect a response when all request packets have been received.
        if !matches!(self.state, State::RequestPhaseComplete) {
            return Err(Error::operation(OpCode::Put, "invalid state"));
        }

        let response = match response {
            Ok(ApplicationResponse::Put) => {
                ResponsePacket::new_no_data(ResponseCode::Ok, HeaderSet::new())
            }
            Ok(ApplicationResponse::Get(_)) => {
                return Err(Error::operation(
                    OpCode::Put,
                    "invalid application response to PUT request",
                ));
            }
            Err((code, response_headers)) => {
                trace!("Application rejected PUT request: {code:?}");
                ResponsePacket::new_no_data(code, response_headers)
            }
        };
        self.state = State::Complete;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;

    use crate::header::{Header, HeaderIdentifier};

    #[fuchsia::test]
    fn single_stage_put_success() {
        let mut operation = PutOperation::new();
        assert!(!operation.is_complete());

        let body = (1..10).collect::<Vec<u8>>();
        let eob = Header::EndOfBody(body.clone());
        let name = Header::name("foo".into());
        let type_ = Header::Type("text".into());
        let headers = HeaderSet::from_headers(vec![eob, name, type_]).unwrap();
        let request = RequestPacket::new_put_final(headers);
        let response = operation.handle_peer_request(request).expect("valid request");
        assert_matches!(response,
            OperationRequest::PutApplicationData(data, headers)
            if headers.contains_header(&HeaderIdentifier::Name)
            && headers.contains_header(&HeaderIdentifier::Type)
            && data == body
        );
        assert!(!operation.is_complete());

        // Application accepts.
        let response = operation
            .handle_application_response(ApplicationResponse::accept_put())
            .expect("valid response");
        assert_eq!(*response.code(), ResponseCode::Ok);
        assert!(operation.is_complete());
    }

    #[fuchsia::test]
    fn multistage_put_success() {
        let mut operation = PutOperation::new();
        assert!(!operation.is_complete());

        // First request to provide informational headers describing the PUT payload. We will ack
        // with an empty Continue packet.
        let headers1 = HeaderSet::from_header(Header::name("random file".into()));
        let request1 = RequestPacket::new_put(headers1);
        let response1 = operation.handle_peer_request(request1).expect("valid request");
        assert_matches!(response1, OperationRequest::SendPacket(packet) if *packet.code() == ResponseCode::Continue);
        assert!(!operation.is_complete());

        // Second request just contains a part of the payload. We will ack with an empty Continue
        // packet.
        let body2 = (0..50).collect::<Vec<u8>>();
        let headers2 = HeaderSet::from_header(Header::Body(body2));
        let request2 = RequestPacket::new_put(headers2);
        let response2 = operation.handle_peer_request(request2).expect("valid request");
        assert_matches!(response2, OperationRequest::SendPacket(packet) if *packet.code() == ResponseCode::Continue);
        assert!(!operation.is_complete());

        // Third and final request contains the remaining payload. We will ask the application to
        // accept or reject with the complete reassembled data payload.
        let body3 = (50..100).collect::<Vec<u8>>();
        let headers3 = HeaderSet::from_header(Header::EndOfBody(body3));
        let request3 = RequestPacket::new_put_final(headers3);
        let response3 = operation.handle_peer_request(request3).expect("valid request");
        let expected_payload = (0..100).collect::<Vec<u8>>();
        assert_matches!(response3,
            OperationRequest::PutApplicationData(data, headers)
            if headers.contains_header(&HeaderIdentifier::Name)
            && data == expected_payload
        );
        assert!(!operation.is_complete());

        // Simulate application accepting.
        let response4 = operation
            .handle_application_response(ApplicationResponse::accept_put())
            .expect("valid response");
        assert_eq!(*response4.code(), ResponseCode::Ok);
        assert!(operation.is_complete());
    }

    #[fuchsia::test]
    fn application_reject_is_ok() {
        let mut operation = PutOperation::new_at_state(State::RequestPhaseComplete);

        let headers = HeaderSet::from_header(Header::Description("not allowed".into()));
        let reject = Err((ResponseCode::Forbidden, headers));
        let response_packet =
            operation.handle_application_response(reject).expect("valid response");
        assert_eq!(*response_packet.code(), ResponseCode::Forbidden);
        assert!(response_packet.headers().contains_header(&HeaderIdentifier::Description));
        assert!(operation.is_complete());
    }

    #[fuchsia::test]
    fn non_put_request_is_error() {
        let mut operation = PutOperation::new();
        let random_request1 = RequestPacket::new_get(HeaderSet::new());
        assert_matches!(
            operation.handle_peer_request(random_request1),
            Err(Error::OperationError { .. })
        );

        let random_request2 = RequestPacket::new_disconnect(HeaderSet::new());
        assert_matches!(
            operation.handle_peer_request(random_request2),
            Err(Error::OperationError { .. })
        );
    }

    #[fuchsia::test]
    fn invalid_application_response_is_error() {
        // Receiving a response before the request phase is complete is an Error.
        let mut operation = PutOperation::new();
        assert_matches!(
            operation.handle_application_response(ApplicationResponse::accept_put()),
            Err(Error::OperationError { .. })
        );

        // We don't expect a GET response from the application.
        let mut operation = PutOperation::new_at_state(State::RequestPhaseComplete);
        let invalid = ApplicationResponse::accept_get(vec![], HeaderSet::new());
        assert_matches!(
            operation.handle_application_response(invalid),
            Err(Error::OperationError { .. })
        );
    }
}
