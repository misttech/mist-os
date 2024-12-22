// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use circuit::multi_stream::multi_stream_node_connection_to_async;
use fuchsia_fs::directory::{WatchEvent, Watcher};
use futures::{AsyncReadExt, StreamExt, TryStreamExt};
use overnet_core::Router;
use std::path::PathBuf;
use std::sync::Weak;
use {
    fidl_fuchsia_hardware_overnet as overnet_usb, fidl_fuchsia_io as fio, fuchsia_async as fasync,
};

static USB_DEVICE_PATH: &str = "/dev/class/overnet-usb";

pub async fn run_usb_links(router: Weak<Router>) -> Result<()> {
    let dir = fuchsia_fs::directory::open_in_namespace(USB_DEVICE_PATH, fio::PERM_READABLE)?;
    let mut watcher = { Watcher::new(&dir).await? };

    while let Some(msg) = watcher.try_next().await? {
        if msg.event == WatchEvent::ADD_FILE || msg.event == WatchEvent::EXISTING {
            let Some(filename) = msg.filename.as_os_str().to_str() else {
                log::warn!(filename:? = msg.filename; "skipping device with bad character in name");
                continue;
            };
            if filename == "." {
                continue;
            }
            let filename = filename.to_owned();
            let debug_path =
                PathBuf::from(USB_DEVICE_PATH).join(msg.filename.clone()).display().to_string();
            let dir = Clone::clone(&dir);
            let router = Clone::clone(&router);
            fasync::Task::spawn(async move {
                let debug_path = debug_path.as_str();
                loop {
                    match fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                        overnet_usb::DeviceMarker,
                    >(&dir, &filename)
                    {
                        Ok(proxy) => {
                            let (callback_client, callback) =
                                fidl::endpoints::create_endpoints::<overnet_usb::CallbackMarker>();
                            if let Err(error) = proxy.set_callback(callback_client).await {
                                log::warn!(
                                    error:? = error;
                                    "Could not communicate with USB driver"
                                );
                                break;
                            }

                            let callback = callback.into_stream();

                            let router = router.clone();
                            callback
                                .for_each_concurrent(None, move |req| {
                                    let router = router.clone();
                                    async move {
                                        let socket = match req {
                                            Ok(overnet_usb::CallbackRequest::NewLink {
                                                socket,
                                                responder,
                                            }) => {
                                                if let Err(error) = responder.send() {
                                                    log::warn!(
                                                        error:? = error;
                                                        "USB driver callback responder error"
                                                    );
                                                }
                                                socket
                                            }
                                            Err(error) => {
                                                log::warn!(
                                                    error:? = error;
                                                    "USB driver callback error"
                                                );
                                                return;
                                            }
                                        };

                                        let Some(router) = router.upgrade() else {
                                            return;
                                        };

                                        let socket = fasync::Socket::from_socket(socket);

                                        let (mut reader, mut writer) = socket.split();
                                        let (err_sender, mut err_receiver) =
                                            futures::channel::mpsc::unbounded();

                                        fasync::Task::spawn(async move {
                                            while let Some(error) = err_receiver.next().await {
                                                log::debug!(
                                                    error:? = error;
                                                    "Stream error for USB link"
                                                )
                                            }
                                        })
                                        .detach();

                                        if let Err(error) = multi_stream_node_connection_to_async(
                                            router.circuit_node(),
                                            &mut reader,
                                            &mut writer,
                                            true,
                                            circuit::Quality::USB,
                                            err_sender,
                                            format!("USB Overnet Device {debug_path}"),
                                        )
                                        .await
                                        {
                                            log::info!(
                                                device = debug_path,
                                                error:? = error;
                                                "USB link terminated",
                                            );
                                        }
                                    }
                                })
                                .await;
                        }
                        Err(error) => {
                            log::info!(
                                device = debug_path,
                                error:? = error;
                                "USB node could not be opened",
                            );
                            break;
                        }
                    }
                }
            })
            .detach();
        }
    }

    Err(format_err!("USB watcher hung up unexpectedly"))
}
