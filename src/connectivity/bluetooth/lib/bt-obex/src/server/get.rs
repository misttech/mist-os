// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use packet_encoding::Encodable;
use std::collections::VecDeque;
use tracing::warn;

use crate::error::Error;
use crate::header::{Header, HeaderSet};
use crate::operation::{OpCode, RequestPacket, ResponseCode, ResponsePacket};

/// All Body & EndOfBody headers have 3 bytes (1 byte HI, 2 bytes Length) preceding the payload.
const BODY_HEADER_PREFIX_LENGTH_BYTES: usize = 3;

/// A collection that maintains the staged data during a GET operation.
#[derive(Debug, PartialEq)]
struct StagedData {
    /// The first chunk of user data.
    /// This is Some<T> when the first chunk is available and can be taken with `first_response`
    /// and None otherwise.
    /// In some cases, no user data can fit in the first chunk and so this will be Some<None>.
    first: Option<Option<Vec<u8>>>,
    /// The remaining chunks of user data to be sent.
    /// The `StagedData` is considered exhausted and complete when this is empty.
    rest: VecDeque<Vec<u8>>,
}

impl StagedData {
    fn new(first: Option<Vec<u8>>, rest: VecDeque<Vec<u8>>) -> Self {
        Self { first: Some(first), rest }
    }

    /// Builds and stages the `data` into chunks of Headers (Body/EndOfBody) that conform to the
    /// provided `max_headers_size`.
    /// `header_size` is the size (in bytes) of the informational headers describing the `data`.
    /// Returns Ok if the data was successfully divided, Error if the data could not be chunked or
    /// if the provided `max_headers_size` or `headers_size` is invalid.
    fn from_data(
        mut data: Vec<u8>,
        max_headers_size: u16,
        headers_size: usize,
    ) -> Result<Self, Error> {
        let max_headers_size = max_headers_size as usize;
        // To many headers provided.
        if headers_size > max_headers_size {
            warn!("Too many headers in GET");
            // TODO(fxbug.dev/132595): It's probably reasonable to support this case by splitting
            // the headers across multiple responses.
            return Err(Error::operation(OpCode::Get, "too many headers"));
        }

        // The maximum header size must be able to fit at least a Body/EndOfBody Header with one
        // byte of data. In practice, this max is in the range of [255, u16::MAX], but this
        // function is resilient to smaller values.
        if max_headers_size <= BODY_HEADER_PREFIX_LENGTH_BYTES {
            return Err(Error::operation(OpCode::Get, "max_headers_size too small"));
        }

        // The maximum size of the first packet is special as it may contain non-Body/EndOfBody
        // `headers` and a chunk of user data in the Body/EndOfBody.
        let max_first_data_packet_size = max_headers_size - headers_size;

        // Check if the entire payload can fit in a single packet.
        let data_encoded_len = data.len() + BODY_HEADER_PREFIX_LENGTH_BYTES;
        if data_encoded_len <= max_first_data_packet_size {
            return Ok(Self::new(Some(data), VecDeque::new()));
        }

        // Otherwise, we'll need more than one packet. Chunk the special first packet.
        let first_chunk_size =
            max_first_data_packet_size.checked_sub(BODY_HEADER_PREFIX_LENGTH_BYTES);
        let (first, remaining) = if let Some(max) = first_chunk_size {
            let remaining = data.split_off(max);
            (Some(data), remaining)
        } else {
            // No space in first packet for any user data.
            (None, data)
        };

        // The maximum size of all other packets where only data is present.
        let max_data_packet_size = max_headers_size - BODY_HEADER_PREFIX_LENGTH_BYTES;
        // Chunk up the rest.
        let mut chunks = VecDeque::new();
        for chunk in remaining.chunks(max_data_packet_size as usize) {
            chunks.push_back(chunk.to_vec());
        }
        Ok(Self::new(first, chunks))
    }

    /// Returns true if the first response exists and can be taken.
    fn is_first_response(&self) -> bool {
        self.first.is_some()
    }

