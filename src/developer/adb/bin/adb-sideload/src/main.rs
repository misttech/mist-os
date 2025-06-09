// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use fidl_fuchsia_hardware_adb::{ProviderMarker, ProviderRequest, ProviderRequestStream};
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt as _, TryFutureExt as _};

mod sideload_file;

struct SideloadServer {}

impl SideloadServer {
    async fn connect_to_service(
        &self,
        socket: fidl::Socket,
        args: Option<String>,
    ) -> Result<(), zx::Status> {
        let [file_size, block_size] =
            *args.as_deref().unwrap_or_default().split(':').collect::<Vec<_>>()
        else {
            log::error!("Unexpected number of args: {args:?}");
            return Err(zx::Status::INVALID_ARGS);
        };
        let file_size = file_size.parse::<u64>().map_err(|e| {
            log::error!("Failed to parse file size from args {file_size:?}: {e}");
            zx::Status::INVALID_ARGS
        })?;
        let block_size = block_size.parse::<usize>().map_err(|e| {
            log::error!("Failed to parse block size from args {block_size:?}: {e}");
            zx::Status::INVALID_ARGS
        })?;
        if block_size == 0 {
            log::error!("Block size must be greater than 0");
            return Err(zx::Status::INVALID_ARGS);
        }
        log::info!("Starting sideload, file size: {file_size}, block size: {block_size}");
        fasync::Task::spawn(
            async move {
                let sideload_file = sideload_file::SideloadFile::new(
                    fasync::Socket::from_socket(socket),
                    file_size,
                    block_size,
                );
                // TODO(https://fxbug.dev/423639056): parse offset and length from zip
                let far_file = sideload_file.into_sub_file(89, file_size - 89);
                let archive = fuchsia_archive::AsyncUtf8Reader::new(far_file)
                    .await
                    .context("Failed to parse far")?;
                for entry in archive.list() {
                    log::info!("far entry: {entry:?}");
                }
                // TODO(https://fxbug.dev/419106573): serve files in far over http
                let far_file = archive.into_source();
                let () = far_file.close().await?;
                Ok(())
            }
            .unwrap_or_else(|e: Error| log::error!("Error in sideload server: {e:#}")),
        )
        .detach();
        Ok(())
    }
}

#[async_trait::async_trait]
impl fidl_server::AsyncRequestHandler<ProviderMarker> for SideloadServer {
    async fn handle_request(&self, request: ProviderRequest) -> Result<(), Error> {
        let ProviderRequest::ConnectToService { socket, args, responder } = request;
        log::info!("Connecting to service with args: {:?}", args);
        responder
            .send(self.connect_to_service(socket, args).await.map_err(|s| s.into_raw()))
            .context("Failed to send response")?;
        Ok(())
    }
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(|stream: ProviderRequestStream| {
        fidl_server::serve_async_detached(stream, SideloadServer {})
    });

    fs.take_and_serve_directory_handle()?;

    let () = fs.collect().await;

    Ok(())
}
