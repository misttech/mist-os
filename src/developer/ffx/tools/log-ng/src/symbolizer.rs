// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::symbolizer_impl::{is_symbolizer_context_marker, Symbolizer};
use anyhow::Result;
use async_trait::async_trait;
use log_command::log_formatter;
use log_formatter::{LogData, LogEntry, Symbolize};
use std::fmt::Debug;
use thiserror::Error;

use crate::error::LogError;

/// Connection to a symbolizer.
#[derive(Debug)]
pub struct SymbolizerChannel<T>
where
    T: Symbolizer + Debug,
{
    sender: async_channel::Sender<String>,
    receiver: async_channel::Receiver<String>,
    _symbolizer: T,
}

#[async_trait(?Send)]
impl<T> Symbolize for SymbolizerChannel<T>
where
    T: Symbolizer + Debug,
{
    async fn symbolize(&self, entry: LogEntry) -> LogEntry {
        self.symbolize_message(entry).await
    }
}

#[derive(Error, Debug)]
enum SymbolizerError {
    #[error("No context marker found in message")]
    NoContextMarker,
    #[error(transparent)]
    AsyncChannelWriteError(#[from] async_channel::SendError<String>),
    #[error(transparent)]
    RecvError(#[from] async_channel::RecvError),
}

impl<T> SymbolizerChannel<T>
where
    T: Symbolizer + Debug,
{
    /// Constructs a SymbolizerChannel from the given symbolizer
    /// and starts the symbolizer process, opening a channel for communication.
    pub async fn new(symbolizer: T) -> Result<SymbolizerChannel<T>, LogError> {
        let (sender_tx, sender_rx) = async_channel::bounded(1);
        let (reader_tx, reader_rx) = async_channel::bounded(1);
        symbolizer.start(sender_rx, reader_tx, vec![]).await?;
        Ok(SymbolizerChannel { _symbolizer: symbolizer, sender: sender_tx, receiver: reader_rx })
    }

    /// Symbolizes the message if a target log, or returns the entry
    /// if it cannot be symbolized.
    async fn symbolize_message(&self, log: LogEntry) -> LogEntry
    where
        T: Symbolizer + Debug,
    {
        let LogEntry { timestamp, data } = log;
        let data = match data {
            LogData::TargetLog(data) => data,
            _ => return LogEntry { timestamp, data },
        };
        let msg = data.msg().unwrap_or("");
        let symbolized_msg = match self.execute_symbolization(msg).await {
            Ok(msg) => msg,
            Err(err) => {
                tracing::error!(%err, "Failed to symbolize message");
                return LogEntry { timestamp, data: LogData::TargetLog(data) };
            }
        };
        LogEntry { timestamp, data: LogData::SymbolizedTargetLog(data, symbolized_msg) }
    }

    async fn execute_symbolization(&self, msg: &str) -> Result<String, SymbolizerError> {
        if !is_symbolizer_context_marker(msg) {
            return Err(SymbolizerError::NoContextMarker);
        }
        let msg = format!("{msg}\n");
        self.sender.send(msg).await?;
        let out = self.receiver.recv().await?;
        // Reconstruct the message by dropping the \n we added above.
        let mut split: Vec<&str> = out.split('\n').collect();
        if split.len() > 1 {
            // Remove the empty newline we added above so we don't have an unnecessary
            // newline after each symbolized message.
            split.remove(split.len() - 1);
        }

        return Ok(split.join("\n"));
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use crate::symbolizer_impl::FakeSymbolizerForTest;
    use assert_matches::assert_matches;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
    use futures::StreamExt;

    use super::*;

    const LOG_PREFIX: &str = "cool-logger";

    /// Half-open symbolizer, where stdout from the remote process
    /// hung up but stdin is still open.
    #[derive(Debug)]
    struct HalfOpenSymbolizer {
        rx: Rc<RefCell<Option<async_channel::Receiver<String>>>>,
        _task: RefCell<Option<fuchsia_async::Task<()>>>,
    }

    impl HalfOpenSymbolizer {
        fn new(_prefix: &str, _expected_args: Vec<String>) -> Self {
            Self { rx: Rc::new(RefCell::new(None)), _task: RefCell::new(None) }
        }
    }

    #[async_trait(?Send)]
    impl Symbolizer for HalfOpenSymbolizer {
        async fn start(
            &self,
            rx: async_channel::Receiver<String>,
            _tx: async_channel::Sender<String>,
            _extra_args: Vec<String>,
        ) -> anyhow::Result<()> {
            *self.rx.borrow_mut() = Some(rx);
            let rx = self.rx.clone();
            *self._task.borrow_mut() = Some(fuchsia_async::Task::local(async move {
                // Discard all data.
                while let Some(_) = rx.borrow_mut().as_mut().unwrap().next().await {}
            }));
            Ok(())
        }
    }

    /// Symbolizer which immediately closes its channel
    #[derive(Debug)]
    struct ClosedSymbolizer;

    #[async_trait(?Send)]
    impl Symbolizer for ClosedSymbolizer {
        async fn start(
            &self,
            _rx: async_channel::Receiver<String>,
            _tx: async_channel::Sender<String>,
            _extra_args: Vec<String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Broken symbolizer which always returns an error on start
    #[derive(Debug)]
    struct BrokenSymbolizer;

    #[async_trait(?Send)]
    impl Symbolizer for BrokenSymbolizer {
        async fn start(
            &self,
            _rx: async_channel::Receiver<String>,
            _tx: async_channel::Sender<String>,
            _extra_args: Vec<String>,
        ) -> anyhow::Result<()> {
            Err(LogError::NoSymbolizerConfig)?
        }
    }

    fn make_log_entry(message: impl Into<String>) -> LogEntry {
        LogEntry {
            data: LogData::TargetLog(
                LogsDataBuilder::new(BuilderArgs {
                    component_url: Some("ffx".to_string()),
                    moniker: "ffx".to_string(),
                    severity: Severity::Info,
                    timestamp_nanos: Timestamp::from(0),
                })
                .set_message(message)
                .build(),
            ),
            timestamp: Timestamp::from(0),
        }
    }

    #[fuchsia::test]
    async fn symbolizer_replaces_markers_with_symbolized_logs() {
        let symbolizer =
            SymbolizerChannel::new(FakeSymbolizerForTest::new(LOG_PREFIX, vec![])).await.unwrap();
        let log = symbolizer.symbolize(make_log_entry("{{{reset}}}\n".to_string())).await;
        let (_, out) = log.data.as_symbolized_log().unwrap();
        assert_eq!(out, "cool-logger{{{reset}}}\n");

        let log = symbolizer.symbolize(make_log_entry("{{{mmap:something}}\n".to_string())).await;
        let (_, out) = log.data.as_symbolized_log().unwrap();

        assert_eq!(out, "cool-logger{{{mmap:something}}\n");

        let out = symbolizer
            .symbolize(make_log_entry("not_real\n".to_string()))
            .await
            .data
            .as_target_log()
            .unwrap()
            .msg()
            .unwrap()
            .to_string();
        assert_eq!(out, "not_real\n");
    }

    #[fuchsia::test]
    async fn symbolizer_returns_error_if_start_fails() {
        let config = BrokenSymbolizer {};
        assert_matches!(SymbolizerChannel::new(config).await, Err(LogError::UnknownError(_)));
    }

    #[fuchsia::test]
    async fn symbolizer_does_nothing_if_process_exits() {
        let config = ClosedSymbolizer {};
        let symbolizer = SymbolizerChannel::new(config).await.unwrap();
        let out = symbolizer
            .symbolize(make_log_entry("{{{reset}}}\n".to_string()))
            .await
            .data
            .as_target_log()
            .unwrap()
            .msg()
            .unwrap()
            .to_string();
        assert_eq!(out, "{{{reset}}}\n");

        let out = symbolizer
            .symbolize(make_log_entry("{{{mmap:something}}\n".to_string()))
            .await
            .data
            .as_target_log()
            .unwrap()
            .msg()
            .unwrap()
            .to_string();

        assert_eq!(out, "{{{mmap:something}}\n");

        let out = symbolizer
            .symbolize(make_log_entry("not_real\n".to_string()))
            .await
            .data
            .as_target_log()
            .unwrap()
            .msg()
            .unwrap()
            .to_string();
        assert_eq!(out, "not_real\n");
    }

    #[fuchsia::test]
    async fn symbolizer_does_nothing_if_stdout_hangs_up() {
        let config = HalfOpenSymbolizer::new(LOG_PREFIX, vec![]);

        let symbolizer = SymbolizerChannel::new(config).await.unwrap();
        let out = symbolizer
            .symbolize(make_log_entry("{{{reset}}}\n".to_string()))
            .await
            .data
            .as_target_log()
            .unwrap()
            .msg()
            .unwrap()
            .to_string();
        assert_eq!(out, "{{{reset}}}\n");

        let out = symbolizer
            .symbolize(make_log_entry("{{{mmap:something}}\n".to_string()))
            .await
            .data
            .as_target_log()
            .unwrap()
            .msg()
            .unwrap()
            .to_string();

        assert_eq!(out, "{{{mmap:something}}\n");

        let out = symbolizer
            .symbolize(make_log_entry("not_real\n".to_string()))
            .await
            .data
            .as_target_log()
            .unwrap()
            .msg()
            .unwrap()
            .to_string();
        assert_eq!(out, "not_real\n");
    }
}
