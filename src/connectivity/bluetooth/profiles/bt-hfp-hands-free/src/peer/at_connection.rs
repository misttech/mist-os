// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42077657) Use this in task.rs.
#![allow(unused)]

use anyhow::{format_err, Result};
use at_commands as at;
use at_commands::{DeserializeBytes, SerDe};
use fuchsia_bluetooth::types::{Channel, PeerId};
use futures::io::AsyncWriteExt;
use futures::stream::FusedStream;
use futures::Stream;
use log::warn;
use std::collections::VecDeque;
use std::io::Cursor;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct AtConnection {
    peer_id: PeerId,
    rfcomm: Channel,
    unreturned_responses: VecDeque<at::Response>,
    remaining_bytes: DeserializeBytes,
}

/// Stream for AtConnection.  The stream produces at::Responses coming in from the peer.  These are
/// yielded one at a time.  While we expect spec-compliant peers to give us one AT response per
/// RFCOMM data transfer, this stream will assemble fragmented AT responses and split multiple AT
/// responses which arrive together into a series of well formed responses.
impl Stream for AtConnection {
    type Item = Result<at::Response, zx::Status>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.is_terminated() {
                panic!(
                    "Tried to poll AT Connection to peer {} after it was terminated.",
                    self.peer_id
                )
            }

            if let Some(item) = self.unreturned_responses.pop_front() {
                return Poll::Ready(Some(Ok(item)));
            }

            let rfcomm = Pin::new(&mut self.rfcomm);
            let bytes_poll = rfcomm.poll_next(context);
            match bytes_poll {
                Poll::Pending => return Poll::Pending, // Nothing to read
                Poll::Ready(None) => return Poll::Ready(None), // Channel is closed
                Poll::Ready(Some(Err(err))) => return Poll::Ready(Some(Err(err))), // zx::Status indicating error
                Poll::Ready(Some(Ok(bytes))) if bytes.len() == 0 => continue, // Got no bytes; try again
                Poll::Ready(Some(Ok(bytes))) => {
                    // Received bytes
                    let mut cursor = Cursor::new(&bytes);
                    let remaining_bytes = mem::take(&mut self.remaining_bytes);
                    let at::DeserializeResult {
                        values: deserialized_values,
                        error: deserialize_error,
                        remaining_bytes,
                    } = at::Response::deserialize(&mut cursor, remaining_bytes);
                    self.remaining_bytes = remaining_bytes;
                    if let Some(error) = deserialize_error {
                        // In this case, we may have other commands that deserialized correctly, so continue.
                        warn!(
                            "Could not deserialize AT response received from peer {}: {:?}",
                            self.peer_id, error
                        );
                    }

                    self.unreturned_responses.append(&mut deserialized_values.into());
                    // Loop.
                }
            }
        }
    }
}

impl FusedStream for AtConnection {
    fn is_terminated(&self) -> bool {
        self.unreturned_responses.is_empty() && self.rfcomm.is_terminated()
    }
}

impl AtConnection {
    pub fn new(peer_id: PeerId, rfcomm: Channel) -> Self {
        Self {
            peer_id,
            rfcomm,
            unreturned_responses: VecDeque::new(),
            remaining_bytes: at::DeserializeBytes::new(),
        }
    }