    /// Returns true if the first response and all staged data has been taken.
    fn is_complete(&self) -> bool {
        self.first.is_none() && self.rest.is_empty()
    }

    /// Returns the first response in the staged data.
    /// Returns the response code and potential first user data chunk on success, Error otherwise.
    fn first_response(&mut self) -> Result<(ResponseCode, Option<Header>), Error> {
        if self.is_complete() {
            return Err(Error::operation(OpCode::Get, "staged data exhausted"));
        }

        let first_packet = self
            .first
            .take()
            .ok_or(Error::operation(OpCode::Get, "first response already taken"))?;
        if self.rest.is_empty() {
            Ok((ResponseCode::Ok, first_packet.map(|p| Header::EndOfBody(p))))
        } else {
            Ok((ResponseCode::Continue, first_packet.map(|p| Header::Body(p))))
        }
    }

    /// Returns the response code and Header for the next chunk of data in the staged payload.
    /// Returns Error if the first chunk of data has not been retrieved.
    /// Returns Error if all the data has already been returned - namely, `Self::is_complete` is
    /// true.
    fn next_response(&mut self) -> Result<(ResponseCode, Header), Error> {
        if self.is_complete() || self.is_first_response() {
            return Err(Error::operation(OpCode::Get, "next_response called from invalid state"));
        }

        let next_chunk = self.rest.pop_front();
        // Last chunk.
        if self.rest.is_empty() {
            let h = Header::EndOfBody(next_chunk.unwrap_or(vec![]));
            Ok((ResponseCode::Ok, h))
        } else {
            // Otherwise, it's a multi-packet response.
            let h = Header::Body(next_chunk.expect("more than one chunk"));
            Ok((ResponseCode::Continue, h))
        }
    }
}

/// The current state of the GET operation.
#[derive(Debug)]
enum State {
    /// The request phase of the operation in which the remote OBEX client sends informational
    /// headers describing the payload. This can be spread out over multiple packets.
    /// `headers` will be updated each time new headers are received.
    Request { headers: HeaderSet },
    /// The request phase of the operation is complete (GET_FINAL has been received) and we are
    /// waiting for the profile application to accept or reject the request.
    /// The response phase (`State::Response { .. }`) is started by calling
    /// `GetOperation::start_response_phase` which indicates whether the profile application has
    /// accepted or rejected the GET request.
    RequestPhaseComplete,
    /// The profile application has accepted the GET request.
    /// The response phase of the operation in which the local OBEX server sends the payload over
    /// potentially multiple packets.
    Response { staged_data: StagedData },
    /// The response phase of the operation is complete (all data packets have been relayed). The
    /// entire GET operation is considered complete.
    Complete,
}

/// Represents an action to be taken in the operation.
#[derive(Debug)]
pub enum Action {
    /// Send a response packet to the remote peer (OBEX client).
    SendPacket(ResponsePacket),
    /// Ask the application to accept or reject the request for the payload.
    /// The `HeaderSet` describes the object to be retrieved.
    GetApplicationData(HeaderSet),
}

/// Represents an in-progress GET operation.
pub struct GetOperation {
    /// The maximum number of bytes that can be allocated to headers in the GetOperation. This
    /// includes informational headers and data headers.
    max_headers_size: u16,
    state: State,
}

impl GetOperation {
    /// `max_packet_size` is the max number of bytes that can fit in a single packet.
    pub fn new(max_packet_size: u16) -> Self {
        let max_headers_size = max_packet_size - ResponsePacket::MIN_PACKET_SIZE as u16;
        Self { max_headers_size, state: State::Request { headers: HeaderSet::new() } }
    }

    #[cfg(test)]
    fn new_at_state(max_packet_size: u16, state: State) -> Self {
        let max_headers_size = max_packet_size - ResponsePacket::MIN_PACKET_SIZE as u16;
        Self { max_headers_size, state }
    }

