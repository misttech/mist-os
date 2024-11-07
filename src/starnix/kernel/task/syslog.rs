// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, EventHandler, WaitCallback, WaitCanceler, WaitQueue, Waiter};
use crate::vfs::OutputBuffer;
use diagnostics_data::{Data, Logs, Severity};
use fidl_fuchsia_diagnostics as fdiagnostics;
use fuchsia_component::client::connect_to_protocol_sync;
use once_cell::sync::OnceCell;
use serde::Deserialize;
use starnix_sync::{Locked, Mutex, Unlocked};
use starnix_uapi::auth::{CAP_SYSLOG, CAP_SYS_ADMIN};
use starnix_uapi::errors::{errno, error, Errno, EAGAIN};
use starnix_uapi::vfs::FdEvents;
use std::cmp;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{mpsc, Arc};

const BUFFER_SIZE: i32 = 1_049_000;

#[derive(Default)]
pub struct Syslog {
    syscall_subscription: OnceCell<Mutex<LogSubscription>>,
}

impl Syslog {
    pub fn init(&self, system_task: &CurrentTask) -> Result<(), anyhow::Error> {
        let subscription = LogSubscription::snapshot_then_subscribe(system_task)?;
        self.syscall_subscription.set(Mutex::new(subscription)).expect("syslog inititialized once");
        Ok(())
    }

    // TODO(b/316630310): for now, all actions on the syslog are privileged.
    // READ_ALL and SIZE_BUFFER should be granted unprivileged access if
    // `/proc/sys/kernel/dmesg_restrict` is 0, which also allows to read /dev/kmsg.
    pub fn access(&self, current_task: &CurrentTask) -> Result<GrantedSyslog<'_>, Errno> {
        Self::validate_access(current_task)?;
        let syscall_subscription = self.subscription()?;
        Ok(GrantedSyslog { syscall_subscription })
    }

    pub fn validate_access(current_task: &CurrentTask) -> Result<(), Errno> {
        let credentials = current_task.creds();
        if credentials.has_capability(CAP_SYSLOG) || credentials.has_capability(CAP_SYS_ADMIN) {
            return Ok(());
        }
        error!(EPERM)
    }

    pub fn snapshot_then_subscribe(current_task: &CurrentTask) -> Result<LogSubscription, Errno> {
        LogSubscription::snapshot_then_subscribe(current_task)
    }

    pub fn subscribe(current_task: &CurrentTask) -> Result<LogSubscription, Errno> {
        LogSubscription::subscribe(current_task)
    }

    fn subscription(&self) -> Result<&Mutex<LogSubscription>, Errno> {
        self.syscall_subscription.get().ok_or_else(|| errno!(ENOENT))
    }
}

pub struct GrantedSyslog<'a> {
    syscall_subscription: &'a Mutex<LogSubscription>,
}

impl GrantedSyslog<'_> {
    pub fn read(&self, out: &mut dyn OutputBuffer) -> Result<i32, Errno> {
        let mut subscription = self.syscall_subscription.lock();
        if let Some(log) = subscription.try_next()? {
            let size_to_write = cmp::min(log.len(), out.available() as usize);
            out.write(&log[..size_to_write])?;
            return Ok(size_to_write as i32);
        }
        Ok(0)
    }

    pub fn wait(&self, waiter: &Waiter, events: FdEvents, handler: EventHandler) -> WaitCanceler {
        self.syscall_subscription.lock().wait(waiter, events, handler)
    }

    pub fn blocking_read(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        out: &mut dyn OutputBuffer,
    ) -> Result<i32, Errno> {
        let mut subscription = self.syscall_subscription.lock();
        let mut write_log = |log: Vec<u8>| {
            let size_to_write = cmp::min(log.len(), out.available() as usize);
            out.write(&log[..size_to_write])?;
            Ok(size_to_write as i32)
        };
        match subscription.try_next() {
            Err(errno) if errno == EAGAIN => {}
            Err(errno) => return Err(errno),
            Ok(Some(log)) => return write_log(log),
            Ok(None) => return Ok(0),
        }
        let waiter = Waiter::new();
        loop {
            let _w = subscription.wait(
                &waiter,
                FdEvents::POLLIN | FdEvents::POLLHUP,
                WaitCallback::none(),
            );
            match subscription.try_next() {
                Err(errno) if errno == EAGAIN => {}
                Err(errno) => return Err(errno),
                Ok(Some(log)) => return write_log(log),
                Ok(None) => return Ok(0),
            }
            waiter.wait(locked, current_task)?;
        }
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

    pub fn size_unread(&self) -> Result<i32, Errno> {
        let mut subscription = self.syscall_subscription.lock();
        Ok(subscription.available()?.try_into().unwrap_or(std::i32::MAX))
    }

    pub fn size_buffer(&self) -> Result<i32, Errno> {
        // For now always return a constant for this.
        Ok(BUFFER_SIZE)
    }
}

#[derive(Debug)]
pub struct LogSubscription {
    pending: Option<Vec<u8>>,
    receiver: mpsc::Receiver<Result<Vec<u8>, Errno>>,
    waiters: Arc<WaitQueue>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
}

impl LogSubscription {
    pub fn wait(&self, waiter: &Waiter, events: FdEvents, handler: EventHandler) -> WaitCanceler {
        self.waiters.wait_async_fd_events(waiter, events, handler)
    }

    pub fn available(&mut self) -> Result<usize, Errno> {
        if let Some(log) = &self.pending {
            return Ok(log.len());
        }
        match self.try_next() {
            Err(err) if err == EAGAIN => Ok(0),
            Err(err) => Err(err),
            Ok(Some(log)) => {
                let size = log.len();
                self.pending.replace(log);
                return Ok(size);
            }
            Ok(None) => Ok(0),
        }
    }

