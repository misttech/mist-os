// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::{LogsData, Severity};
use futures_util::{
    stream::{unfold, BoxStream},
    AsyncReadExt, Stream, StreamExt,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Read buffer size. Sufficiently large to store a large number
/// of messages to reduce the number of socket read calls we have
/// to make when reading messages.
const READ_BUFFER_SIZE: usize = 1000 * 1000 * 2;

enum JsonReadState {
    /// Initial decode state
    Init,
    /// Reading a message (offset, buffer length)
    ReadingMessage((usize, usize)),
    /// Partial read (middle of message)
    PartialRead(Vec<u8>),
}

impl JsonReadState {
    fn new() -> Self {
        JsonReadState::Init
    }
    fn take(&mut self) -> Self {
        std::mem::replace(self, JsonReadState::Init)
    }
}

// JSON decoder state
struct State {
    socket: fuchsia_async::Socket,
    buffer: Vec<u8>,
    json: JsonReadState,
}

impl State {
    /// Reads from the socket, returns None if the value is zero
    async fn read(&mut self) -> Option<usize> {
        self.socket
            .read(&mut self.buffer)
            .await
            .ok()
            .map(|value| if value == 0 { None } else { Some(value) })
            .flatten()
    }
}

// Reads LogsData from a socket.
// This can't be in a lambda as we need to explicitly specify lifetime bounds.
async fn read_socket_impl(mut value: State) -> Option<(OneOrMany<LogsData>, State)> {
    loop {
        match value.json.take() {
            JsonReadState::Init => {
                // read from socket and create iterator from slice
                let ret = value.read().await?;
                let des = serde_json::Deserializer::from_slice(&value.buffer[..ret]);
                let mut msg = des.into_iter::<OneOrMany<LogsData>>();
                let maybe_log = msg.next();
                if let Some(Ok(log)) = maybe_log {
                    value.json = JsonReadState::ReadingMessage((msg.byte_offset(), ret));
                    return Some((log, value));
                } else {
                    value.json = JsonReadState::PartialRead(value.buffer[..ret].to_vec());
                }
            }
            JsonReadState::ReadingMessage((offset, buffer_len)) => {
                let des = serde_json::Deserializer::from_slice(&value.buffer[offset..buffer_len]);
                let mut msg = des.into_iter::<OneOrMany<LogsData>>();
                match msg.next() {
                    Some(Ok(log)) => {
                        return Some((log, value));
                    }
                    _ => {
                        // End of message
                        if msg.byte_offset() + offset != buffer_len {
                            // partial read state
                            let mut partial_read = vec![];
                            partial_read
                                .extend_from_slice(&value.buffer[offset + msg.byte_offset()..]);
                            value.json = JsonReadState::PartialRead(partial_read);
                        }
                    }
                }
            }
            JsonReadState::PartialRead(mut partial) => {
                // read from socket
                let ret = value.read().await?;
                partial.extend_from_slice(&value.buffer[..ret]);
                let des = serde_json::Deserializer::from_slice(&partial);
                let mut msg = des.into_iter::<OneOrMany<LogsData>>();
                match msg.next() {
                    Some(Ok(log)) => {
                        value.json =
                            JsonReadState::PartialRead(partial[msg.byte_offset()..].to_vec());
                        return Some((log, value));
                    }
                    _ => {
                        // End of message
                        if msg.byte_offset() != partial.len() {
                            // partial read state
                            let mut partial_read = vec![];
                            partial_read.extend_from_slice(&partial[msg.byte_offset()..]);
                            value.json = JsonReadState::PartialRead(partial_read);
                        }
                    }
                }
            }
        }
    }
}

/// Streams raw JSON from a socket as an async stream
fn stream_raw_json(stream: fuchsia_async::Socket) -> impl Stream<Item = OneOrMany<LogsData>> {
    let mut buffer = vec![];
    buffer.resize(READ_BUFFER_SIZE, 0);
    unfold(State { socket: stream, buffer, json: JsonReadState::new() }, read_socket_impl)
}

/// Streams JSON logs from a socket
fn stream_json(socket: fuchsia_async::Socket) -> impl Stream<Item = LogsData> {
    stream_raw_json(socket).map(|value| futures_util::stream::iter(value)).flatten()
}

/// Stream of JSON logs from the target device.
pub struct LogsDataStream {
    inner: BoxStream<'static, LogsData>,
}

impl LogsDataStream {
    /// Creates a new LogsDataStream from a socket of log messages in JSON format.
    pub fn new(socket: fuchsia_async::Socket) -> Self {
        Self { inner: Box::pin(stream_json(socket)) }
    }
}

impl Stream for LogsDataStream {
    type Item = LogsData;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.poll_next_unpin(cx)
    }
}

/// Something that can contain either a single value or a Vec of values
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

enum OneOrManyIterator<T> {
    One(Option<T>),
    Many(std::vec::IntoIter<T>),
}

impl<T> Iterator for OneOrManyIterator<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            OneOrManyIterator::One(v) => v.take(),
            OneOrManyIterator::Many(v) => v.next(),
        }
    }
}

impl<T> IntoIterator for OneOrMany<T> {
    type Item = T;
    type IntoIter = OneOrManyIterator<T>;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            OneOrMany::One(v) => OneOrManyIterator::One(Some(v)),
            OneOrMany::Many(v) => OneOrManyIterator::Many(v.into_iter()),
        }
    }
}

/// Error type for log streamer
#[derive(Error, Debug)]
pub enum JsonDeserializeError {
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
    #[error("No more data")]
    NoMoreData,
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Timestamp};

    #[fuchsia::test]
    fn test_one_or_many() {
        let one: OneOrMany<u32> = serde_json::from_str("1").unwrap();
        assert_eq!(one, OneOrMany::One(1));
        let many: OneOrMany<u32> = serde_json::from_str("[1,2,3]").unwrap();
        assert_eq!(many, OneOrMany::Many(vec![1, 2, 3]));
    }

    const BOOT_TS: u64 = 98765432000000000;

    #[fuchsia::test]
    async fn test_json_decoder() {
        // This is intentionally a datagram socket so we can
        // guarantee torn writes and test all the code paths
        // in the decoder.
        let (local, remote) = fuchsia_zircon::Socket::create_datagram();
        let socket = fuchsia_async::Socket::from_socket(remote).unwrap();
        let mut decoder = LogsDataStream::new(socket);
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
        let part_c = &serialized_bytes[20..];
        local.write(part_a).unwrap();
        local.write(part_b).unwrap();
        local.write(part_c).unwrap();
        assert_eq!(&decoder.next().await.unwrap(), &test_log);
    }

    #[fuchsia::test]
    async fn test_json_decoder_regular_message() {
        // This is intentionally a datagram socket so we can
        // send the entire message as one "packet".
        let (local, remote) = fuchsia_zircon::Socket::create_datagram();
        let socket = fuchsia_async::Socket::from_socket(remote).unwrap();
        let mut decoder = LogsDataStream::new(socket);
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
        local.write(serialized_bytes).unwrap();
        assert_eq!(&decoder.next().await.unwrap(), &test_log);
    }

    #[fuchsia::test]
    async fn test_json_decoder_truncated_message() {
        // This is intentionally a datagram socket so we can
        // guarantee torn writes and test all the code paths
        // in the decoder.
        let (local, remote) = fuchsia_zircon::Socket::create_datagram();
        let socket = fuchsia_async::Socket::from_socket(remote).unwrap();
        let mut decoder = LogsDataStream::new(socket);
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
        assert_matches!(decoder.next().await, None);
    }
}
