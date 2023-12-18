// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{task::CurrentTask, vfs::OutputBuffer};
use diagnostics_data::{Data, Logs, Severity};
use diagnostics_reader::ArchiveReader;
use fidl_fuchsia_diagnostics as fdiagnostics;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_zircon as zx;
use futures::{channel::mpsc, SinkExt, StreamExt};
use once_cell::sync::OnceCell;
use serde::Deserialize;
use starnix_logging::not_implemented;
use starnix_sync::Mutex;
use starnix_uapi::{
    auth::{CAP_SYSLOG, CAP_SYS_ADMIN},
    errors::{errno, error, Errno},
};
use std::{
    cmp,
    collections::VecDeque,
    io::{self, Write},
    time::Duration,
};

const BUFFER_SIZE: i32 = 1_049_000;

#[derive(Default)]
pub struct Syslog {
    syscall_subscription: OnceCell<Mutex<LogSubscription>>,
}

impl Syslog {
    pub fn init(&self, system_task: &CurrentTask) -> Result<(), anyhow::Error> {
        let subscription = LogSubscription::snapshot_then_subscribe(system_task, false)?;
        self.syscall_subscription.set(Mutex::new(subscription)).expect("syslog.set called once");
        Ok(())
    }

    pub fn read(
        &self,
        current_task: &CurrentTask,
        out: &mut dyn OutputBuffer,
    ) -> Result<i32, Errno> {
        Self::check_credentials(&current_task)?;
        let mut subscription = self.syscall_subscription.get().expect("syslog initialized").lock();
        if let Some(log) = subscription.try_next()? {
            let size_to_write = cmp::min(log.len(), out.available() as usize);
            out.write(&log[..size_to_write])?;
            return Ok(size_to_write as i32);
        }
        Ok(0)
    }

    pub fn read_all(&self, out: &mut dyn OutputBuffer) -> Result<i32, Errno> {
        let mut subscription = LogSubscription::snapshot()?;
        let mut buffer = ResultBuffer::new(out.available());
        while let Some(log_result) = subscription.next() {
            buffer.push(log_result?);
        }
        let result: Vec<u8> = buffer.into();
        out.write(result.as_slice())?;
        Ok(result.len() as i32)
    }

    pub fn size_unread(&self, current_task: &CurrentTask) -> Result<i32, Errno> {
        Self::check_credentials(&current_task)?;
        not_implemented!("syslog: size_unread");
        Ok(0)
    }

    pub fn size_buffer(&self) -> Result<i32, Errno> {
        // For now always return a constant for this.
        Ok(BUFFER_SIZE)
    }

    pub fn snapshot_then_subscribe(
        &self,
        current_task: &CurrentTask,
        non_blocking: bool,
    ) -> Result<LogSubscription, Errno> {
        LogSubscription::snapshot_then_subscribe(current_task, non_blocking)
    }

    fn check_credentials(current_task: &CurrentTask) -> Result<(), Errno> {
        let credentials = current_task.creds();
        if credentials.has_capability(CAP_SYSLOG) || credentials.has_capability(CAP_SYS_ADMIN) {
            return Ok(());
        }
        error!(EPERM)
    }
}

#[derive(Debug)]
pub enum LogSubscription {
    Blocking {
        iterator: fdiagnostics::BatchIteratorSynchronousProxy,
        pending: VecDeque<Data<Logs>>,
    },
    NonBlocking(mpsc::Receiver<Result<Vec<u8>, Errno>>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
}

impl LogSubscription {
    fn snapshot() -> Result<Self, Errno> {
        Self::blocking(fdiagnostics::StreamMode::Snapshot)
    }

    fn snapshot_then_subscribe(
        current_task: &CurrentTask,
        non_blocking: bool,
    ) -> Result<Self, Errno> {
        if non_blocking {
            let (mut snd, rcv) = mpsc::channel(1);
            current_task.kernel().kthreads.spawner().spawn(move |_, _| {
                let mut executor = fasync::LocalExecutor::new();
                executor.run_singlethreaded(async {
                    let reader = ArchiveReader::new();
                    let (mut logs, _) =
                        reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
                    while let Some(data) = logs.next().await {
                        if snd.is_closed() {
                            return;
                        }
                        match format_log(data) {
                            Ok(Some(log)) => {
                                let _ = snd.send(Ok(log)).await;
                            }
                            Err(_err) => {
                                let _ = snd.send(Err(errno!(EIO))).await;
                            }
                            Ok(None) => {}
                        }
                    }
                });
            });
            Ok(Self::NonBlocking(rcv))
        } else {
            Self::blocking(fdiagnostics::StreamMode::SnapshotThenSubscribe)
        }
    }

    fn blocking(mode: fdiagnostics::StreamMode) -> Result<Self, Errno> {
        let accessor = connect_to_protocol_sync::<fdiagnostics::ArchiveAccessorMarker>()
            .map_err(|_| errno!(ENOENT, format!("Failed to connecto to ArchiveAccessor")))?;
        let stream_parameters = fdiagnostics::StreamParameters {
            stream_mode: Some(mode),
            data_type: Some(fdiagnostics::DataType::Logs),
            format: Some(fdiagnostics::Format::Json),
            client_selector_configuration: Some(
                fdiagnostics::ClientSelectorConfiguration::SelectAll(true),
            ),
            ..fdiagnostics::StreamParameters::default()
        };
        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fdiagnostics::BatchIteratorMarker>();
        accessor.stream_diagnostics(&stream_parameters, server_end).map_err(|err| {
            errno!(EIO, format!("ArchiveAccessor/StreamDiagnostics failed: {err}"))
        })?;
        let iterator = fdiagnostics::BatchIteratorSynchronousProxy::new(client_end.into_channel());
        Ok(Self::Blocking { iterator, pending: VecDeque::new() })
    }