    /// Returns true if the operation is complete (e.g. all response packets have been sent).
    pub fn is_complete(&self) -> bool {
        matches!(self.state, State::Complete)
    }

    fn check_complete_and_update_state(&mut self) {
        let State::Response { ref staged_data } = &self.state else {
            return
        };

        if staged_data.is_complete() {
            self.state = State::Complete;
        }
    }

    /// Handles a request packet received from the remote OBEX client.
    /// On success, returns an `Action` to be taken by the local OBEX server.
    /// Returns Error if the `request` couldn't be handled or was invalid.
    pub fn handle_request(&mut self, request: RequestPacket) -> Result<Action, Error> {
        let code = *request.code();
        match &mut self.state {
            State::Request { ref mut headers } if code == OpCode::Get => {
                headers.try_append(HeaderSet::from(request))?;
                let response = ResponsePacket::new_get(ResponseCode::Continue, HeaderSet::new());
                Ok(Action::SendPacket(response))
            }
            State::Request { ref mut headers } if code == OpCode::GetFinal => {
                headers.try_append(HeaderSet::from(request))?;
                let request_headers = std::mem::replace(headers, HeaderSet::new());
                // Received the final request packet. The set of request headers is considered
                // complete and we are ready to ask the application for the payload.
                self.state = State::RequestPhaseComplete;
                Ok(Action::GetApplicationData(request_headers))
            }
            State::Response { ref mut staged_data } if code == OpCode::GetFinal => {
                let (code, body_header) = staged_data.next_response()?;
                let response = ResponsePacket::new_get(code, HeaderSet::from_header(body_header)?);
                self.check_complete_and_update_state();
                Ok(Action::SendPacket(response))
            }
            _ => Err(Error::operation(OpCode::Get, "received invalid request")),
        }
    }

