// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use async_utils::hanging_get::client::HangingGetStream;
use fidl_fuchsia_bluetooth_sys::{HostInfo, HostWatcherMarker, HostWatcherProxy};
use fuchsia_async::{LocalExecutor, TimeoutExt};
use fuchsia_component::client::connect_to_protocol;
use fuchsia_sync::RwLock;
use futures::channel::{mpsc, oneshot};
use futures::executor::block_on;
use futures::{SinkExt, StreamExt, TryFutureExt};
use std::sync::LazyLock;
use std::thread;
use zx::sys::zx_status_t;

enum Request {
    ReadLocalAddress(oneshot::Sender<Result<[u8; 6], anyhow::Error>>),
    Stop,
}

struct WorkThread {
    handle: RwLock<Option<thread::JoinHandle<Result<(), anyhow::Error>>>>,
    sender: RwLock<Option<mpsc::UnboundedSender<Request>>>,
}

impl WorkThread {
    const fn new() -> WorkThread {
        WorkThread { handle: RwLock::new(None), sender: RwLock::new(None) }
    }

    fn spawn(&self) {
        let (sender, mut receiver) = mpsc::unbounded::<Request>();
        *self.sender.write() = Some(sender);

        *self.handle.write() = Some(thread::spawn(move || {
            LocalExecutor::new().run_singlethreaded(async {
                let mut host_watcher_stream = HangingGetStream::new_with_fn_ptr(
                    connect_to_protocol::<HostWatcherMarker>()?,
                    HostWatcherProxy::watch,
                );
                let mut host_cache: Vec<HostInfo> = Vec::new();

                while let Some(request) = receiver.next().await {
                    match request {
                        Request::ReadLocalAddress(sender) => {
                            sender
                                .send(
                                    get_active_host(&mut host_cache, &mut host_watcher_stream)
                                        .map_ok(|host| {
                                            host.addresses
                                                .clone()
                                                .unwrap()
                                                .first()
                                                .expect("Host has no address")
                                                .bytes
                                        })
                                        .await,
                                )
                                .expect("Failed to send");
                        }
                        Request::Stop => {
                            break;
                        }
                    }
                }

                Ok::<(), anyhow::Error>(())
            })?;

            Ok(())
        }));
    }

    fn join(&self) -> zx_status_t {
        if self.handle.read().is_none() {
            return zx::Status::BAD_STATE.into_raw();
        }
        block_on(self.sender.write().as_mut().unwrap().send(Request::Stop))
            .expect("Failed to send");
        if let Err(err) = self
            .handle
            .write()
            .take()
            .expect("No work thread")
            .join()
            .expect("Failed to join work thread")
        {
            eprintln!("Work thread exited with error: {}", err);
            return zx::Status::INTERNAL.into_raw();
        }
        zx::Status::OK.into_raw()
    }

    // Write address of active host into `addr_byte_buff`.
    //
    // Returns ZX_ERR_INTERNAL on error (check logs).
    async fn read_local_address(&self, addr_byte_buff: *mut u8) -> Result<(), anyhow::Error> {
        let addr_bytes_slice = unsafe { std::slice::from_raw_parts_mut(addr_byte_buff, 6) };
        let (sender, receiver) = oneshot::channel::<Result<[u8; 6], anyhow::Error>>();
        self.sender
            .write()
            .as_mut()
            .unwrap()
            .send(Request::ReadLocalAddress(sender))
            .await
            .expect("Failed to send");
        addr_bytes_slice.clone_from_slice(&receiver.await.expect("Failed to receive")?);
        Ok(())
    }
}

async fn get_active_host<'a>(
    host_cache: &'a mut Vec<HostInfo>,
    host_watcher_stream: &mut HangingGetStream<HostWatcherProxy, Vec<HostInfo>>,
) -> Result<&'a HostInfo, anyhow::Error> {
    if let Some(host_watcher_result) =
        host_watcher_stream.next().on_timeout(std::time::Duration::from_millis(100), || None).await
    {
        let Ok(new_host_list) = host_watcher_result else {
            return Err(anyhow!(
                "fuchsia.bluetooth.sys.HostWatcher error: {}",
                host_watcher_result.unwrap_err()
            ));
        };
        *host_cache = new_host_list
    }
    host_cache.first().ok_or_else(|| anyhow!("No hosts"))
}

static WORKER: LazyLock<WorkThread> = LazyLock::new(|| {
    let worker = WorkThread::new();
    worker.spawn();
    worker
});

/// Stop serving Rust affordances.
///
/// Returns ZX_STATUS_BAD_STATE if Rust affordances are not running.
/// Returns ZX_STATUS_INTERNAL if Rust affordances exited with an error (check logs).
#[no_mangle]
pub extern "C" fn stop_rust_affordances() -> zx_status_t {
    println!("Stopping Rust affordances");
    WORKER.join()
}

/// Populates `addr_byte_buff` with public address of active host.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
///
/// # Safety
///
/// The caller must ensure that `addr_byte_buff` points to a valid buffer of 6 bytes.
#[no_mangle]
pub extern "C" fn read_local_address(addr_byte_buff: *mut u8) -> zx_status_t {
    if let Err(err) = block_on(WORKER.read_local_address(addr_byte_buff)) {
        eprintln!("read_local_address encountered error: {}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}