    fn snapshot() -> Result<LogIterator, Errno> {
        LogIterator::new(fdiagnostics::StreamMode::Snapshot)
    }

    fn subscribe(current_task: &CurrentTask) -> Result<Self, Errno> {
        Self::new_listening(current_task, fdiagnostics::StreamMode::Subscribe)
    }

    fn snapshot_then_subscribe(current_task: &CurrentTask) -> Result<Self, Errno> {
        Self::new_listening(current_task, fdiagnostics::StreamMode::SnapshotThenSubscribe)
    }

    fn new_listening(
        current_task: &CurrentTask,
        mode: fdiagnostics::StreamMode,
    ) -> Result<Self, Errno> {
        let iterator = LogIterator::new(mode)?;
        let (snd, receiver) = mpsc::sync_channel(1);
        let waiters = Arc::new(WaitQueue::default());
        let waiters_clone = waiters.clone();
        current_task.kernel().kthreads.spawner().spawn(move |_, _| {
            scopeguard::defer! {
                waiters_clone.notify_fd_events(FdEvents::POLLHUP);
            };
            for log in iterator {
                if snd.send(log).is_err() {
                    break;
                };
                waiters_clone.notify_fd_events(FdEvents::POLLIN);
            }
        });

        Ok(Self { receiver, waiters, pending: Default::default() })
    }

    fn try_next(&mut self) -> Result<Option<Vec<u8>>, Errno> {
        if let Some(value) = self.pending.take() {
            return Ok(Some(value));
        }
        match self.receiver.try_recv() {
            // We got the next log.
            Ok(Ok(log)) => Ok(Some(log)),
            // An error happened attempting to get the next log.
            Ok(Err(err)) => Err(err),
            // The channel was closed and there's no more messages in the queue.
            Err(mpsc::TryRecvError::Disconnected) => Ok(None),
            // No messages available but the channel hasn't closed.
            Err(mpsc::TryRecvError::Empty) => Err(errno!(EAGAIN)),
        }
    }
}

struct LogIterator {
    iterator: fdiagnostics::BatchIteratorSynchronousProxy,
    pending_formatted_contents: VecDeque<fdiagnostics::FormattedContent>,
    pending_datas: VecDeque<Data<Logs>>,
}

impl LogIterator {
    fn new(mode: fdiagnostics::StreamMode) -> Result<Self, Errno> {
        let accessor = connect_to_protocol_sync::<fdiagnostics::ArchiveAccessorMarker>()
            .map_err(|_| errno!(ENOENT, format!("Failed to connecto to ArchiveAccessor")))?;
        let is_subscribe = matches!(mode, fdiagnostics::StreamMode::Subscribe);
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
        if is_subscribe {
            let () = iterator.wait_for_ready(zx::MonotonicInstant::INFINITE).map_err(|err| {
                errno!(EIO, format!("Failed to wait for BatchIterator being ready: {err}"))
            })?;
        }
        Ok(Self {
            iterator,
            pending_formatted_contents: VecDeque::new(),
            pending_datas: VecDeque::new(),
        })
    }

    // TODO(b/315520045): implement and use a more efficient approach for fetching logs that
    // doesn't involve having to parse JSON...
    fn get_next(&mut self) -> Result<Option<Vec<u8>>, Errno> {
        'main_loop: loop {
            while let Some(data) = self.pending_datas.pop_front() {
                if let Some(log) = format_log(data).map_err(|_| errno!(EIO))? {
                    return Ok(Some(log));
                }
            }
            while let Some(formatted_content) = self.pending_formatted_contents.pop_front() {
                let output: OneOrMany<Data<Logs>> = match formatted_content {
                    fdiagnostics::FormattedContent::Json(data) => {
                        let buf = data.vmo.read_to_vec(0, data.size).map_err(|err| {
                            errno!(EIO, format!("failed to read logs vmo: {err}"))
                        })?;
                        serde_json::from_slice(&buf)
                            .map_err(|_| errno!(EIO, format!("archivist returned invalid data")))?
                    }
                    format => {
                        unreachable!("we only request and expect one format. Got: {format:?}")
                    }
                };
                match output {
                    OneOrMany::One(data) => {
                        if let Some(log) = format_log(data).map_err(|_| errno!(EIO))? {
                            return Ok(Some(log));
                        }
                    }
                    OneOrMany::Many(datas) => {
                        if datas.len() > 0 {
                            self.pending_datas.extend(datas);
                            continue 'main_loop;
                        }
                    }
                }
            }
            let next_batch = self
                .iterator
                .get_next(zx::MonotonicInstant::INFINITE)
                .map_err(|_| errno!(ENOENT))?
                .map_err(|_| errno!(ENOENT))?;
            if next_batch.is_empty() {
                return Ok(None);
            }
            self.pending_formatted_contents = VecDeque::from(next_batch);
        }
    }
}

impl Iterator for LogIterator {
    type Item = Result<Vec<u8>, Errno>;

    fn next(&mut self) -> Option<Result<Vec<u8>, Errno>> {
        self.get_next().transpose()
    }
}

impl Iterator for LogSubscription {
    type Item = Result<Vec<u8>, Errno>;

    fn next(&mut self) -> Option<Self::Item> {
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
    let time = zx::MonotonicDuration::from_nanos(data.metadata.timestamp.into_nanos());
    let component_name = data.component_name();
    let time_secs = time.into_seconds();
    let time_fract = time.into_micros() % 1_000_000;
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