    /// Starts the response phase of the GET operation.
    /// Returns the first response packet to be sent to the remote peer. There may be
    /// more response packets which will be processed in subsequent `handle_request` calls.
    /// Returns Error if the `data` couldn't be processed or the response could not be built.
    pub fn start_response_phase(
        &mut self,
        data: Vec<u8>,
        mut response_headers: HeaderSet,
    ) -> Result<ResponsePacket, Error> {
        if !matches!(self.state, State::RequestPhaseComplete) {
            return Err(Error::operation(OpCode::Get, "invalid state"));
        }

        // Potentially split the user data payload into chunks to be sent over multiple response
        // packets. Grab the first packet and stage the rest.
        let mut staged_data =
            StagedData::from_data(data, self.max_headers_size, response_headers.encoded_len())?;
        let (code, body_header) = staged_data.first_response()?;
        self.state = State::Response { staged_data };
        self.check_complete_and_update_state();

        // Update the header set and build the response packet.
        if let Some(header) = body_header {
            response_headers.add(header)?;
        }
        Ok(ResponsePacket::new_get(code, response_headers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;

    use crate::header::HeaderIdentifier;

    fn bytes(start_idx: usize, end_idx: usize) -> Vec<u8> {
        // NOTE: In practice this can result in unexpected behavior if `start_idx` and `end_idx`
        // are too large.
        let s = start_idx as u8;
        let e = end_idx as u8;
        (s..e).collect::<Vec<u8>>()
    }

    #[track_caller]
    fn expect_packet_with_body(
        action: Action,
        final_: bool,
        expected_code: ResponseCode,
        expected_body: Vec<u8>,
    ) {
        let body = if let Action::SendPacket(packet) = action {
            assert_eq!(*packet.code(), expected_code);
            let mut h = HeaderSet::from(packet);
            h.remove_body(final_).expect("contains body")
        } else {
            panic!("Expected send packet, got: {:?}", action);
        };
        assert_eq!(body, expected_body);
    }

    #[fuchsia::test]
    fn single_packet_get_operation() {
        let max_packet_size = 50;
        let mut operation = GetOperation::new(max_packet_size);
        assert!(!operation.is_complete());

        // First (and final) request with informational headers.
        let headers = HeaderSet::from_header(Header::name("default")).unwrap();
        let request = RequestPacket::new_get_final(headers);
        let response1 = operation.handle_request(request).expect("valid request");
        assert_matches!(response1,
            Action::GetApplicationData(headers)
            if headers.contains_header(&HeaderIdentifier::Name)
        );

        // Application provides the payload. Since the entire payload can fit in a single packet,
        // we expect the operation to be complete after it is returned.
        let payload = bytes(0, 25);
        let response2 =
            operation.start_response_phase(payload, HeaderSet::new()).expect("valid request");
        assert_eq!(*response2.code(), ResponseCode::Ok);
        let mut received_headers = HeaderSet::from(response2);
        let received_body = received_headers.remove_body(/*final_=*/ true).expect("contains body");
        assert_eq!(received_body, bytes(0, 25));
        assert!(operation.is_complete());
    }

    #[fuchsia::test]
    fn multi_packet_get_operation() {
        let max_packet_size = 50;
        let mut operation = GetOperation::new(max_packet_size);
        assert!(!operation.is_complete());

        // First request provides informational headers. Expect a positive ack to the request.
        let headers1 = HeaderSet::from_header(Header::name("foo".into())).unwrap();
        let request1 = RequestPacket::new_get(headers1);
        let response1 = operation.handle_request(request1).expect("valid request");
        assert_matches!(response1, Action::SendPacket(packet) if *packet.code() == ResponseCode::Continue);

        // Second and final request provides informational headers. Expect to ask the profile
        // application for the user data payload. The received informational headers should be
        // relayed.
        let headers2 = HeaderSet::from_header(Header::Type("text/x-vCard".into())).unwrap();
        let request2 = RequestPacket::new_get_final(headers2);
        let response2 = operation.handle_request(request2).expect("valid request");
        assert_matches!(response2,
            Action::GetApplicationData(headers)
            if headers.contains_header(&HeaderIdentifier::Name)
            && headers.contains_header(&HeaderIdentifier::Type)
        );

        // After providing the payload, we expect the first response packet to contain the
        // response headers and the first chunk of the payload since it cannot fit in
        // `max_packet_size`.
        // The `response_headers` has an encoded size of 33 bytes. This leaves 17 bytes for the
        // first chunk of user data, of which 6 bytes are allocated to the prefix.
        let payload = bytes(0, 200);
        let response_headers =
            HeaderSet::from_header(Header::Description("random payload".into())).unwrap();
        let response_packet3 =
            operation.start_response_phase(payload, response_headers).expect("valid request");
        assert_eq!(*response_packet3.code(), ResponseCode::Continue);
        let mut received_headers = HeaderSet::from(response_packet3);
        assert!(received_headers.contains_header(&HeaderIdentifier::Description));
        let received_body = received_headers.remove_body(/*final_=*/ false).expect("contains body");
        assert_eq!(received_body, bytes(0, 11));

        // Peer will keep asking for payload until finished.
        // Each data chunk will be 44 bytes long (max 50 - 3 bytes for response prefix - 3 bytes for
        // header prefix)
        let expected_bytes = vec![bytes(11, 55), bytes(55, 99), bytes(99, 143), bytes(143, 187)];
        for expected in expected_bytes {
            let request = RequestPacket::new_get_final(HeaderSet::new());
            let response = operation.handle_request(request).expect("valid request");
            expect_packet_with_body(
                response,
                /*final_=*/ false,
                ResponseCode::Continue,
                expected,
            );
        }

        // Final packet and the operation is complete.
        let request4 = RequestPacket::new_get_final(HeaderSet::new());
        let response4 = operation.handle_request(request4).expect("valid request");
        expect_packet_with_body(response4, /*final=*/ true, ResponseCode::Ok, bytes(187, 200));
        assert!(operation.is_complete());
    }

    #[fuchsia::test]
    fn application_response_error() {
        let max_packet_size = 15;
        // Receiving the application response before the request phase is complete is an Error.
        let mut operation = GetOperation::new(max_packet_size);
        let data = vec![1, 2, 3];
        assert_matches!(
            operation.start_response_phase(data, HeaderSet::new()),
            Err(Error::OperationError { .. })
        );
    }

    #[fuchsia::test]
    fn non_get_request_is_error() {
        let mut operation = GetOperation::new(50);
        let random_request1 = RequestPacket::new_put(HeaderSet::new());
        assert_matches!(
            operation.handle_request(random_request1),
            Err(Error::OperationError { .. })
        );

        let random_request2 = RequestPacket::new_disconnect(HeaderSet::new());
        assert_matches!(
            operation.handle_request(random_request2),
            Err(Error::OperationError { .. })
        );
    }

    #[fuchsia::test]
    fn get_request_invalid_state_is_error() {
        let random_headers = HeaderSet::from_header(Header::name("foo".into())).unwrap();

        // Receiving another GET request while we are waiting for the application to accept is an
        // Error.
        let mut operation1 = GetOperation::new_at_state(10, State::RequestPhaseComplete);
        let request1 = RequestPacket::new_get(random_headers.clone());
        let response1 = operation1.handle_request(request1);
        assert_matches!(response1, Err(Error::OperationError { .. }));

        // Receiving a GET request when the operation is complete is an Error.
        let mut operation2 = GetOperation::new_at_state(10, State::Complete);
        let request2 = RequestPacket::new_get(random_headers.clone());
        let response2 = operation2.handle_request(request2);
        assert_matches!(response2, Err(Error::OperationError { .. }));

        // Receiving a non-GETFINAL request in the response phase is an Error.
        let staged_data = StagedData { first: None, rest: VecDeque::from(vec![vec![1, 2, 3]]) };
        let mut operation3 = GetOperation::new_at_state(10, State::Response { staged_data });
        let request3 = RequestPacket::new_get(random_headers);
        let response3 = operation3.handle_request(request3);
        assert_matches!(response3, Err(Error::OperationError { .. }));
    }

    #[fuchsia::test]
    fn build_staged_data_success() {
        // An empty data payload is fine. Should be chunked as a single packet.
        let empty_data = Vec::new();
        let empty_headers = HeaderSet::new();
        let result = StagedData::from_data(empty_data, 50, empty_headers.encoded_len())
            .expect("can divide data");
        let expected = StagedData { first: Some(Some(vec![])), rest: VecDeque::new() };
        assert_eq!(result, expected);

        // A data payload that can fit in a single packet.
        let headers = HeaderSet::from_header(Header::name("foo".into())).unwrap();
        let data = vec![1, 2, 3];
        let result =
            StagedData::from_data(data, 50, headers.encoded_len()).expect("can divide data");
        let expected = StagedData { first: Some(Some(vec![1, 2, 3])), rest: VecDeque::new() };
        assert_eq!(result, expected);

        // A data payload with headers that is split into multiple packets. The first chunk is
        // smaller since there must be room for the provided headers.
        let headers = HeaderSet::from_header(Header::Http(vec![5, 5, 5])).unwrap();
        let max = 10;
        let large_data = (0..50).collect::<Vec<u8>>();
        let result =
            StagedData::from_data(large_data, max, headers.encoded_len()).expect("can divide data");
        let first = Some(vec![0]);
        let rest = VecDeque::from(vec![
            bytes(1, 8),
            bytes(8, 15),
            bytes(15, 22),
            bytes(22, 29),
            bytes(29, 36),
            bytes(36, 43),
            bytes(43, 50),
        ]);
        let expected = StagedData { first: Some(first), rest };
        assert_eq!(result, expected);
    }

    #[fuchsia::test]
    fn build_staged_data_error() {
        let random_data = bytes(0, 50);

        // Cannot build and stage data if the overall max packet size is too small.
        let too_small_max = 2;
        assert_matches!(
            StagedData::from_data(random_data.clone(), too_small_max, 0),
            Err(Error::OperationError { .. })
        );
        assert_matches!(
            StagedData::from_data(random_data.clone(), 0, 0),
            Err(Error::OperationError { .. })
        );

        // Cannot build and stage data if the header size is larger than the max.
        // TODO(fxbug.dev/132595): Delete this case when headers can be split across packets.
        let small_max = 10;
        let large_header_size = 20;
        assert_matches!(
            StagedData::from_data(random_data, small_max, large_header_size),
            Err(Error::OperationError { .. })
        );
    }

    #[fuchsia::test]
    fn empty_staged_data_success() {
        let empty = Vec::new();
        let mut staged = StagedData::from_data(empty.clone(), 50, 0).expect("can construct");
        assert!(staged.is_first_response());
        assert!(!staged.is_complete());
        let (c, h) = staged.first_response().expect("has first response");
        assert_eq!(c, ResponseCode::Ok);
        assert_matches!(h, Some(Header::EndOfBody(v)) if v == empty);
        assert!(!staged.is_first_response());
        assert!(staged.is_complete());
    }

    #[fuchsia::test]
    fn single_packet_staged_data_success() {
        let single = vec![1, 2, 3];
        let mut staged = StagedData::from_data(single.clone(), 50, 0).expect("can construct");
        let (c, h) = staged.first_response().expect("has first response");
        assert_eq!(c, ResponseCode::Ok);
        assert_matches!(h, Some(Header::EndOfBody(v)) if v == single);
        assert!(staged.is_complete());
    }

    #[fuchsia::test]
    fn multi_packet_staged_data_success() {
        let max_packet_size = 10;
        let large_data = (0..30).collect::<Vec<u8>>();
        let header_size = 8;
        let mut staged =
            StagedData::from_data(large_data, max_packet_size, header_size).expect("can construct");
        let (c, h) = staged.first_response().expect("has first response");
        assert_eq!(c, ResponseCode::Continue);
        // First buffer has no user data since it can't fit with headers.
        assert_matches!(h, None);
        assert!(!staged.is_complete());

        // Each next chunk should contain 7 bytes each since the max is 10 and 3 bytes are
        // reserved for the header prefix.
        let expected_bytes = vec![bytes(0, 7), bytes(7, 14), bytes(14, 21), bytes(21, 28)];
        for expected in expected_bytes {
            let (c, h) = staged.next_response().expect("has next response");
            assert_eq!(c, ResponseCode::Continue);
            assert_matches!(h, Header::Body(v) if v == expected);
        }

        // Final chunk has the remaining bits and an Ok response code to signal completion.
        let (c, h) = staged.next_response().expect("has next response");
        assert_eq!(c, ResponseCode::Ok);
        let expected = bytes(28, 30);
        assert_matches!(h, Header::EndOfBody(v) if v == expected);
        assert!(staged.is_complete());
    }

    #[fuchsia::test]
    fn staged_data_response_error() {
        // Calling `first_response` twice is Error.
        let mut staged = StagedData::new(None, VecDeque::from(vec![vec![1]]));
        let _ = staged.first_response().expect("has first response");
        assert!(!staged.is_complete());
        assert_matches!(staged.first_response(), Err(Error::OperationError { .. }));

        // Calling `first_response` when the operation is considered complete is Error.
        let mut staged_no_data = StagedData::new(None, VecDeque::new());
        let _ = staged_no_data.first_response().expect("has first response");
        assert!(staged_no_data.is_complete());
        assert_matches!(staged_no_data.first_response(), Err(Error::OperationError { .. }));

        // Calling `next_response` before `first_response` is Error.
        let mut staged = StagedData::new(None, VecDeque::new());
        assert_matches!(staged.next_response(), Err(Error::OperationError { .. }));
        // Calling `next_response` when complete is Error.
        let _ = staged.first_response().expect("has first response");
        assert!(staged.is_complete());
        assert_matches!(staged.next_response(), Err(Error::OperationError { .. }));
    }
}
