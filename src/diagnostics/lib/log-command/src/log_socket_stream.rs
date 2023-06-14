// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::pin::Pin;

use async_stream::stream;
use diagnostics_data::LogsData;
use futures_util::{AsyncReadExt, Stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;

/// Read buffer size. Sufficiently large to store a large number
/// of messages to reduce the number of socket read calls we have
/// to make when reading messages.
const READ_BUFFER_SIZE: usize = 1000 * 1000 * 2;

/// Amount to increase the read buffer size by after
/// each read attempt.
const READ_BUFFER_INCREMENT: usize = 1000 * 256;

fn stream_raw_json<T, const BUFFER_SIZE: usize, const INC: usize>(
    mut socket: fuchsia_async::Socket,
) -> impl Stream<Item = OneOrMany<T>>
where
    T: DeserializeOwned,
{
    stream! {
        let mut buffer = vec![];
        buffer.resize(BUFFER_SIZE, 0);
        let mut write_offset = 0;
        let mut read_offset = 0;
        let mut available = 0;
        loop {
            // Read data from socket
            debug_assert!(write_offset <= buffer.len());
            if write_offset == buffer.len() {
                buffer.resize(buffer.len() + INC, 0);
            }
            let socket_bytes_read = socket.read(&mut buffer[write_offset..]).await.unwrap();
            if socket_bytes_read == 0 {
                break;
            }
            write_offset += socket_bytes_read;
            available += socket_bytes_read;
            let mut des = serde_json::Deserializer::from_slice(&buffer[read_offset..available])
                .into_iter();
            let mut read_nothing = true;
            while let Some(Ok(item)) = des.next() {
                read_nothing = false;
                yield item;
            }
            // Don't update the read offset if we haven't successfully
            // read anything.
            if read_nothing {
                continue;
            }
            let byte_offset = des.byte_offset();
            if byte_offset+read_offset == available {
                available = 0;
                write_offset = 0;
                read_offset = 0;
                buffer.resize(READ_BUFFER_SIZE, 0);
            } else {
                read_offset += byte_offset;
            }
        }
    }
}

/// Streams JSON logs from a socket
fn stream_json<T>(socket: fuchsia_async::Socket) -> impl Stream<Item = T>
where
    T: DeserializeOwned,
{
    stream_raw_json::<T, READ_BUFFER_SIZE, READ_BUFFER_INCREMENT>(socket)
        .map(|value| futures_util::stream::iter(value))
        .flatten()
}

/// Stream of JSON logs from the target device.
pub struct LogsDataStream {
    inner: Pin<Box<dyn Stream<Item = LogsData>>>,
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
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use futures_util::AsyncWriteExt;

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
    async fn test_json_decoder_large_message() {
        const MSG_COUNT: usize = 100;
        let (local, remote) = fuchsia_zircon::Socket::create_stream();
        let socket = fuchsia_async::Socket::from_socket(remote).unwrap();
        let mut decoder = Box::pin(
            stream_raw_json::<LogsData, 100, 10>(socket)
                .map(|value| futures_util::stream::iter(value))
                .flatten(),
        );
        let test_logs = (0..MSG_COUNT)
            .map(|value| {
                LogsDataBuilder::new(BuilderArgs {
                    component_url: None,
                    moniker: "ffx".to_string(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(BOOT_TS as i64),
                })
                .set_message(format!("Hello world! {}", value))
                .add_tag("Some tag")
                .build()
            })
            .collect::<Vec<_>>();
        let mut local = fuchsia_async::Socket::from_socket(local).unwrap();
        let test_logs_clone = test_logs.clone();
        let _write_task = fuchsia_async::Task::local(async move {
            for log in test_logs {
                let serialized_log = serde_json::to_string(&log).unwrap();
                let serialized_bytes = serialized_log.as_bytes();
                local.write_all(serialized_bytes).await.unwrap();
            }
        });
        for i in 0..MSG_COUNT {
            assert_eq!(&decoder.next().await.unwrap(), &test_logs_clone[i]);
        }
    }

    #[fuchsia::test]
    async fn test_json_decoder_large_single_message() {
        // At least 10MB of characters in a single message
        const CHAR_COUNT: usize = 1000 * 1000;
        let (local, remote) = fuchsia_zircon::Socket::create_stream();
        let socket = fuchsia_async::Socket::from_socket(remote).unwrap();
        let mut decoder = Box::pin(
            stream_raw_json::<LogsData, 256000, 20000>(socket)
                .map(|value| futures_util::stream::iter(value))
                .flatten(),
        );
        let test_log = LogsDataBuilder::new(BuilderArgs {
            component_url: None,
            moniker: "ffx".to_string(),
            severity: Severity::Info,
            timestamp_nanos: Timestamp::from(BOOT_TS as i64),
        })
        .set_message(format!("Hello world! {}", "h".repeat(CHAR_COUNT)))
        .add_tag("Some tag")
        .build();
        let mut local = fuchsia_async::Socket::from_socket(local).unwrap();
        let test_log_clone = test_log.clone();
        let _write_task = fuchsia_async::Task::local(async move {
            let serialized_log = serde_json::to_string(&test_log).unwrap();
            let serialized_bytes = serialized_log.as_bytes();
            local.write_all(serialized_bytes).await.unwrap();
        });
        assert_eq!(&decoder.next().await.unwrap(), &test_log_clone);
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
