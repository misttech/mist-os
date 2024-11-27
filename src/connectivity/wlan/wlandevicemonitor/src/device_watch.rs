// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context as _;
use fuchsia_fs::directory::{WatchEvent, WatchMessage, Watcher};
use futures::future::TryFutureExt as _;
use futures::stream::{Stream, TryStreamExt as _};
use std::hash::{Hash as _, Hasher as _};
use tracing::error;
use {fidl_fuchsia_io as fio, fidl_fuchsia_wlan_device as fidl_wlan_dev};

pub struct NewPhyDevice {
    pub id: u16,
    pub proxy: fidl_wlan_dev::PhyProxy,
    pub device_path: String,
}

pub fn watch_phy_devices<'a>(
    device_directory: &'a str,
) -> Result<impl Stream<Item = Result<NewPhyDevice, anyhow::Error>> + 'a, anyhow::Error> {
    let directory = fuchsia_fs::directory::open_in_namespace(device_directory, fio::Flags::empty())
        .context("open directory")?;
    Ok(async move {
        let watcher = Watcher::new(&directory).await.context("create watcher")?;
        Ok(watcher.err_into().try_filter_map(move |WatchMessage { event, filename }| {
            futures::future::ready((|| {
                match event {
                    WatchEvent::ADD_FILE | WatchEvent::EXISTING => {}
                    _ => return Ok(None),
                };
                let filename = match filename.as_path().to_str() {
                    Some(filename) => filename,
                    None => return Ok(None),
                };
                if filename == "." {
                    return Ok(None);
                }
                let (proxy, server_end) = fidl::endpoints::create_proxy();
                let connector = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                    fidl_fuchsia_wlan_device::ConnectorMarker,
                >(&directory, filename)
                .context("connect to device")?;
                let () = match connector.connect(server_end) {
                    Ok(()) => (),
                    Err(e) => {
                        return match e {
                            fidl::Error::ClientChannelClosed { .. } => {
                                error!("Error opening '{}': {}", filename, e);
                                Ok(None)
                            }
                            e => Err(e.into()),
                        }
                    }
                };
                // TODO(https://fxbug.dev/42075598): remove the assumption that devices have numeric IDs.
                let mut s = std::collections::hash_map::DefaultHasher::new();
                let () = filename.hash(&mut s);
                let mut s: u64 = s.finish();
                let mut id: u16 = 0;
                while s != 0 {
                    id |= s as u16;
                    s = s >> 16;
                }
                Ok(Some(NewPhyDevice {
                    id,
                    proxy,
                    device_path: format!("{}/{}", device_directory, filename),
                }))
            })())
        }))
    }
    .try_flatten_stream())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_device::{ConnectorRequest, ConnectorRequestStream};
    use fuchsia_async as fasync;
    use futures::poll;
    use futures::stream::StreamExt as _;
    use futures::task::Poll;
    use std::pin::pin;
    use std::sync::Arc;
    use tracing::info;
    use vfs::directory::entry_container::Directory;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::pseudo_directory;
    use wlan_common::test_utils::ExpectWithin;

    #[fasync::run_singlethreaded(test)]
    async fn watch_single_phy() {
        let fake_dir = pseudo_directory! {
            "123" => serve_device_connector(),
        };

        serve_and_bind_vfs(fake_dir.clone(), "/test-dev");

        let mut phy_watcher =
            pin!(watch_phy_devices("/test-dev").expect("Failed to create phy_watcher"));

        phy_watcher
            .next()
            .expect_within(zx::MonotonicDuration::from_seconds(60), "phy_watcher did not respond")
            .await
            .expect("phy_watcher ended without yielding a phy")
            .expect("phy_watcher returned an error");

        if let Poll::Ready(..) = poll!(phy_watcher.next()) {
            panic!("phy_watcher found more than one phy");
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn watch_multiple_phys() {
        let fake_dir = pseudo_directory! {
            "123" => serve_device_connector(),
            "456" => serve_device_connector(),
        };

        serve_and_bind_vfs(fake_dir.clone(), "/test-dev");

        let mut phy_watcher =
            pin!(watch_phy_devices("/test-dev").expect("Failed to create phy_watcher"));

        for _ in 0..2 {
            phy_watcher
                .next()
                .expect_within(
                    zx::MonotonicDuration::from_seconds(60),
                    "phy_watcher did not respond",
                )
                .await
                .expect("phy_watcher ended without yielding a phy")
                .expect("phy_watcher returned an error");
        }

        if let Poll::Ready(..) = poll!(phy_watcher.next()) {
            panic!("phy_watcher found more than one phy");
        }
    }

    fn serve_and_bind_vfs(vfs_dir: Arc<dyn Directory>, path: &'static str) {
        let (client, server) = fidl::endpoints::create_endpoints();
        let scope = ExecutionScope::new();
        vfs_dir.open(
            scope.clone(),
            fidl_fuchsia_io::OpenFlags::RIGHT_READABLE | fidl_fuchsia_io::OpenFlags::DIRECTORY,
            Path::dot(),
            fidl::endpoints::ServerEnd::new(server.into_channel()),
        );

        let ns = fdio::Namespace::installed().expect("failed to get installed namespace");
        ns.bind(path, client).expect("Failed to bind dev in namespace");
    }

    fn serve_device_connector() -> Arc<vfs::service::Service> {
        vfs::service::host(move |mut stream: ConnectorRequestStream| async move {
            while let Some(request) = stream.next().await {
                match request {
                    Ok(ConnectorRequest::Connect { request: _request, .. }) => {
                        info!("device connector got connect request");
                    }
                    Err(e) => {
                        panic!("Unexpected error in device connector {:?}", e);
                    }
                }
            }
        })
    }
}
