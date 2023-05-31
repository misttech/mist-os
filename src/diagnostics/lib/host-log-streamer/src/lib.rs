// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use diagnostics_data::LogsData;
use futures_util::AsyncReadExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const READ_BUFFER_SIZE: usize = 1000 * 1000 * 5;

/// JSON decoder for log stream
pub struct JsonDecoder {
    /// Socket containing JSON logs
    socket: fuchsia_async::Socket,
    /// Buffer for reading from socket
    buffer: Vec<u8>,
    /// Current state of the decoder
    state: JsonDecoderState,
}

/// State of the JSON decoder
/// Can be either in the "Init" state at the beginning
/// of a message, or ReadingMessage while in the middle of one
#[derive(PartialEq)]
enum JsonDecoderState {
    /// Initial state
    Init,
    /// Reading a message
    ReadingMessage(Vec<u8>),
}

impl JsonDecoderState {
    fn take(&mut self) -> JsonDecoderState {
        let mut ret = JsonDecoderState::Init;
        std::mem::swap(self, &mut ret);
        ret
    }
}

/// Something that can contain either a single value or a Vec of values
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> From<OneOrMany<T>> for Vec<T> {
    fn from(v: OneOrMany<T>) -> Self {
        match v {
            OneOrMany::One(v) => vec![v],
            OneOrMany::Many(v) => v,
        }
    }
}

/// Error type for log streamer
#[derive(Error, Debug)]
pub enum LogError {
    #[error("Unknown error: {}", error)]
    UnknownError {
        #[from]
        error: anyhow::Error,
    },
    #[error("IO error {}", error)]
    IOError {
        #[from]
        error: std::io::Error,
    },
    #[error("Peer closed")]
    PeerClosed,
}

/// Callback invoked when a value is deserialized
#[async_trait(?Send)]
pub trait DeserializeCallback {
    async fn on_log_entry(&mut self, data: OneOrMany<LogsData>) -> anyhow::Result<()>;
}

impl JsonDecoder {
    /// Create a new JSON decoder
    pub fn new(socket: fuchsia_async::Socket) -> Self {
        let mut buffer = vec![];
        buffer.resize(READ_BUFFER_SIZE, 0);
        Self { socket, buffer, state: JsonDecoderState::Init }
    }

    /// Decode JSON from the socket, invoking the callback for each decoded message group
    pub async fn decode(
        &mut self,
        callback: &mut impl DeserializeCallback,
    ) -> Result<(), LogError> {
        loop {
            let mut next_state = JsonDecoderState::Init;
            match self.state.take() {
                JsonDecoderState::Init => {
                    let bytes_read = self.socket.read(&mut self.buffer).await?;
                    if bytes_read == 0 {
                        return Err(LogError::PeerClosed);
                    }
                    // Try to decode message
                    let des = serde_json::Deserializer::from_slice(&self.buffer[..bytes_read]);
                    let mut stream = des.into_iter::<OneOrMany<LogsData>>();
                    while let Some(Ok(value)) = stream.next() {
                        callback.on_log_entry(value).await?;
                    }
                    // byte_offset is the last deserialized object, if any
                    if stream.byte_offset() != bytes_read {
                        next_state = JsonDecoderState::ReadingMessage(
                            self.buffer[stream.byte_offset()..bytes_read].to_vec(),
                        );
                    }
                }
                JsonDecoderState::ReadingMessage(mut msg) => {
                    let res = self.socket.read(&mut self.buffer).await?;
                    if res == 0 {
                        return Err(LogError::PeerClosed);
                    }
                    // append to msg
                    msg.append(&mut self.buffer[..res].to_vec());
                    let des = serde_json::Deserializer::from_slice(&msg);
                    let mut stream = des.into_iter::<OneOrMany<LogsData>>();
                    while let Some(Ok(value)) = stream.next() {
                        callback.on_log_entry(value).await?;
                    }
                    // byte_offset is the last deserialized object, if any
                    if stream.byte_offset() != msg.len() {
                        next_state =
                            JsonDecoderState::ReadingMessage(msg[stream.byte_offset()..].to_vec());
                    }
                }
            }
            self.state = next_state;
            if self.state == JsonDecoderState::Init {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::cell::RefCell;

    use super::*;
    use assert_matches::assert_matches;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};

    const BOOT_TS: u64 = 98765432000000000;

    struct LogAccumulator<'a> {
        logs: &'a RefCell<Vec<LogsData>>,
    }

    impl<'a> LogAccumulator<'a> {
        fn new(accumulator: &'a RefCell<Vec<LogsData>>) -> Self {
            LogAccumulator { logs: accumulator }
        }
    }

    #[async_trait(?Send)]
    impl DeserializeCallback for LogAccumulator<'_> {
        async fn on_log_entry(&mut self, data: OneOrMany<LogsData>) -> anyhow::Result<()> {
            self.logs.borrow_mut().append(&mut Vec::from(data));
            Ok(())
        }
    }

