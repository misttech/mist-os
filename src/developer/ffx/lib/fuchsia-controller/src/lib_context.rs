// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::commands::LibraryCommand;
use crate::ext_buffer::ExtBuffer;
use anyhow::Result;
use async_lock::Mutex as AsyncMutex;
use byteorder::{NativeEndian, WriteBytesExt};
use errors::ffx_error;
use fuchsia_async::{LocalExecutor, Task};
use fuchsia_zircon_types as zx_types;
use futures_lite::AsyncWriteExt;
use std::ops::DerefMut;
use std::os::fd::{FromRawFd, RawFd};
use std::sync::Mutex;

pub struct LibContext {
    buf: Mutex<ExtBuffer<u8>>,
    thread_ctx: LibThreadHandle,
    notifier: AsyncMutex<Option<LibNotifier>>,
}

impl LibContext {
    pub fn new(buf: ExtBuffer<u8>) -> Self {
        Self {
            buf: Mutex::new(buf),
            thread_ctx: LibThreadHandle::new(),
            notifier: Default::default(),
        }
    }

    pub fn write_err<T: std::fmt::Debug>(&self, err: T) {
        let error = format!("FFX Library Error: {err:?}");
        let mut guard = self.buf.lock().unwrap();
        let buf = guard.deref_mut();
        buf[..error.len()].clone_from_slice(error.as_bytes());
        buf[error.len()] = 0.into();
    }

    pub fn run(&self, cmd: LibraryCommand) {
        self.thread_ctx.run(cmd)
    }

    pub async fn notifier_descriptor(&self) -> Result<RawFd> {
        let mut notifier = self.notifier.lock().await;
        if notifier.is_some() {
            return Err(ffx_error!(
                "Only one handle notification descriptor can be open at a time"
            )
            .into());
        }

        let (n, fd) = LibNotifier::new().await?;
        *notifier = Some(n);
        Ok(fd)
    }

    pub async fn notification_sender(
        &self,
    ) -> Option<async_channel::Sender<zx_types::zx_handle_t>> {
        self.notifier.lock().await.as_ref().map(|n| n.sender())
    }
}

struct LibThreadHandle {
    join_hdl: Option<std::thread::JoinHandle<()>>,
    sender: async_channel::Sender<LibraryCommand>,
}

impl LibThreadHandle {
    fn new() -> Self {
        let (sender, receiver) = async_channel::unbounded::<LibraryCommand>();
        let join_hdl = std::thread::spawn(|| {
            let mut executor = LocalExecutor::new();
            executor.run_singlethreaded(async move {
                while let Ok(cmd) = receiver.recv().await {
                    if matches!(cmd, LibraryCommand::ShutdownLib) {
                        break;
                    }
                    cmd.run().await;
                }
            });
        });
        Self { join_hdl: Some(join_hdl), sender }
    }

    fn run(&self, cmd: LibraryCommand) {
        // Should not fail as this is an unbounded channel. In the future, when
        // updating to more recent versions of the async_channel library, this
        // can be handled using send_blocking instead.
        self.sender.try_send(cmd).expect("Sending to command channel");
    }
}

impl Drop for LibThreadHandle {
    fn drop(&mut self) {
        self.sender.try_send(LibraryCommand::ShutdownLib).unwrap();
        self.join_hdl.take().expect("should only drop once").join().expect("joining channel");
    }
}

struct LibNotifier {
    _pipe_reader_task: Task<()>,
    handle_notification_sender: async_channel::Sender<zx_types::zx_handle_t>,
}

fn unix_stream(fd: RawFd) -> Result<async_net::unix::UnixStream, std::io::Error> {
    let owned_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    async_net::unix::UnixStream::try_from(owned_fd)
}

impl LibNotifier {
    // This function isn't actually async, but it should be called inside an
    // executor to ensure spawned tasks are scheduled correctly.
    async fn new() -> Result<(Self, RawFd)> {
        let (pipe_rx, pipe_tx) = nix::unistd::pipe()?;
        let mut stream = unix_stream(pipe_tx)?;
        let (tx, rx) = async_channel::unbounded::<zx_types::zx_handle_t>();
        let _pipe_reader_task = fuchsia_async::Task::local(async move {
            let mut bytes: [u8; 4] = [0, 0, 0, 0];
            while let Ok(raw_handle) = rx.recv().await {
                bytes.as_mut().write_u32::<NativeEndian>(raw_handle).unwrap();
                match stream.write_all(&bytes).await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::info!("Exiting pipe reader task. Error: {e:?}");
                        break;
                    }
                }
            }
        });
        Ok((Self { handle_notification_sender: tx, _pipe_reader_task }, pipe_rx))
    }

    fn sender(&self) -> async_channel::Sender<zx_types::zx_handle_t> {
        self.handle_notification_sender.clone()
    }
}
