// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    execution::execute_task_with_prerun_result,
    fs::{devpts::create_main_and_replica, fuchsia::create_fuchsia_pipe},
    task::{CurrentTask, ExitStatus, Kernel},
    vfs::{
        buffers::{VecInputBuffer, VecOutputBuffer},
        file_server::serve_file_at,
        socket::VsockSocket,
        FdFlags, FileHandle,
    },
};
use anyhow::Error;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_element as felement;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_starnix_container as fstarcontainer;
use fuchsia_async::{
    DurationExt, {self as fasync},
};
use fuchsia_zircon as zx;
use futures::{channel::oneshot, AsyncReadExt, AsyncWriteExt, TryStreamExt};
use starnix_logging::log_error;
use starnix_uapi::{open_flags::OpenFlags, uapi};
use std::{ffi::CString, sync::Arc};

use super::start_component;

pub fn expose_root(
    system_task: &CurrentTask,
    server_end: ServerEnd<fio::DirectoryMarker>,
) -> Result<(), Error> {
    let root_file = system_task.open_file(b"/", OpenFlags::RDONLY)?;
    serve_file_at(server_end.into_channel().into(), system_task, &root_file)?;
    Ok(())
}

pub async fn serve_component_runner(
    request_stream: frunner::ComponentRunnerRequestStream,
    system_task: &CurrentTask,
) -> Result<(), Error> {
    request_stream
        .try_for_each_concurrent(None, |event| async {
            match event {
                frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                    if let Err(e) = start_component(start_info, controller, system_task).await {
                        log_error!("failed to start component: {:?}", e);
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(Error::from)
}

fn to_winsize(window_size: Option<fstarcontainer::ConsoleWindowSize>) -> uapi::winsize {
    window_size
        .map(|window_size| uapi::winsize {
            ws_row: window_size.rows,
            ws_col: window_size.cols,
            ws_xpixel: window_size.x_pixels,
            ws_ypixel: window_size.y_pixels,
        })
        .unwrap_or(uapi::winsize::default())
}

async fn spawn_console(
    kernel: &Arc<Kernel>,
    payload: fstarcontainer::ControllerSpawnConsoleRequest,
) -> Result<Result<u8, i32>, Error> {
    if let (Some(console_in), Some(console_out), Some(binary_path)) =
        (payload.console_in, payload.console_out, payload.binary_path)
    {
        let binary_path = CString::new(binary_path)?;
        let argv = payload
            .argv
            .unwrap_or(vec![])
            .into_iter()
            .map(CString::new)
            .collect::<Result<Vec<_>, _>>()?;
        let environ = payload
            .environ
            .unwrap_or(vec![])
            .into_iter()
            .map(CString::new)
            .collect::<Result<Vec<_>, _>>()?;
        let window_size = to_winsize(payload.window_size);
        let current_task = CurrentTask::create_init_child_process(kernel, &binary_path)?;
        let (sender, receiver) = oneshot::channel::<Result<u8, i32>>();
        let pty = execute_task_with_prerun_result(
            current_task,
            move |_, current_task| {
                let executable =
                    current_task.open_file(binary_path.as_bytes(), OpenFlags::RDONLY)?;
                current_task.exec(executable, binary_path, argv, environ)?;
                let (pty, pts) = create_main_and_replica(&current_task, window_size)?;
                let fd_flags = FdFlags::empty();
                assert_eq!(0, current_task.add_file(pts.clone(), fd_flags)?.raw());
                assert_eq!(1, current_task.add_file(pts.clone(), fd_flags)?.raw());
                assert_eq!(2, current_task.add_file(pts, fd_flags)?.raw());
                Ok(pty)
            },
            move |result| {
                let _ = match result {
                    Ok(ExitStatus::Exit(exit_code)) => sender.send(Ok(exit_code)),
                    _ => sender.send(Err(zx::Status::CANCELED.into_raw())),
                };
            },
        )?;
        let _ = forward_to_pty(kernel, console_in, console_out, pty).map_err(|e| {
            log_error!("failed to forward to terminal {:?}", e);
        });

        Ok(receiver.await?)
    } else {
        Ok(Err(zx::Status::INVALID_ARGS.into_raw()))
    }
}

pub async fn serve_container_controller(
    request_stream: fstarcontainer::ControllerRequestStream,
    system_task: &CurrentTask,
) -> Result<(), Error> {
    request_stream
        .map_err(Error::from)
        .try_for_each_concurrent(None, |event| async {
            match event {
                fstarcontainer::ControllerRequest::VsockConnect { port, bridge_socket, .. } => {
                    connect_to_vsock(port, bridge_socket, system_task).await.unwrap_or_else(|e| {
                        log_error!("failed to connect to vsock {:?}", e);
                    });
                }
                fstarcontainer::ControllerRequest::SpawnConsole { payload, responder } => {
                    responder.send(spawn_console(system_task.kernel(), payload).await?)?;
                }
                fstarcontainer::ControllerRequest::_UnknownMethod { .. } => (),
            }
            Ok(())
        })
        .await
}

async fn connect_to_vsock(
    port: u32,
    bridge_socket: fidl::Socket,
    system_task: &CurrentTask,
) -> Result<(), Error> {
    let socket = loop {
        if let Ok(socket) = system_task.kernel().default_abstract_vsock_namespace.lookup(&port) {
            break socket;
        };
        fasync::Timer::new(fasync::Duration::from_millis(100).after_now()).await;
    };

    let pipe =
        create_fuchsia_pipe(system_task, bridge_socket, OpenFlags::RDWR | OpenFlags::NONBLOCK)?;
    socket.downcast_socket::<VsockSocket>().unwrap().remote_connection(
        &socket,
        system_task,
        pipe,
    )?;

    Ok(())
}

fn forward_to_pty(
    kernel: &Kernel,
    console_in: fidl::Socket,
    console_out: fidl::Socket,
    pty: FileHandle,
) -> Result<(), Error> {
    // Matches fuchsia.io.Transfer capacity, somewhat arbitrarily.
    const BUFFER_CAPACITY: usize = 8192;

    let mut rx = fuchsia_async::Socket::from_socket(console_in)?;
    let mut tx = fuchsia_async::Socket::from_socket(console_out)?;
    let pty_sink = pty.clone();
    kernel.kthreads.spawn({
        move |_, current_task| {
            let _result: Result<(), Error> =
                fasync::LocalExecutor::new().run_singlethreaded(async {
                    let mut buffer = vec![0u8; BUFFER_CAPACITY];
                    loop {
                        let bytes = rx.read(&mut buffer[..]).await?;
                        if bytes == 0 {
                            return Ok(());
                        }
                        pty_sink.write(current_task, &mut VecInputBuffer::new(&buffer[..bytes]))?;
                    }
                });
        }
    });

    let pty_source = pty;
    kernel.kthreads.spawn({
        move |_, current_task| {
            let _result: Result<(), Error> =
                fasync::LocalExecutor::new().run_singlethreaded(async {
                    let mut buffer = VecOutputBuffer::new(BUFFER_CAPACITY);
                    loop {
                        buffer.reset();
                        let bytes = pty_source.read(current_task, &mut buffer)?;
                        if bytes == 0 {
                            return Ok(());
                        }
                        tx.write_all(buffer.data()).await?;
                    }
                });
        }
    });

    Ok(())
}

pub async fn serve_graphical_presenter(
    request_stream: felement::GraphicalPresenterRequestStream,
    kernel: &Kernel,
) -> Result<(), Error> {
    request_stream
        .try_for_each_concurrent(None, |event| async {
            match event {
                felement::GraphicalPresenterRequest::PresentView {
                    view_spec,
                    annotation_controller: _,
                    view_controller_request: _,
                    responder,
                } => match view_spec.viewport_creation_token {
                    Some(token) => {
                        kernel.framebuffer.present_view(token);
                        let _ = responder.send(Ok(()));
                    }
                    None => {
                        let _ = responder.send(Err(felement::PresentViewError::InvalidArgs));
                    }
                },
            }
            Ok(())
        })
        .await
        .map_err(Error::from)
}