    #[fuchsia::test]
    fn test_one_or_many() {
        let one: OneOrMany<u32> = serde_json::from_str("1").unwrap();
        assert_eq!(one, OneOrMany::One(1));
        let many: OneOrMany<u32> = serde_json::from_str("[1,2,3]").unwrap();
        assert_eq!(many, OneOrMany::Many(vec![1, 2, 3]));
    }

    #[fuchsia::test]
    async fn test_json_decoder() {
        // This is intentionally a datagram socket so we can
        // guarantee torn writes and test all the code paths
        // in the decoder.
        let (local, remote) = fuchsia_zircon::Socket::create_datagram();
        let mut decoder = JsonDecoder::new(fuchsia_async::Socket::from_socket(remote).unwrap());
        let test_log = vec![LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            severity: Severity::Info,
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
        })
        .set_message("Hello world!")
        .add_tag("Some tag")
        .build()];
        let serialized_log = serde_json::to_string(&test_log).unwrap();
        let serialized_bytes = serialized_log.as_bytes();
        let part_a = &serialized_bytes[..15];
        let part_b = &serialized_bytes[15..20];
        let part_c = &serialized_bytes[20..];
        local.write(part_a).unwrap();
        local.write(part_b).unwrap();
        local.write(part_c).unwrap();
        let decoded_log = RefCell::new(vec![]);
        let mut accumulator = LogAccumulator::new(&decoded_log);
        decoder.decode(&mut accumulator).await.unwrap();
        assert_eq!(&decoded_log.borrow().clone(), &test_log);
    }

    #[fuchsia::test]
    async fn test_json_decoder_regular_message() {
        // This is intentionally a datagram socket so we can
        // send the entire message as one "packet".
        let (local, remote) = fuchsia_zircon::Socket::create_datagram();
        let mut decoder = JsonDecoder::new(fuchsia_async::Socket::from_socket(remote).unwrap());
        let test_log = vec![LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            severity: Severity::Info,
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
        })
        .set_message("Hello world!")
        .add_tag("Some tag")
        .build()];
        let serialized_log = serde_json::to_string(&test_log).unwrap();
        let serialized_bytes = serialized_log.as_bytes();
        local.write(serialized_bytes).unwrap();
        let decoded_log = RefCell::new(vec![]);
        let mut accumulator = LogAccumulator::new(&decoded_log);
        decoder.decode(&mut accumulator).await.unwrap();
        assert_eq!(&decoded_log.borrow().clone(), &test_log);
    }

    #[fuchsia::test]
    async fn test_json_decoder_truncated_message() {
        // This is intentionally a datagram socket so we can
        // guarantee torn writes and test all the code paths
        // in the decoder.
        let (local, remote) = fuchsia_zircon::Socket::create_datagram();
        let mut decoder = JsonDecoder::new(fuchsia_async::Socket::from_socket(remote).unwrap());
        let test_log = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            severity: Severity::Info,
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
        })
        .set_message("Hello world!")
        .add_tag("Some tag")
        .build();
        let serialized_log = serde_json::to_string(&test_log).unwrap();
        let serialized_bytes = serialized_log.as_bytes();
        let part_a = &serialized_bytes[..15];
        let part_b = &serialized_bytes[15..20];
        local.write(part_a).unwrap();
        local.write(part_b).unwrap();
        drop(local);
        let decoded_log = RefCell::new(vec![]);
        let mut accumulator = LogAccumulator::new(&decoded_log);
        assert_matches!(decoder.decode(&mut accumulator).await, Err(LogError::PeerClosed));
    }
}
