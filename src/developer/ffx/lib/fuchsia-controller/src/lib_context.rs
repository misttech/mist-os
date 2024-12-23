// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::commands::LibraryCommand;
use crate::ext_buffer::ExtBuffer;
use anyhow::Result;
use async_lock::Mutex as AsyncMutex;
use fuchsia_async::{LocalExecutor, Task};
use std::ops::DerefMut;
use std::os::fd::{IntoRawFd, RawFd};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use zx_types;

type Notifier = Arc<AsyncMutex<Option<LibNotifier>>>;

pub struct LibContext {
    buf: Mutex<ExtBuffer<u8>>,
    notifier: Notifier,
    cmd_sender: async_channel::Sender<LibraryCommand>,
    thread_ctx: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl LibContext {
    pub(crate) fn new(buf: ExtBuffer<u8>) -> Self {
        let notifier = Notifier::default();
        let (cmd_sender, receiver) = async_channel::unbounded::<LibraryCommand>();
        Self {
            cmd_sender,
            buf: Mutex::new(buf),
            notifier: notifier.clone(),
            thread_ctx: Mutex::new(Some(new_command_thread(receiver, notifier))),
        }
    }

    pub(crate) fn write_err<T: std::fmt::Debug>(&self, err: T) {
        let error = format!("FFX Library Error: {err:?}");
        let mut guard = self.buf.lock().unwrap();
        let buf = guard.deref_mut();
        buf[..error.len()].clone_from_slice(error.as_bytes());
        buf[error.len()] = 0.into();
    }

    pub(crate) fn run(&self, cmd: LibraryCommand) {
        // Should not fail as this is an unbounded channel. In the future, when
        // updating to more recent versions of the async_channel library, this
        // can be handled using send_blocking instead.
        self.cmd_sender.try_send(cmd).expect("Sending to command channel");
    }

    pub(crate) async fn notifier_descriptor(&self) -> Result<RawFd> {
        let mut notifier = self.notifier.lock().await;
        if !notifier.is_some() {
            *notifier = Some(LibNotifier::new().await?);
        }
        Ok(notifier.as_ref().unwrap().receiver())
    }

    pub(crate) async fn notification_sender(
        &self,
    ) -> Option<async_channel::Sender<zx_types::zx_handle_t>> {
        self.notifier.lock().await.as_ref().map(|n| n.sender())
    }

    pub(crate) fn shutdown_cmd_thread(&self) {
        self.run(LibraryCommand::ShutdownLib);
        let thread =
            self.thread_ctx.lock().unwrap().take().expect("thread context must have been set");
        assert_ne!(
            std::thread::current().id(),
            thread.thread().id(),
            "thread is being dropped from inside itself"
        );
        thread.join().expect("joining thread");
    }
}

fn new_command_thread(
    receiver: async_channel::Receiver<LibraryCommand>,
    notifier: Notifier,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        let mut executor = LocalExecutor::new();
        executor.run_singlethreaded(async move {
            while let Ok(cmd) = receiver.recv().await {
                if let LibraryCommand::ShutdownLib = cmd {
                    // Dropping the notifier will cause spawned tasks to be dropped.
                    *notifier.lock().await = None;
                    break;
                }
                cmd.run().await;
            }
        });
    })
}

pub(crate) struct LibNotifier {
    _pipe_reader_task: Task<()>,
    handle_notification_sender: async_channel::Sender<zx_types::zx_handle_t>,
    stream_fd: RawFd,
}

impl LibNotifier {
    // This function isn't actually async, but it should be called inside an
    // executor to ensure spawned tasks are scheduled correctly.
    async fn new() -> Result<Self> {
        let (stream_rx, mut stream_tx) = UnixStream::pair()?;
        let (tx, rx) = async_channel::unbounded::<zx_types::zx_handle_t>();
        let pipe_reader_task = fuchsia_async::Task::local(async move {
            while let Ok(raw_handle) = rx.recv().await {
                match stream_tx.write_u32_le(raw_handle).await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::info!("Exiting pipe reader task. Error: {e:?}");
                        break;
                    }
                }
            }
        });
        Ok(Self {
            handle_notification_sender: tx,
            _pipe_reader_task: pipe_reader_task,
            stream_fd: stream_rx.into_std()?.into_raw_fd(),
        })
    }

    fn receiver(&self) -> RawFd {
        self.stream_fd
    }

    fn sender(&self) -> async_channel::Sender<zx_types::zx_handle_t> {
        self.handle_notification_sender.clone()
    }
}