    // TODO(b/315520045): implement and use a more efficient approach for fetching logs that
    // doesn't involve having to parse JSON...
    fn try_next(&mut self) -> Result<Option<Vec<u8>>, Errno> {
        match self {
            Self::NonBlocking(receiver) => match receiver.try_next() {
                // We got the next log.
                Ok(Some(Ok(log))) => Ok(Some(log)),
                // An error happened attempting to get the next log.
                Ok(Some(Err(err))) => Err(err),
                // The channel was closed and there's no more messages in the queue.
                Ok(None) => Ok(None),
                // No messages available but the channel hasn't closed.
                Err(_) => Err(errno!(EAGAIN)),
            },
            Self::Blocking { iterator, pending } => loop {
                while let Some(data) = pending.pop_front() {
                    if let Some(log) = format_log(data).map_err(|_| errno!(EIO))? {
                        return Ok(Some(log));
                    }
                }
                let next_batch = iterator
                    .get_next(zx::Time::INFINITE)
                    .map_err(|_| errno!(ENOENT))?
                    .map_err(|_| errno!(ENOENT))?;
                if next_batch.is_empty() {
                    return Ok(None);
                }
                for formatted_content in next_batch {
                    let output: OneOrMany<Data<Logs>> = match formatted_content {
                        fdiagnostics::FormattedContent::Json(data) => {
                            let mut buf = vec![0; data.size as usize];
                            data.vmo.read(&mut buf, 0).map_err(|err| {
                                errno!(EIO, format!("failed to read logs vmo: {err}"))
                            })?;
                            serde_json::from_slice(&buf).map_err(|_| {
                                errno!(EIO, format!("archivist returned invalid data"))
                            })?
                        }
                        format => {
                            unreachable!("we only request and expect one format. Got: {format:?}")
                        }
                    };
                    match output {
                        OneOrMany::One(data) => pending.push_back(data),
                        OneOrMany::Many(datas) => pending.extend(datas.into_iter()),
                    }
                }
            },
        }
    }
}

impl Iterator for LogSubscription {
    type Item = Result<Vec<u8>, Errno>;

    fn next(&mut self) -> Option<Result<Vec<u8>, Errno>> {
        self.try_next().transpose()
    }
}

struct ResultBuffer {
    max_size: usize,
    buffer: VecDeque<Vec<u8>>,
    current_size: usize,
}

impl ResultBuffer {
    fn new(max_size: usize) -> Self {
        Self { max_size, buffer: VecDeque::default(), current_size: 0 }
    }

    fn push(&mut self, data: Vec<u8>) {
        while !self.buffer.is_empty() && self.current_size + data.len() > self.max_size {
            let old = self.buffer.pop_front().unwrap();
            self.current_size -= old.len();
        }
        self.current_size += data.len();
        self.buffer.push_back(data);
    }
}

impl Into<Vec<u8>> for ResultBuffer {
    fn into(self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.current_size);
        for mut item in self.buffer {
            result.append(&mut item);
        }
        // If we still exceed the size (for example, a single message of size N in a buffer of
        // size M when N>M), we trim the output.
        let size = std::cmp::min(result.len(), std::cmp::min(self.max_size, self.current_size));
        if result.len() != size {
            result.resize(size, 0);
        }
        result
    }
}

fn format_log(data: Data<Logs>) -> Result<Option<Vec<u8>>, io::Error> {
    if let Some(msg) = data.msg() {
        if msg.contains("DBG:") {
            return Ok(None);
        }
    }
    let mut formatted_tags = match data.tags() {
        None => vec![],
        Some(tags) => {
            let mut formatted = vec![];
            for (i, tag) in tags.iter().enumerate() {
                // TODO(b/299533466): remove this.
                if tag.contains("fxlogcat") {
                    return Ok(None);
                }
                if i != 0 {
                    write!(&mut formatted, ",")?;
                }
                write!(&mut formatted, "{tag}")?;
            }
            write!(&mut formatted, ": ")?;
            formatted
        }
    };

    let mut result = Vec::<u8>::new();
    let level = match data.severity() {
        Severity::Trace | Severity::Debug => 7,
        Severity::Info => 6,
        Severity::Warn => 4,
        Severity::Error => 3,
        Severity::Fatal => 2,
    };
    let time: Duration = Duration::from_nanos(data.metadata.timestamp as u64);
    let component_name = data.component_name();
    let time_secs = time.as_secs();
    let time_fract = time.as_micros() % 1_000_000;
    write!(&mut result, "<{level}>[{time_secs:05}.{time_fract:06}] {component_name}",)?;

    match data.metadata.pid {
        Some(pid) => write!(&mut result, "[{pid}]: ")?,
        None => write!(&mut result, ": ")?,
    }

    result.append(&mut formatted_tags);

    if let Some(msg) = data.msg() {
        write!(&mut result, "{msg}")?;
    }

    for kvp in data.payload_keys_strings() {
        write!(&mut result, " {kvp}")?;
    }
    write!(&mut result, "\n")?;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_result_buffer() {
        let mut buffer = ResultBuffer::new(100);
        buffer.push(vec![0; 200]);
        let result: Vec<u8> = buffer.into();
        assert_eq!(result.len(), 100);

        let mut buffer = ResultBuffer::new(100);
        buffer.push(Vec::from_iter(0..20));
        buffer.push(Vec::from_iter(20..50));
        let result: Vec<u8> = buffer.into();
        assert_eq!(result.len(), 50);
        for i in 0..50u8 {
            assert_eq!(result[i as usize], i);
        }
    }
}
