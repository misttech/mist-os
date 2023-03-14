// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::{endpoints::ControlHandle, AsyncChannel};
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_starnix_binder as fbinder;
use fidl_fuchsia_starnix_container as fstarcontainer;
use fuchsia_async::{self as fasync, DurationExt};
use futures::TryStreamExt;
use std::sync::Arc;

use crate::execution::Container;
use crate::fs::fuchsia::create_fuchsia_pipe;
use crate::logging::log_error;
use crate::types::{errno, OpenFlags};

use super::*;

/// Returns a DirectoryProxy to the root of the initial namespace of the container.
pub fn expose_root(container: &Arc<Container>) -> Result<fio::DirectoryProxy, Error> {
    let dir_fd = container.root_fs.root().open(&container.system_task, OpenFlags::RDWR, false)?;
    let client = container.kernel.file_server.serve(&dir_fd)?;
    Ok(fio::DirectoryProxy::new(AsyncChannel::from_channel(client.into_channel())?))
}

pub async fn serve_component_runner(
    mut request_stream: fcrunner::ComponentRunnerRequestStream,
    container: Arc<Container>,
) -> Result<(), Error> {
    while let Some(event) = request_stream.try_next().await? {
        match event {
            fcrunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                let container = container.clone();
                fasync::Task::local(async move {
                    if let Err(e) = start_component(start_info, controller, container).await {
                        log_error!("failed to start component: {:?}", e);
                    }
                })
                .detach();
            }
        }
    }
    Ok(())
}

pub async fn serve_container_controller(
    mut request_stream: fstarcontainer::ControllerRequestStream,
    container: Arc<Container>,
) -> Result<(), Error> {
    while let Some(event) = request_stream.try_next().await? {
        match event {
            fstarcontainer::ControllerRequest::VsockConnect { port, bridge_socket, .. } => {
                connect_to_vsock(port, bridge_socket, &container).await.unwrap_or_else(|e| {
                    log_error!("failed to connect to vsock {:?}", e);
                });
            }
        }
    }
    Ok(())
}

pub async fn serve_dev_binder(
    mut request_stream: fbinder::DevBinderRequestStream,
    container: Arc<Container>,
) -> Result<(), Error> {
    while let Some(event) = request_stream.try_next().await? {
        match event {
            fbinder::DevBinderRequest::Open { payload, control_handle } => {
                let result: Result<(), Error> = (|| {
                    let path = payload.path.ok_or_else(|| errno!(EINVAL))?;
                    let process_accessor =
                        payload.process_accessor.ok_or_else(|| errno!(EINVAL))?;
                    let binder = payload.binder.ok_or_else(|| errno!(EINVAL))?;
                    let node = container.system_task.lookup_path_from_root(&path)?;
                    let device_type = node.entry.node.info().rdev;
                    let binder_driver = container
                        .kernel
                        .binders
                        .read()
                        .get(&device_type)
                        .ok_or_else(|| errno!(ENOTSUP))?
                        .clone();
                    binder_driver
                        .open_external(&container.kernel, process_accessor, binder)
                        .detach();
                    Ok(())
                })();
                if result.is_err() {
                    control_handle.shutdown();
                }
            }
        }
    }
    Ok(())
}

async fn connect_to_vsock(
    port: u32,
    bridge_socket: fidl::Socket,
    container: &Arc<Container>,
) -> Result<(), Error> {
    let socket = loop {
        if let Ok(socket) = container.kernel.default_abstract_vsock_namespace.lookup(&port) {
            break socket;
        };
        fasync::Timer::new(fasync::Duration::from_millis(100).after_now()).await;
    };

    let pipe = create_fuchsia_pipe(
        &container.system_task,
        bridge_socket,
        OpenFlags::RDWR | OpenFlags::NONBLOCK,
    )?;
    socket.remote_connection(&container.system_task, pipe)?;

    Ok(())
}