    /// Serializes the AT commands and sends them through the RFCOMM channel
    // TODO(fxr/127025) use this in PeerTask.
    #[allow(unused)]
    pub async fn write_commands(&mut self, commands: &[at::Command]) -> Result<()> {
        if commands.len() > 0 {
            let mut bytes = Vec::new();
            at::Command::serialize(&mut bytes, commands).map_err(|err| {
                format_err!(
                    "Failed to serialize AT commands to channel for peer {:}: {:?}",
                    self.peer_id,
                    err
                )
            })?;
            self.rfcomm.write_all(&bytes).await.map_err(|err| {
                format_err!(
                    "Could not write serialized AT commands to channel for peer {:}: {:?}",
                    self.peer_id,
                    err
                )
            })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use async_utils::PollExt;
    use fuchsia_async as fasync;
    use futures::StreamExt;

    #[fuchsia::test]
    fn at_response_received() {
        let mut exec = fasync::TestExecutor::new();
        let (mut near, far) = Channel::create();

        let mut conn = AtConnection::new(PeerId(1), far);

        let response_bytes = "+BRSF:0\r".as_bytes();
        exec.run_singlethreaded(near.write_all(&response_bytes)).expect("Sent AT");

        let response = exec
            .run_singlethreaded(conn.next())
            .expect("Received channel read closed error")
            .expect("Received channel read Zircon error");

        let expected_response = at::Response::Success(at::Success::Brsf { features: 0 });
        assert_eq!(response, expected_response);
    }

    #[fuchsia::test]
    fn at_responses_received() {
        let mut exec = fasync::TestExecutor::new();
        let (mut near, far) = Channel::create();

        let mut conn = AtConnection::new(PeerId(1), far);

        let response_bytes = "+BRSF:1\r+BRSF:2\r".as_bytes();
        exec.run_singlethreaded(near.write_all(&response_bytes)).expect("Sent AT");

        let response_1 = exec
            .run_singlethreaded(conn.next())
            .expect("Received channel read closed error")
            .expect("Received channel read Zircon error");

        let expected_response_1 = at::Response::Success(at::Success::Brsf { features: 1 });
        assert_eq!(response_1, expected_response_1);

        let response_2 = exec
            .run_singlethreaded(conn.next())
            .expect("Received AT connection closed error")
            .expect("Received AT connection Zircon error");

        let expected_response_2 = at::Response::Success(at::Success::Brsf { features: 2 });
        assert_eq!(response_2, expected_response_2);
    }

    #[fuchsia::test]
    fn at_response_received_and_defragmented() {
        let mut exec = fasync::TestExecutor::new();
        let (mut near, far) = Channel::create();

        let mut conn = AtConnection::new(PeerId(1), far);

        let response_bytes_1 = "+BRS".as_bytes();
        let response_bytes_2 = "F:0\r".as_bytes();

        exec.run_singlethreaded(near.write_all(&response_bytes_1)).expect("Sent AT 1");

        exec.run_until_stalled(&mut conn.next()).expect_pending("Reading AT fragmanet");

        exec.run_singlethreaded(near.write_all(&response_bytes_2)).expect("Sent AT 2");

        let response = exec
            .run_singlethreaded(&mut conn.next())
            .expect("Received channel read closed error")
            .expect("Received channel read Zircon error");

        let expected_response = at::Response::Success(at::Success::Brsf { features: 0 });
        assert_eq!(response, expected_response);
    }

    #[fuchsia::test]
    fn at_commands_written() {
        let mut exec = fasync::TestExecutor::new();
        let (mut near, far) = Channel::create();

        let mut conn = AtConnection::new(PeerId(1), far);

        let command_1 = at::Command::Brsf { features: 1 };
        let command_2 = at::Command::Brsf { features: 2 };

        exec.run_singlethreaded(conn.write_commands(&[command_1, command_2]))
            .expect("Sent commands");

        let commands_bytes = exec
            .run_singlethreaded(near.next())
            .expect("Received channel read closed error")
            .expect("Received channel read Zircon error");

        let expected_commands_bytes: Vec<u8> = "AT+BRSF=1\rAT+BRSF=2\r".into();

        assert_eq!(commands_bytes, expected_commands_bytes);
    }

    #[fuchsia::test]
    fn stream_terminates_when_responses_are_consumed() {
        let mut exec = fasync::TestExecutor::new();
        let (mut near, far) = Channel::create();

        let mut conn = AtConnection::new(PeerId(1), far);

        let response_bytes = "+BRSF:0\r".as_bytes();
        exec.run_singlethreaded(near.write_all(&response_bytes)).expect("Sent AT");

        // RFCOMM is open and response isn't yet consumed.
        assert!(!conn.is_terminated());

        drop(near);

        // RFCOMM is closed and response isn't yet consumed.
        assert!(!conn.is_terminated());

        let response = exec
            .run_singlethreaded(conn.next())
            .expect("Received channel read closed error")
            .expect("Received channel read Zircon error");

        let expected_response = at::Response::Success(at::Success::Brsf { features: 0 });
        assert_eq!(response, expected_response);

        // Read again to get a closed channel
        let response = exec.run_singlethreaded(conn.next());
        assert_eq!(response, None);

        // RFCOMM is closed and response is consumed.
        assert!(conn.is_terminated());
    }
}
